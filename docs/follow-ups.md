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
| WP10 | Stripe and bank transfer are configurable but not offered at checkout (no adapter); the order email has a bank-transfer placeholder | WP11 |
| WP10 | COD cash rounding is not applied at placement (the tender is unknown until collection, A16) | WP11 |
| WP10 | The Packeta widget key is a platform setting (`PACKETA_API_KEY`); per-tenant carrier credentials and verifying the chosen point against the Packeta API | WP12 |
| WP10 | The real Packeta widget (`library.js` + callback) is only exercised against the local mock; validate on the pre-launch checklist | WP15 |
| WP10 | No cancellation email when an unpaid order expires; late-payment exceptions have no admin action yet (refund task) | WP12 |
| WP10 | Payment timeouts are one global scan per minute (orders expire up to ~1.5 min late); per-tenant order numbers serialize placements of one tenant on the counter row | later (perf) |
| WP13a | Feed import applies product by product and feeds render in memory per market; batch/stream for 100k-item catalogs | later (perf) |
| WP13a | Legal templates are starting points; every shop needs a lawyer's review (the admin says so) | pre-launch |
| WP13a | Orders/customers CSV import, tenant data export, customer access/erasure (A29) | WP13b |
| WP22 | Translations cover names, descriptions, SEO, page blocks and menu labels; option/value names, parameter texts and image alt texts are not translated by AI yet | later |
| WP22 | One entity per proposal: no "translate every product missing sk" batch job (bulk plans cover non-text fields) | later |
| WP22 | The AI quota is a soft limit (concurrent calls may overshoot by one call); no superadmin UI for quotas (CLI `set-ai-quota`) | WP14 (only if it matters) |
| WP22 | Old `ai_proposals` / `ai_bulk_plans` rows are never purged | WP14 |
