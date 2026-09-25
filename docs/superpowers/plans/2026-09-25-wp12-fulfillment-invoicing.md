# WP12 Fulfillment, invoicing, returns: implementation plan

**Goal:** full admin order management, Packeta/PPL shipping (labels, tracking, per-tenant
credentials), CZ/SK invoicing with Typst PDFs and ČNB rates, refunds with credit notes, the A19
withdrawal flow, cancellation emails. Spec §7.4, §10.5-10.7, §17 WP12, amendments A13, A15,
A16, A17, A19, A21; follow-ups assigned to WP12 in `docs/follow-ups.md`.

**Architecture:** business logic in `crates/commerce` (new modules `fulfillment`, `carriers`,
`invoicing`, `refunds`, `withdrawals`, `documents`), Admin/Storefront API handlers in
`crates/api`, jobs + cron in `crates/worker`, local provider stand-ins in `apps/mocks`, UI in
`apps/admin` and `apps/checkout` (+ edge allowlists). PDFs are rendered by the worker with the
Typst CLI (0.15.1, in the runtime image) into the private bucket (A21) and served through 5-min
presigned URLs after authorization.

## Global constraints

- Every status change goes through the WP9 machines (`orders::status`). Stock: commit on
  shipment, restock on merchant-confirmed receipt of returned goods (A13, idempotent movement
  identities `order:<id>` / `return_line:<id>`).
- M1: no financial edits after payment. Non-financial edits only: shipping address before a
  label exists, staff notes.
- Invoices are immutable: the runtime role may only UPDATE `pdf_key`/`pdf_rendered_at`
  (column grants). Numbering `{prefix}{YYYY}{seq:05}`, gapless, assigned under the series row
  lock in the issuing transaction.
- A17 scenarios: prepaid → invoice on the payment date (DUZP = payment date, also the final
  invoice); COD → invoice on dispatch (DUZP = dispatch date); non-VAT payer → no VAT, no recap;
  returns/cancellations after invoicing → credit note referencing the original allocations.
  ČNB rate = the latest fixing on or before DUZP (ČNB's own weekend/holiday fallback); a rate
  that is not published yet → the issue job retries and the order shows a warning.
- A15 refunds: per line, `k` of `q` units refund `floor(total·k/q)` (VAT `round_half_up`),
  the last units of a line get the residual, so refunding everything returns exactly the
  persisted allocation. Shipping/payment fee charges are refunded whole (all VAT portions).
- A19: public `checkout.<host>/withdraw`; order number + email → emailed single-use link
  (24 h, hashed at rest, no account enumeration: always 202) → line selection → explicit
  confirm → receipt email with the full declaration; the declaration text is stored immutably.
- Carrier secrets sealed with `SecretBox` (AAD `carrier:<tenant>:<carrier>`); never returned.
- Tenant tables: `tenant_id`, RLS + FORCE, composite FKs, cross-tenant test.
- "Templates require accountant approval before real use" (docs + admin invoice section).

## Review focus

Gapless numbering under concurrency, immutability grants, A15 residual arithmetic (golden
tests), A17 scenario selection and ČNB fallback, idempotency of shipment/stock/invoice/email
side effects, withdrawal token security and enumeration, presigned URL authorization.

## Data model (migration `20261011000000_fulfillment_invoicing.sql`)

- `carrier_accounts(tenant_id, carrier packeta|ppl, credentials bytea sealed JSON,
  public_key text null (Packeta widget key), sender_label, updated_at)` PK (tenant, carrier).
- `shipments(id, tenant_id, order_id, carrier, status creating|label_created|shipped|delivered|
  returned|cancelled, carrier_ref, tracking_number, tracking_url, label_key, carrier_status,
  tracked_at, shipped_at, delivered_at, created_by, created_at)`; one non-cancelled shipment
  per order.
- `platform.exchange_rates(currency, fixing_date, amount, rate_milli, fetched_at)` PK
  (currency, fixing_date): CZK per `amount` units × 1000 (ČNB publishes 3 decimals).
- `invoice_series(id, tenant_id, kind invoice|credit_note, year, prefix, next_number)`.
- `invoices(id, tenant_id, series_id, kind, number, order_id, original_id, issued_on,
  taxable_supply_date, due_on, currency, vat_payer, exchange_rate jsonb null, document jsonb,
  pdf_key, pdf_rendered_at, created_by, created_at)`; unique number per tenant; one
  invoice per order.
- `refunds` += `credit_note_id`, `iban`, `lines jsonb` (what the refund covered).
- `withdrawals(id, tenant_id, order_id, customer_id, email, status open|received|refunded,
  declaration text, iban, delivered_at, declared_at, goods_received_at, return_proof_at,
  refund_due_at, refunded_at, refund_id, locale)`, `withdrawal_tokens(token_hash, order_id,
  expires_at, used_at)`, `return_lines(id, withdrawal_id, order_line_id, quantity, status)`.
- `documents(id, tenant_id, kind packing_slips|labels, order_ids uuid[], locale, status
  pending|ready|failed, object_key, error, created_by, created_at)`.
- `email_messages.attachments jsonb` `[{key, filename, content_type}]` (private bucket keys).

## Document data (Typst render model, all strings preformatted by Rust)

`invoice.typ` reads `data.json`:

```json
{ "locale": "cs", "kind": "invoice", "number": "FV202600001",
  "issued_on": "25. 9. 2026", "taxable_supply_date": "25. 9. 2026", "due_on": "25. 9. 2026",
  "vat_payer": true, "currency": "CZK", "order_number": "100001", "variable_symbol": "100001",
  "payment_method": "bank_transfer", "paid": true,
  "bank_account": { "iban": "CZ65…", "bic": "GIBACZPX", "name": "Demo s.r.o." },
  "supplier": { "name": "Demo s.r.o.", "address": ["Dlouhá 1", "110 00 Praha", "CZ"],
                "company_id": "12345678", "vat_id": "CZ12345678", "sk_ic_dph": null,
                "registry": "…", "email": "…", "phone": "…" },
  "customer": { "name": "Jana Nováková", "address": ["…"], "email": "…" },
  "original": null, "reason": null,
  "lines": [ { "name": "…", "sku": "…", "quantity": "1", "unit_price": "129,00 Kč",
               "vat_rate": "21 %", "net": "106,61 Kč", "vat": "22,39 Kč", "total": "129,00 Kč" } ],
  "vat_recap": [ { "rate": "21 %", "net": "…", "vat": "…", "gross": "…" } ],
  "czk_recap": null,
  "totals": { "net": "…", "vat": "…", "gross": "…" } }
```

`czk_recap` (EUR document of a CZ VAT payer): `{ "rate": "24,305", "amount": "1",
"currency": "EUR", "rate_date": "24. 9. 2026", "rows": [recap rows in CZK], "vat": "…" }`.
Credit notes: `kind = credit_note`, negative amounts, `original = {number, issued_on}`.
`packing_slip.typ`: `{ "locale", "shop": "…", "orders": [{ "number", "placed_on", "shipping",
"pickup_point": str|null, "address": [..], "email", "phone", "notes", "lines": [{ "sku",
"name", "options", "quantity" }] }] }`. `labels.typ`: `{ "files": ["label-0.pdf", …] }`, one
A6 page per label.

## Tasks

1. **Auth rate limit per client IP** (done, separate commit): Caddy sets `X-Real-IP`, Better
   Auth trusts only that header; test.
2. **Migration + sqlx data**: tables above, grants, RLS, SECURITY DEFINER scans
   (`platform.trackable_shipments`), cross-tenant test.
3. **Carriers** (`commerce::carriers`): `packeta` (REST XML: `createPacket`,
   `packetLabelPdf` "A6 on A6", `packetStatus`; pickup-point validation), `ppl` (CPL API:
   OAuth client credentials token cache, `POST /shipment/batch` + `GET /shipment/batch/{id}`
   + label URL, `GET /shipment?ShipmentNumbers=` tracking), per-tenant credentials
   (admin + fresh auth), the widget key per tenant. Mocks in `apps/mocks` with the same
   shapes (+ control endpoints for tests: advance a packet's status).
4. **Fulfillment** (`commerce::fulfillment`): create label (outside tx: carrier call; row
   `creating` first), cancel label, mark shipped (fulfillment + order machines, stock commit,
   `order.shipped`, shipped email with tracking link), delivered (manual or tracking poll;
   COD → `cod::deliver`; delivered email), returned to sender (restock + credit note),
   start processing, cancel (release stock, refund if paid, credit note if invoiced,
   cancellation email), notes, shipping address edit before a label, allowed actions.
5. **Invoicing** (`commerce::invoicing`): ČNB parser + client + rate lookup, series/numbering,
   invoice builder (pure, golden tests on the data model: prepaid CZK, COD, EUR with CZK
   recap, SK with IČ DPH, non-VAT payer, credit note partial), issue job (subscriber of
   `order.paid` / `order.shipped`), render job (Typst), email with the PDF attached,
   presigned downloads (admin + customer order page/account).
6. **Refunds** (`commerce::refunds`): allocation reversal, credit note, provider refund via
   `payments` (Stripe with fee refund; bank/COD recorded with the IBAN), exception refunds
   (late/duplicate payments → refund + resolve), refund email, `order.refunded`.
7. **Withdrawals** (`commerce::withdrawals`): request link, form, declare (receipt email with
   the declaration), account variant, admin queue (deadlines), receive goods → restock,
   return proof, refund (lines + shipping and payment fee when everything is withdrawn).
8. **Documents**: packing slips and label sheets (bulk) rendered by the worker.
9. **Emails**: shipped, delivered, cancelled (incl. expired unpaid), refunded, invoice/credit
   note (attachment), withdrawal link, withdrawal receipt; attachments in the mail pipeline.
10. **API + OpenAPI + clients**; edge allowlists for `/_p/withdraw/*`.
11. **Admin UI**: orders list (filters, bulk labels/packing slips), detail (actions, timeline,
    shipments, invoices, refunds dialog, withdrawals), withdrawals queue, carrier settings.
12. **Checkout UI**: `/withdraw` flow, account order withdrawal + invoice downloads.
13. **Images**: Typst in the runtime image (checksum-pinned), smoke test, CI installs Typst.
14. **Verification**: fresh stack + seed + theme-build; e2e `e2e/admin/fulfillment.spec.ts`,
    `e2e/checkout/withdrawal.spec.ts`; `make lint test`, `make perf`,
    `scripts/smoke-images.sh`; Astra review; PR.
