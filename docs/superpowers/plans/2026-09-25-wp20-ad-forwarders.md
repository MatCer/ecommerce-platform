# WP20 Ad-platform forwarders: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** forward authoritative purchases (and refunds where a platform takes them) and
consented behavioral events to Meta Conversions API, GA4 Measurement Protocol, Google Ads
(Data Manager API) and Seznam SEM server-to-server, only while the subject's `ads` consent
holds at send time (A20), with hashed identifiers per vendor spec, retries, a delivery log,
per-tenant encrypted credentials, admin settings and local mocks.

**Architecture:**
- `commerce::adtracking` (new):
  - `normalize`: email/phone normalization + SHA-256 per vendor (golden vectors from docs).
  - config: one row per (tenant, platform) in `ad_platforms`: enabled, paused, test mode,
    market ids, non-secret settings (JSON, validated per platform), credentials sealed with
    `SecretBox` (AAD `adplatform:<tenant>:<platform>`), a hint only is ever returned.
  - capture: `capture_events` (beacon + server-side cart steps) and `capture_purchase`
    (inside the order placement transaction) insert one `ad_deliveries` row per enabled
    platform that takes the event, with a stable `event_id` (dedupe key sent to vendors) and
    enqueue a `adtracking.deliver` job. Only if `ads` is granted at capture time; nothing
    identifying is stored: the row keeps the consent subject (pseudonymous), the order id,
    minimized props (SKUs, quantity, the catalog page) and the user agent (needed by
    Meta for website events, dropped when the delivery finishes).
  - deliver: re-checks consent (anon subject grants `ads`, the customer has not refused it),
    builds the vendor payload at send time (email/phone read from the order and hashed in
    memory, never stored), sends through `SafeClient` to fixed vendor hosts, records
    status/response code/short error (no payload, no PII). Retries are the queue's (A14:
    lease, backoff + jitter, `max_attempts`); 4xx other than 408/429 is permanent; the last
    failed attempt marks the delivery `dead`.
  - withdrawal: `consent::insert` of `ads=false` cancels the subject's pending deliveries
    (anon subject, or customer id for purchases) in the same transaction.
  - refunds: outbox `order.refunded` → `adtracking.refund` job → GA4 `refund` for orders
    whose purchase was captured (same subject, consent re-checked).
- Vendor endpoints are platform constants; `AD_PLATFORMS_BASE_URL` (honored only with
  `APP_ENV=dev`) points all of them at `apps/mocks` (`/ads/<platform>/...`).
- Per-platform rate limiting in the worker: `governor` keyed limiter per (tenant, platform).
- Admin API `/admin/v1/ad-platforms` (list, patch, test connection, delivery log), owner/admin
  only, credential changes need fresh auth (A9). Admin page "Ad tracking".
- Edge forwards `X-Client-User-Agent` on `/_p/e` and checkout calls; the SDK beacon also
  sends when only `ads` is granted (the server stores analytics events only with `analytics`).

## Global constraints
- A20: consent from `consent_records` at execution time; beacon purposes ignored.
- A21: outbound only via `SafeClient`; vendor URLs are constants, never merchant input.
- §14: credentials encrypted at rest (`SECRETS_KEY`); no raw email/IP in stored payloads or
  logs; delivery log without payloads.
- Tenant tables: RLS + FORCE + cross-tenant test. Migration after 20261006000000.
- Rust 1.98.1, clippy -D warnings, no unwrap outside tests, sqlx offline data committed.

## Review focus
- Consent gating at capture AND execution; withdrawal race is bounded to an in-flight send.
- Hash normalization correctness (Meta phone without `+`, Google/Sklik E.164 with `+`,
  Gmail dot/plus rule only for Google).
- No PII in `ad_deliveries`, logs, errors, or API responses.

## Tasks
1. Normalization + hashing (`adtracking/normalize.rs`) with golden-vector unit tests.
2. Migration `20261007000000_ad_tracking.sql` (`ad_platforms`, `ad_deliveries`, RLS).
3. Config CRUD + credential sealing + validation; cross-tenant test.
4. Capture (events, purchase), consent gating, withdrawal cancel; integration tests.
5. Vendor payload builders (mapping unit tests) + deliver with retries/dead + rate limit;
   integration tests against an in-process receiver.
6. Worker handlers (`adtracking.deliver`, `adtracking.refund`), outbox subscriber.
7. API routes + storefront wiring (beacon, cart track, place order), OpenAPI + clients.
8. Edge UA header + SDK beacon purpose change.
9. Mocks (`apps/mocks/src/ads.ts`) + tests.
10. Admin page + i18n; e2e `e2e/admin/ad-tracking.spec.ts`.
11. Full verification (fresh stack + seed, lint, tests, smoke images), Astra review, PR.
