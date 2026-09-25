# Follow-ups ledger (gaps reported by merged WPs, with owning WP)

| From | Gap | Owner |
|---|---|---|
| WP0 | CI doesn't smoke-test the auth image (needs Postgres) | WP15 |
| WP1 | The outbox dispatcher polls (500 ms); no LISTEN/NOTIFY | WP14 (only if lag matters) |
| WP1 | The superadmin dead-job view | WP14 |
| WP1 | The smoke scripts are manual; move them into the e2e suite | WP15 |
| WP2 | Artifact GC (private bucket `artifacts/` + edge cache volume); WP6 keeps everything | WP23 |
| WP2 | Page-model gaps left after WP8: size chart, dispatch cutoff + holidays, batch card lookup by ids (recently viewed shows no prices until then); font library | WP10 / WP13 |
| WP3 | Assets stuck in `processing` after the final lease-timeout death → needs a dead-job sweeper | WP14 |
| WP3 | Abandoned uploads / reused presigned URLs never cleaned up | WP14 |
| WP4 | A sale change recomputes every priced variant of the tenant; narrow it for very large catalogs | later (perf) |
| WP4 | Price history returns the full timeline per variant (no pagination) | later |
| WP4 | Allocations are not yet persisted on order lines/charges | WP10 |
| WP5 | The staff invite email is sent before the membership commits (not via the outbox) | WP9 (mail core) |
| WP5 | No invitation-accepted status in the staff list | WP15 |
| WP5 | No e2e for the >15 min reauth or a tenant switch mid-request; the auth rate limit makes quick e2e reruns 429 | WP15 |
| WP7 | Tenant synonyms + real popularity signal are placeholders | WP17 (popularity), WP13 (synonyms admin) |
| WP7 | Indexes of locales removed from all markets are never dropped | WP14 |
| WP7 | Zero-result log stores minimized query text (possible PII typed by users); reviewed and accepted with minimization + 90 d TTL | WP14 (dashboard) |
| WP6 | No per-token/IP rate limits on the Storefront API yet (spec §8.1) | WP14 |
| WP6 | Expired carts (30 days) and used/expired handoff rows are not swept by a job (handoffs are pruned on mint) | WP14 |
| WP6 | `/events` and `/newsletter/subscribe` validate and drop (no storage) | WP14 / M2 |
| WP6 | Catalog/content changes do not purge the edge; HTML ages out by `cache.max_age` (60 s) | WP13 |
| WP6 | Artifact builds are not reproducible: Astro embeds a random per-build `key` (server islands), so ids change on every build; set `ASTRO_KEY` per publish from a platform secret | WP23 |
| WP6 | Cart creation and the handoff start are not keyed by Idempotency-Key (a lost response leaves an orphaned cart / needs a new cart) | WP10 |
| WP8 | The edge forwards `POST /_p/consent` to `/storefront/v1/consent` and answers 202 `{recorded:false}` while the API has no such route; the consent record's subject (anon id / customer) is not passed yet | WP9 |
| WP8 | The theme links `checkout.<host>/account` and `/withdraw` (A19); the checkout app must serve both | WP9 / WP13 |
| WP8 | A market locale without product translations gets an empty search index (documents need a translation); a default-locale fallback would keep listings full. The demo uses `cs` on the SK market, which is fully translated | WP13 |
| WP8 | Legal/CMS page slugs in `/shop` are Czech placeholders for every locale; payment/carrier marks are generic catalog text until payment and shipping methods exist; `free_shipping_threshold` stays null | WP13 / WP10 / WP11 |
| WP8 | No cart cross-sell in the drawer yet (the PDP slot renders `/recommendations`; the drawer would need a client fetch) | WP17 |
| WP8 | PDP JS headroom is 2.5 kB (27.5 kB gz first visit, 28.0 kB with every consent + the RUM sample); keep islands lean | WP8 successors / WP23 gates |
