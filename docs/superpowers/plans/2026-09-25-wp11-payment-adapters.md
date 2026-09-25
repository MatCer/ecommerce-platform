# WP11 Payment adapters: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** real payment methods on top of WP10's attempts: Stripe Connect direct charges with
signed, persisted, asynchronously processed webhooks (A10, A11), bank transfer with SPAYD / PAY
by square 1.2.0 QR codes, variable symbols and statement matching (A25), cash on delivery with
tender, collector, cash rounding at collection and remittance (A16), payment reminders and
late-payment exceptions, and the admin screens to run it.

**Architecture:** business logic in `commerce::payments::{mod, stripe, bank, qr, statements,
cod}`; AES-256-GCM for stored provider credentials in `platform::crypto` (aws-lc-rs, already in
the tree). HTTP: `api::admin_payments` (settings, statements, exceptions, COD), storefront order
routes gain the Stripe simulator, `api::webhooks` gains `POST /webhooks/stripe`. The worker
processes provider events, sends reminders and polls the Fio API. The checkout's order page
shows bank instructions + QR and the Stripe step (real Payment Element when a real key is
configured, a labelled simulator otherwise). `apps/mocks` serves a Fio API stand-in.

## Global constraints

- Every new tenant table: `tenant_id`, RLS + `FORCE`, composite FKs, a cross-tenant test.
  `platform.provider_events` is a platform table (the tenant is unknown until the event is
  matched); the account → tenant lookup is a SECURITY DEFINER function.
- A11: the raw event is stored (unique `(provider, event_id)`) before the 200; processing is
  a job. Only `payment_intent.succeeded` confirms. An event must match the tenant's connected
  account, the account's livemode, the object id stored on the attempt, currency and amount;
  mismatches are recorded as rejected, never applied. Out-of-order events never regress
  `paid` (a failure after a success is ignored). Refunds carry an idempotency key and
  `refund_application_fee=true`. `account.updated` capability loss hides Stripe at checkout.
- Stripe local mode: no `STRIPE_SECRET_KEY` + `STRIPE_MOCK_URL` → API calls go to stripe-mock
  and the order page offers a "Stripe test simulator" that makes the API sign a Stripe-shaped
  event with the (test) webhook secret and feed it to the same receive function the webhook
  route uses. Refused with `APP_ENV=prod`.
- A25: PAY by square v1.2.0 (field order, CRC32, raw LZMA lc=3 lp=0 pb=2 dict 2^17, 2-byte
  header, 2-byte length, base32hex) byte-identical to golden vectors generated once with the
  `bysquare` npm package (`fixtures/qr/`). SPAYD escapes `*` as `%2A`, `ACC:IBAN+BIC`. The
  variable symbol is the order number (≤ 10 digits), unique per tenant + receiving account
  (unique index). Statement lines are unique per account by bank transaction id (duplicates
  ignored). Matching is scoped to tenant + account: VS + amount + currency; partial, over,
  unmatched, already-paid and currency mismatches go to the exceptions queue.
- A16: rounding only for tender = cash (CZK 1 Kč, SK EUR €0.05), applied at collection as a
  persisted `rounding` charge (outside the VAT base unless the tax profile says otherwise);
  `delivered → collected → remitted` via the WP9 COD machine, every manual action audited.
- A21: the Fio token is encrypted at rest (AES-256-GCM, versioned, AAD = tenant + account id)
  with `PAYMENTS_SECRET_KEY`; never returned by the API.
- New dependencies: `qrcode` (QR matrix + SVG), `liblzma` (raw LZMA1 encoder: its output is
  byte-identical to the LZMA SDK encoder `bysquare` uses; a pure-Rust encoder is not).
- No `unwrap()` outside tests, sqlx macros with `.sqlx/` data, TS strict without `any`.

## Review focus

- Webhook: signature over the raw body before parsing, timestamp tolerance, dedupe before
  enqueue, every A11 match check, idempotent processing under concurrent delivery.
- Bank matching: tenant/account scoping, duplicate imports, late payment after expiry (no
  restock), already-paid transfers, manual resolution audit.
- COD rounding: totals, VAT recap and the charge row stay consistent (A15).
- Payment reminders and expiry: no reminder after the window closed, one reminder per slot.
- Secrets: webhook secret, Stripe key and Fio tokens never logged or returned.

## Tasks

1. **Migration** `20261001000000_payment_adapters.sql`: `stripe_accounts`,
   `platform.provider_events`, `bank_accounts`, `bank_transactions`, `refunds`, attempt columns
   (bank account, VS, instructions, tender, collector, COD status + timestamps, reminders),
   order exception resolution, scan functions (reminders, Fio accounts, account → tenant).
2. **QR** (`payments::qr`, pure): SPAYD, PAY by square 1.2.0, deburr, base32hex, SVG. Golden
   vectors script `fixtures/qr/generate.mjs` + committed JSON; tests byte-for-byte.
3. **Statements** (`payments::statements`, pure): camt.053, Fio CSV, GPC (ABO), Fio API JSON →
   `StatementLine`; fixtures under `fixtures/bank/`.
4. **Crypto + config**: `platform::crypto::SecretBox`; `StripeConfig`, `PAYMENTS_SECRET_KEY`,
   `FIO_API_URL`.
5. **Bank transfer**: account per market (upsert, Fio token), availability, VS + instructions
   at placement, import + matching, exceptions + resolution, reminders (day 3/6), Fio polling.
6. **Stripe**: client (stripe-mock / real), PaymentIntent init (direct charge, fee,
   idempotency key), webhook verify + persist + enqueue, event processing, onboarding + status,
   refund, simulator events.
7. **COD**: deliver / collect (tender, collector, rounding charge) / remit, carrier CSV report.
8. **API + OpenAPI**: admin payments routes, storefront simulator route, Stripe webhook.
9. **Worker**: `payments.provider_event`, `payments.remind`, `payments.fio_poll`.
10. **Edge**: order ops allowlist (simulate), Stripe origins in the CSP only on checkout pages.
11. **Mocks**: Fio API stand-in.
12. **Checkout UI**: bank instructions + QR on `/o/<token>`, Stripe step (simulator / Payment
    Element loaded only for Stripe).
13. **Emails**: confirmation with bank instructions + QR; `payment_reminder`.
14. **Admin UI**: bank account + Stripe account on the payments screen, bank transactions +
    statement upload, exceptions queue, COD actions on the order detail.
15. **Seed + compose**: demo bank accounts, bank transfer + Stripe enabled; stripe-mock wiring,
    secrets defaults; CI stripe-mock service.
16. **E2E** `e2e/checkout/payments.spec.ts`; **verification**; Astra review; PR.
