# Follow-ups ledger (gaps reported by merged WPs, with owning WP)

| From | Gap | Owner |
|---|---|---|
| WP0 | CI doesn't smoke-test the auth image (needs Postgres) | WP15 |
| WP1 | The outbox dispatcher polls (500 ms); no LISTEN/NOTIFY | WP14 (only if lag matters) |
| WP1 | The superadmin dead-job view | WP14 |
| WP1 | The smoke scripts are manual; move them into the e2e suite | WP15 |
| WP2 | Artifact GC (private bucket `artifacts/` + edge cache volume); WP6 keeps everything | WP23 |
| WP2 | Page-model gaps left after WP8: size chart, dispatch cutoff + holidays, batch card lookup by ids (recently viewed shows no prices until then); font library | WP15 |
| WP3 | Assets stuck in `processing` after the final lease-timeout death → needs a dead-job sweeper | WP14 |
| WP3 | Abandoned uploads / reused presigned URLs never cleaned up | WP14 |
| WP4 | A sale change recomputes every priced variant of the tenant; narrow it for very large catalogs | later (perf) |
| WP4 | Price history returns the full timeline per variant (no pagination) | later |
| WP5 | No invitation-accepted status in the staff list | WP15 |
| WP5 | No e2e for the >15 min reauth or a tenant switch mid-request; the auth rate limit makes quick e2e reruns 429 | WP15 |
| WP7 | Real popularity signal is a placeholder (tenant synonyms done in WP13a) | WP17 |
| WP7 | Indexes of locales removed from all markets are never dropped | WP14 |
| WP7 | Zero-result log stores minimized query text (possible PII typed by users); reviewed and accepted with minimization + 90 d TTL | WP14 (dashboard) |
| WP6 | No per-token/IP rate limits on the Storefront API yet (spec §8.1) | WP14 |
| WP6 | Expired carts (30 days) and used/expired handoff rows are not swept by a job (handoffs are pruned on mint) | WP14 |
| WP6 | `/events` and `/newsletter/subscribe` validate and drop (no storage) | WP14 / M2 |
| WP6 | Artifact builds are not reproducible: Astro embeds a random per-build `key` (server islands), so ids change on every build; set `ASTRO_KEY` per publish from a platform secret | WP23 |
| WP6 | Cart creation and the handoff start are not keyed by Idempotency-Key (a lost response leaves an orphaned cart / needs a new cart); place-order is keyed since WP10 | WP15 |
| WP8 | `checkout.<host>/withdraw` is a placeholder page (the withdrawal flow, A19) | WP12 |
| WP8 | Payment/carrier marks in `/shop` are generic catalog text (legal/CMS links come from published pages since WP13a; the checkout still links the Czech legal slugs `/pages/obchodni-podminky`, `/pages/odstoupeni-od-smlouvy` for every locale) | WP11 / WP12 |
| WP8 | No cart cross-sell in the drawer yet (the PDP slot renders `/recommendations`; the drawer would need a client fetch) | WP17 |
| WP8 | PDP JS headroom is 2.5 kB (27.5 kB gz first visit, 28.0 kB with every consent + the RUM sample); keep islands lean | WP8 successors / WP23 gates |
| WP9 | Email suppressions are added manually (`api admin suppress-email`); no bounce/complaint ingestion from the provider (SES notifications) | WP14 / pre-launch |
| WP9 | Per-tenant editable email subject/intro text (§11.4) and a tenant logo in emails (no logo in the data model yet; the shop name is the wordmark) | WP13b |
| WP9 | Marketing stream has no `List-Unsubscribe` headers yet (no marketing mail exists) | M2 (newsletter) |
| WP9 | No admin view of `email_messages` (states, failures) or of the suppression list | WP14 |
| WP10 | The Packeta widget key is a platform setting (`PACKETA_API_KEY`); per-tenant carrier credentials and verifying the chosen point against the Packeta API | WP12 |
| WP10 | The real Packeta widget (`library.js` + callback) is only exercised against the local mock; validate on the pre-launch checklist | WP15 |
| WP10 | No cancellation email when an unpaid order expires; refunds of late/duplicate payments are done by hand and then marked settled in the exceptions queue (WP11), no refund UI yet | WP12 |
| WP10 | Payment timeouts are one global scan per minute (orders expire up to ~1.5 min late); per-tenant order numbers serialize placements of one tenant on the counter row | later (perf) |
| WP13a | Feed import applies product by product and feeds render in memory per market; batch/stream for 100k-item catalogs | later (perf) |
| WP13a | Legal templates are starting points; every shop needs a lawyer's review (the admin says so) | pre-launch |
| WP13a | Orders/customers CSV import, tenant data export, customer access/erasure (A29) | WP13b |
| WP11 | `payments::refund` (Stripe with `refund_application_fee`, bank/COD recorded) has no admin screen yet; WP12 wires it into returns/withdrawals | WP12 |
| WP11 | COD `delivered` is set by hand (or the carrier CSV stub `POST /admin/v1/cod-reports`); carrier tracking and real COD payout imports (Packeta/PPL) | WP12 |
| WP11 | The QR code in emails is inline SVG: Gmail and some clients do not render it (the text instructions always are); a CID PNG attachment needs attachments in the mail pipeline | WP14 / pre-launch |
| WP11 | The real Stripe Payment Element, Stripe-hosted onboarding and Connect webhooks are only exercised against stripe-mock + the simulator; validate with Stripe test keys, plus a manual scan of both QR codes in banking apps (A25) | WP15 (pre-launch checklist) |
| WP11 | `platform.provider_events` keeps payloads indefinitely (PaymentIntent objects may hold billing details); add retention | WP14 |
| WP11 | One `PAYMENTS_SECRET_KEY` for stored Fio tokens (ciphertexts carry a version byte for a future rotation); no rotation tooling | later |
| WP11 | Changing a market's IBAN in place keeps open orders' instructions, but statements of the old IBAN are then refused for that account row; add accounts instead of editing when switching banks | later |
| WP11 | Payment reminder and email due dates are the UTC date of the deadline | later |
