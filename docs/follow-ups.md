# Follow-ups ledger (gaps reported by merged WPs, with owning WP)

| From | Gap | Owner |
|---|---|---|
| WP0 | CI doesn't smoke-test the auth image (needs Postgres) | WP15 |
| WP1 | The smoke scripts are manual; move them into the e2e suite | WP15 |
| WP2 | Artifact GC (private bucket `artifacts/` + edge cache volume); WP6 keeps everything | WP23 |
| WP2 | Page-model gaps left after WP8: size chart, dispatch cutoff + holidays; font library (recently viewed has live prices since WP17) | WP15 |
| WP4 | A sale change recomputes every priced variant of the tenant; narrow it for very large catalogs | later (perf) |
| WP4 | Price history returns the full timeline per variant (no pagination) | later |
| WP5 | No invitation-accepted status in the staff list | WP15 |
| WP5 | No e2e for the >15 min reauth or a tenant switch mid-request; the auth rate limit makes quick e2e reruns 429 | WP15 |
| WP6 | Artifact builds are not reproducible: Astro embeds a random per-build `key` (server islands), so ids change on every build; set `ASTRO_KEY` per publish from a platform secret | WP23 |
| WP6 | Cart creation and the handoff start are not keyed by Idempotency-Key (a lost response leaves an orphaned cart / needs a new cart); place-order is keyed since WP10 | WP15 |
| WP8 | `checkout.<host>/withdraw` is a placeholder page (the withdrawal flow, A19) | WP12 |
| WP8 | Payment/carrier marks in `/shop` are generic catalog text (legal/CMS links come from published pages since WP13a; the checkout still links the Czech legal slugs `/pages/obchodni-podminky`, `/pages/odstoupeni-od-smlouvy` for every locale) | WP11 / WP12 |
| WP8 | PDP JS headroom is 2.5 kB (27.5 kB gz first visit, 28.0 kB with every consent + the RUM sample); keep islands lean | WP8 successors / WP23 gates |
| WP9/WP18 | Bounces/complaints arrive at `POST /webhooks/ses` (SNS envelope, HTTP Basic `MAIL_EVENTS_SECRET`); SNS message signature verification (SignatureVersion 2, cert URL pinned to `sns.<region>.amazonaws.com`, TopicArn check) is designed in `commerce::marketing::deliverability` but not built; subscription confirmations are only logged | pre-launch |
| WP10 | Stripe and bank transfer are configurable but not offered at checkout (no adapter); the order email has a bank-transfer placeholder | WP11 |
| WP10 | COD cash rounding is not applied at placement (the tender is unknown until collection, A16) | WP11 |
| WP10 | The Packeta widget key is a platform setting (`PACKETA_API_KEY`); per-tenant carrier credentials and verifying the chosen point against the Packeta API | WP12 |
| WP10 | The real Packeta widget (`library.js` + callback) is only exercised against the local mock; validate on the pre-launch checklist | WP15 |
| WP10 | No cancellation email when an unpaid order expires; refunds of late/duplicate payments are done by hand and then marked settled in the exceptions queue (WP11), no refund UI yet | WP12 |
| WP10 | Payment timeouts are one global scan per minute (orders expire up to ~1.5 min late); per-tenant order numbers serialize placements of one tenant on the counter row | later (perf) |
| WP14 | Meilisearch is not backed up; a restore rebuilds every tenant's index (`api admin reindex`), search is degraded until it finishes | accepted (A27) |
| WP14 | Mailpit (local test mail) is not backed up | accepted (local only) |
| WP14 | Local backups mirror buckets to files: object metadata is dropped (Content-Type restored from the extension, public Cache-Control re-applied); prod relies on R2 versioning/replication instead | accepted (local only) |
| WP14 | Prod backup automation (PITR config, nightly dump job to a separate EU R2 account, bucket versioning, quarterly drill) is documented, not built | WP15 / pre-launch |
| WP14 | `SECRETS_KEY` rotation (re-encrypt stored secrets) is a manual, unsupported operation | later |
| WP14 | A failed local restore leaves database `app` partial (DROP/CREATE DATABASE cannot be transactional); rerun it | accepted (local only) |
| WP14 | Storefront rate limits are in-process buckets per API replica (N replicas allow N× the rate) | later (scale-out / CDN rate limiting) |
| WP14 | Dashboard days are UTC, not the merchant's time zone; revenue is placed, non-cancelled order totals (refunds not netted until WP11/WP12 publish them) | later / WP12 |
| WP14 | Top searches come from consented sessions only (A20); zero-result searches are still the API-side log of all visitors (edge-cache misses only, per locale, accepted in WP7) | later |
| WP14 | Refund analytics: WP11 publishes `order.refunded` (`refunded_minor`, `full`) when an order's payment becomes (partially) refunded; netting it in the dashboard is left | WP12 |
| WP14 | Every webhook-type outbox event gets a fan-out job even for tenants without subscriptions | later (perf) |
| WP13a | Feed import applies product by product and feeds render in memory per market; batch/stream for 100k-item catalogs | later (perf) |
| WP13a | Legal templates are starting points; every shop needs a lawyer's review (the admin says so) | pre-launch |
| WP13a | Orders/customers CSV import, tenant data export, customer access/erasure (A29) | WP13b |
| WP17 | The hourly rollup recomputes the tenant's co-purchases, scores and customer affinity in full (stats for the last 2 days; the nightly run recomputes all 400 retained days so late cancellations leave every result); fine for demo-sized shops, narrow it to changed orders for large catalogs/order books | later (perf) |
| WP11 | `payments::refund(attempt)` (Stripe with `refund_application_fee` and the refund id as idempotency key, bank/COD recorded) and `payments::retry_refund` have no admin screen yet; a Stripe refund whose outcome is unknown stays `pending` until retried or reconciled by `refund.*` webhooks; WP12 wires them into returns/withdrawals and a pending-refund list | WP12 |
| WP11 | COD `delivered` is set by hand (or the carrier CSV stub `POST /admin/v1/cod-reports`); carrier tracking and real COD payout imports (Packeta/PPL) | WP12 |
| WP11 | The QR code in emails is inline SVG: Gmail and some clients do not render it (the text instructions always are); a CID PNG attachment needs attachments in the mail pipeline | WP14 / pre-launch |
| WP11 | The real Stripe Payment Element, Stripe-hosted onboarding and Connect webhooks are only exercised against stripe-mock + the simulator; validate with Stripe test keys, plus a manual scan of both QR codes in banking apps (A25) | WP15 (pre-launch checklist) |
| WP11 | `platform.provider_events` keeps payloads indefinitely (PaymentIntent objects may hold billing details); add a retention rule to WP14's `ops.sweep` (e.g. drop payloads of processed events after 90 days) | later (ops) |
| WP11 | Fio tokens share WP14's single `SECRETS_KEY` (no key id in the ciphertext); no rotation tooling | later |
| WP11 | Payment reminder and email due dates are the UTC date of the deadline | later |
| WP20 | Seznam SEM S2S attribution normally needs the `sid`/`udid` cookies of its `sul.js` browser script, which the platform does not load (no third-party scripts); matching relies on hashed email/phone. Capturing the `sznaiid` click id at the edge would help | later |
| WP20 | Google Ads gets purchases only (Data Manager API offline conversions / enhanced conversions for leads by hashed email/phone); no gclid capture, no refund retractions | later |
| WP20 | Meta receives no `client_ip_address` (IPs are only stored hashed, §14) and no `fbp`/`fbc` (no Meta pixel); match quality relies on hashed email/phone/external_id + user agent | accepted |
| WP20 | Ad-platform rate limits and Google access-token caches are per worker process | later (scale-out) |
| WP22 | Translations cover names, descriptions, SEO, page blocks and menu labels; option/value names, parameter texts and image alt texts are not translated by AI yet | later |
| WP22 | One entity per proposal: no "translate every product missing sk" batch job (bulk plans cover non-text fields) | later |
| WP22 | The AI quota is a soft limit (concurrent calls may overshoot by one call); no superadmin UI for quotas (CLI `set-ai-quota`) | later (only if it matters) |
| WP22 | Old `ai_proposals` / `ai_bulk_plans` rows are never purged | later (ops) |
| WP20/WP22 | Full e2e with 4 workers: checkout handoff `/start` timeouts in 2 specs + shared demo-owner sign-ins hit the auth rate limit; make the suite reliable (per-spec users, IP-aware auth limits from WP12) | WP15 |
| WP18 | No open tracking at all (privacy default); the optional consented tracking pixel of §11.5 is not built | later (only if merchants ask) |
| WP18 | Marketing message bodies stay in `email_messages` indefinitely (one row per recipient); add a retention rule to `ops.sweep` (e.g. drop bodies of final marketing mail after 30 days) | later (ops) |
| WP18 | The marketing rate is one platform constant (500 messages per tenant and minute); per-tenant sending quotas | later |
| WP18 | Subscriber import (CSV) and the AI copy assist per segment (§11.5) | WP13b / M3 |
| WP18 | Segment purchase conditions use placed orders of the same address or linked customer; refunds are not netted in `total_spent` | later |
