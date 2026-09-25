# WP13b Import, export, privacy: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** merchants bring customers, historical orders and newsletter subscribers over from
their old shop as CSV (column mapping, dry run with a row-level report, idempotent apply in a
background job, no side effects), take all their data out (JSONL per table + assets manifest,
zipped, private bucket, 5-minute download link), and answer GDPR access and erasure requests
(anonymize customer + orders, keep invoices, audit-logged). Spec §10.8, §11.5, §14, A20, A21,
A28, A29.

## Architecture

- Migration `20261017000000_data_portability.sql` (tenant tables RLS + FORCE + grants):
  - `data_imports(kind customers|orders|subscribers, status pending|analyzing|analyzed|applying|
    applied|failed, market_id, mapping jsonb, object_key, report jsonb, progress jsonb, error,
    created_by, timestamps)`.
  - `archived_orders(number text, placed_at, email, customer_id null, name, phone, currency,
    total_minor, status_label, address jsonb, lines jsonb, import_id)`, unique
    `(tenant_id, number)`: historical orders live **outside** `orders`, so no order workflow,
    payment, stock, invoice, email, webhook, analytics or flow can ever pick them up (A28).
  - `data_exports(status pending|running|ready|failed, object_key, size_bytes, error,
    created_by, timestamps)`.
  - `subscribers.consent_evidence jsonb` (imported evidence as given: source label, timestamp,
    IP, text version, import id).
  - `platform.erase_consent_subject(type, id)` SECURITY DEFINER: consent records stay
    append-only for the application, erasure replaces the subject id with a random one.
- `commerce::portability`:
  - `imports` (run lifecycle mirroring feed imports: presigned PUT of the CSV → analyze job
    snapshots it to a server-only key, parses (`csv` crate, `,`/`;` sniffed, UTF-8, BOM
    stripped), maps columns, validates every row, stores the report (counts, first 200 row
    errors, 10-row preview) → apply job upserts valid rows in batches; the CSV objects are
    deleted after apply). Limits: 20 MB, 50 000 data rows, 1 000 chars per cell.
  - `imports::customers` (upsert by email; optional default address; no password, no mail),
    `imports::orders` (one row per line, grouped by `order_number`; upsert by number; linked to
    the customer with that email), `imports::subscribers` (evidence = `consent_at` +
    `consent_source`; with evidence → consent record `email_marketing` granted at the evidence
    time, source `import`, subscribed only if that is the address's latest decision and it is
    not suppressed; without evidence → `pending` without a confirmation, never marketable;
    unsubscribed/bounced/complained stay as they are).
  - `export` (job `data.export`: every tenant table the runtime role can read, discovered from
    the catalog, as `<table>.jsonl` with `bytea` columns, `password_hash` and mail bodies
    removed and capability/session tables skipped; `assets-manifest.jsonl`; zip streamed to
    the private bucket; download via `documents::download_url`).
- `commerce::privacy`: `access(email)` (one JSON document with everything linked to the
  address or its customer account) and `erase(email)` (refused with `409 erasure_blocked`
  while an order is open, a withdrawal unrefunded or a refund pending; deletes the customer
  account, sessions, addresses, subscriber, analytics/ad/affinity rows, suppressions and
  rate-limit rows; anonymizes orders, order addresses, archived orders, carts, withdrawals,
  mail log and PII-bearing order events; pseudonymizes consent records; keeps invoices,
  credit notes and bank transactions (tax/accounting law); deletes label PDFs after commit;
  records `privacy.erased` with counts only).
- API (`admin_portability.rs`, Admin role; export download, access and erasure also need a
  fresh login, A9): `/admin/v1/data-imports[/{id}[/analyze|/apply]]`,
  `/admin/v1/archived-orders`, `/admin/v1/data-exports[/{id}[/download]]`,
  `/admin/v1/privacy/access`, `/admin/v1/privacy/erasure`.
- Worker: `data.import`, `data.export` handlers.
- Admin UI (`/data`): CSV import (kind, file, column mapping from the file's header row read in
  the browser, report, apply), archived orders list, tenant export list + download, privacy
  requests (access download, erasure with typed confirmation).

## Global Constraints

- Business logic only in `crates/commerce`; SQL via `query!` + committed `.sqlx`.
- Every new tenant table: RLS + FORCE + cross-tenant test.
- Imports never enqueue mail, outbox events, stock movements, invoices or payments; a test
  asserts `email_messages`, `queue.outbox`, `stock_movements`, `invoices`, `orders` stay
  untouched.
- No PII in audit diffs or logs.

## Review Focus

- Erasure completeness vs. retention (what stays and why), blockers.
- Export: nothing secret leaves (hashes, ciphertexts, tokens, password hashes, live sign-in
  links); tenant isolation of catalog-discovered tables (RLS applies, runtime role only).
- Import idempotency and the "no side effects" guarantee; consent evidence semantics.

## Tasks

1. Migration + `.sqlx` + cross-tenant isolation test for the new tables.
2. CSV reading + mapping + limits (unit tests: delimiter, BOM, required columns, limits).
3. Customers import (dry run report, apply, re-import idempotent, no side effects).
4. Orders import (grouping, line validation, archived, customer link, re-import).
5. Subscribers import (evidence rules, marketability, suppression, unsubscribed kept).
6. Import run lifecycle + worker job + API routes.
7. Tenant export job + API (zip content test: tables present, secrets absent, isolation).
8. Privacy access + erasure (+ blockers, retention of invoices, audit) + API.
9. Admin UI + i18n (cs/sk/en) + unit tests for the pure helpers (mapping guess).
10. OpenAPI + clients, lint/test, real-stack verification, Astra review, PR.
