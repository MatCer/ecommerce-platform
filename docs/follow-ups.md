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
| WP3 | Staff management API (the smoke uses psql) | WP5 |
| WP3 | Product list search unindexed (pg_trgm) | WP5 |
