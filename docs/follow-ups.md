# Follow-ups ledger (gaps reported by merged WPs, with owning WP)

| From | Gap | Owner |
|---|---|---|
| WP0 | CI doesn't smoke-test the auth image (needs Postgres) | WP15 |
| WP1 | The outbox dispatcher polls (500 ms); no LISTEN/NOTIFY | WP14 (only if lag matters) |
| WP1 | The superadmin dead-job view | WP14 |
| WP1 | The smoke scripts are manual; move them into the e2e suite | WP15 |
| WP2 | Handoff tokens live in edge memory; they must move to the API/DB | WP6 |
| WP2 | Real `/internal/v1/resolve` + Storefront API replace the mocks stub; WfP mapping notes; artifact GC | WP6 |
| WP2 | Page-model gaps: second card image, size chart, dispatch cutoff; platform i18n catalogs; font library; image `sizes`/preload helper; calls-per-page budget | WP6/WP8 |
| WP2 | JS headroom on PDP is 0.8 kB with RUM sampled; keep islands lean | WP8 |
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
