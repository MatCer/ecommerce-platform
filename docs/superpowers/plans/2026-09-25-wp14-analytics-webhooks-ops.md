# WP14 Analytics, webhooks, ops: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** privacy-first analytics (A20: cookieless edge counters before consent, consented
browser events after, authoritative purchases from the outbox) with rollups and an admin
dashboard; signed outbound webhooks through the SSRF-safe client (A21) with retries and a
delivery log; operations plumbing: Prometheus metrics, a superadmin dead-job view, sweepers,
Storefront API rate limits, a local backup + restore drill (A29) and the runbook.

**Architecture:**
- `commerce::analytics`: counter upserts, event ingestion (consent resolved server-side),
  purchase events, sessionization at ingest (30 min gap), rollups into `daily_metrics`,
  dashboard queries (sales from `orders`, traffic from counters/events).
- `commerce::webhooks`: subscriptions (secret encrypted at rest, shown once), fan-out per
  outbox event, delivery with HMAC-SHA256 `X-Signature: t=<ts>,v1=<hex>` over `<ts>.<body>`,
  exponential retries for 24 h, then `dead`; redeliver.
- `commerce::ops`: sweepers (dead media jobs → failed assets, abandoned uploads, expired carts
  and handoffs, stale locale indexes).
- `platform::http::SafeClient` (A21): own DNS resolution, public unicast only (v4 + v6),
  pinned connect, redirects re-validated (max 3), no credentials/cookies, 20 MB body cap,
  10 s timeout; dev-only host allowlist (`SAFE_FETCH_ALLOW_HOSTS` from WP13a).
- `platform::crypto::SecretBox`: AES-256-GCM with a platform key from env (`SECRETS_KEY`),
  reused by WP20 for ad-platform credentials.
- `platform::metrics`: Prometheus recorder; api request histogram middleware; worker queue
  gauges (`queue.stats()`), job durations. `/metrics` served on a separate internal port
  (`METRICS_BIND`), never routed by Caddy.
- Storefront rate limit: `governor` keyed GCRA per (storefront token, client IP) as an axum
  middleware on `/storefront/v1/*`, `429` problem+json + `Retry-After`. The edge forwards the
  client IP (last `X-Forwarded-For` hop from Caddy) on every API call, SSR bindings included.
- Edge: in-memory counters per (tenant, market, UTC day, route template) and search-query
  counts, flushed every 30 s to `POST /internal/v1/analytics/counters`; the consent subject
  cookie goes with `/_p/e` and checkout calls.
- Worker: outbox subscribers (`analytics.purchase`, `webhooks.fanout`), deliveries, cron
  (hourly rollups, nightly partitions + retention, sweeps every 15 min), `/metrics`.
- Admin (Solid): dashboard, webhooks (subscriptions, delivery log, redeliver), superadmin
  jobs page. `make backup` / `make restore` / `scripts/backup-drill.sh`, `docs/runbook.md`.

## Global constraints

- Every new tenant table: `tenant_id`, RLS + `FORCE`, cross-tenant test. `events` is
  partitioned by month; RLS on the parent, grants on the parent only.
- A20: no identifiers in counters; events only when `consent_records` grant `analytics` for
  the subject at ingest; beacon purposes ignored; props allowlisted per event type.
- A21: webhook URLs only through `SafeClient`; https required in prod.
- Secrets: webhook secrets encrypted at rest, returned only on create/rotate.
- `/metrics` not reachable through Caddy.
- No `unwrap()` outside tests, sqlx macros with `.sqlx/`, TS strict, Biome clean.

## Review focus

- Consent gate for events (server-resolved, derived anon id, no raw subject stored).
- SSRF client: IP classification, redirects, DNS pinning, size cap.
- Webhook signing, retry schedule, 24 h window, fencing against duplicate delivery jobs.
- Partition maintenance + 13-month retention; RLS on partitioned `events`.
- Rate limiter key/IP trust, memory bound (`retain_recent`).
- Backup/restore drill actually restores DB + buckets and the shop serves again.

## Tasks

1. **Migration** `20261001000000_analytics_webhooks_ops.sql`: `analytics_counters`,
   `search_query_counts`, `events` (monthly partitions + `platform.ensure_event_partitions`,
   `platform.drop_event_partitions`), `daily_metrics`, `order_analytics` linkage via purchase
   event upsert, `webhook_subscriptions`, `webhook_deliveries`, `assets.upload_purged_at`,
   `queue.stats()`, `queue.list_jobs()`, `queue.requeue()`, dead-job retention.
2. **platform**: `http::SafeClient` (+ unit tests for IP classes, redirect limits),
   `crypto::SecretBox`, `metrics`, config (`SECRETS_KEY`,
   `METRICS_BIND`, rate limits), queue stats/list/requeue.
3. **commerce::analytics** (+ tests: consent gate, props validation, sessionization,
   rollups, dashboard numbers, cross-tenant).
4. **commerce::webhooks** (+ tests: signing vector, retry schedule, fan-out, redeliver,
   delivery against a local receiver through an allowlisted SafeClient).
5. **commerce::ops** sweepers (+ tests).
6. **api**: events ingest, internal counters, admin analytics, admin webhooks, superadmin
   jobs (`Me.is_superadmin`), metrics listener + middleware, storefront rate limit,
   purchase linkage after place-order.
7. **worker**: handlers, subscribers, cron, metrics server.
8. **edge**: counters + flush, consent subject on `/_p/e` and checkout, client IP on bindings,
   search counting (+ vitest).
9. **theme/SDK**: `page_view`, `view_item`, `add_to_cart`, `begin_checkout` via the beacon.
10. **mocks**: webhook receiver with signature verification and failure mode.
11. **OpenAPI + clients**, compose env (metrics ports, secrets key, allowlist).
12. **Admin UI** + e2e (dashboard, webhooks).
13. **Backup/restore** scripts + drill, `docs/runbook.md`, follow-ups ledger.
14. Verification on the stack, Astra review, PR.
