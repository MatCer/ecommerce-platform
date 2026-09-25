# WP1 Identity, tenancy, jobs: implementation plan

> **For agentic workers:** execute task by task with TDD; commit after every task.

**Goal:** tenants/markets/domains with RLS, staff identity via Better Auth + EdDSA JWT verified
in Rust, audit log, idempotency store, superadmin CLI, internal host resolution, and the
outbox + leased job queue + cron leader the later WPs build on.

**Spec:** §5, §7.7, §8.1, §8.4, §13, §14, §17 (WP1) and binding amendments A8, A9, A12, A14
(jobs only), A29 (domain verification stub), A30 (no per-tenant fairness).

## Global Constraints

- Roles: `app_owner` owns every table and function; `app_runtime` (api/worker/CLI) owns
  nothing and has no BYPASSRLS; `auth_service` owns only the `auth` schema.
- Every tenant table: `tenant_id uuid not null`, policy `tenant_isolation TO app_runtime`
  on `current_setting('app.tenant_id')::uuid`, `ENABLE` + `FORCE ROW LEVEL SECURITY`.
  A catalog test enforces this for every table in `public`.
- Tenant tables are only touched through `platform::db::TenantTx` (type-level: commerce
  functions take `&mut TenantTx`).
- `queue.*` tables have no grants to `app_runtime`; access only via `SECURITY DEFINER`
  functions with `search_path = ''`.
- Cross-tenant platform reads (membership lookup, retention purge) are `SECURITY DEFINER`
  functions owned by `app_owner`, backed by `TO app_owner` policies (FORCE RLS stays on).
- SQL via `sqlx::query!` macros with offline data in `.sqlx/` (`SQLX_OFFLINE=true` in
  `.cargo/config.toml`; `make sqlx-prepare` refreshes it).

## Review Focus

- RLS/grant model and fail-closed behaviour of `tenant_tx` (connection reuse).
- JWT validation: algorithm pinning, iss/aud/exp, JWKS refresh throttling, membership check.
- Queue fencing: `complete`/`fail`/`heartbeat` only with the current `lease_token`.
- Idempotency semantics (same key + different body => 409).

## Tasks

### 1. Migrations: platform, tenant tables, queue, functions
Files: `migrations/20260925100000_tenancy.sql`, `migrations/20260925100100_queue.sql`.
- `platform.uuid_v7()`, `platform.tenants`, `platform.domains` (hostname PK,
  `verification_token`, `verified_at`), `platform.platform_admins`.
- `markets`, `staff_members`, `audit_log` (runtime: SELECT/INSERT only), `idempotency_keys`.
- `platform.staff_membership(user_id, tenant_id) returns text`,
  `platform.staff_tenants(user_id)`, `platform.purge_idempotency_keys()`.
- `queue.outbox`, `queue.jobs`; `queue.publish`, `queue.enqueue`, `queue.claim`
  (reclaims expired leases, kills expired final attempts), `queue.heartbeat`,
  `queue.complete`, `queue.fail`, `queue.claim_outbox`, `queue.mark_dispatched`, `queue.purge`.
Tests (`crates/platform/tests/rls.rs`): catalog guard, runtime has no direct queue access.

### 2. `platform::db::TenantTx`
```rust
pub async fn tenant_tx(pool: &PgPool, tenant_id: Uuid) -> Result<TenantTx, sqlx::Error>;
impl TenantTx { fn tenant_id(&self) -> Uuid; async fn commit(self); async fn rollback(self) }
impl DerefMut for TenantTx { type Target = PgConnection; }
```
Tests: no context => error; cross-tenant read/update/delete/insert denied for every tenant
table; pool of 1 reused after commit, rollback, and a cancelled (timed out) transaction
never carries the previous tenant.

### 3. `platform::queue` + worker
- Rust wrappers for the SQL functions; `backoff(attempt) -> Duration` (exponential, full
  jitter, capped).
- `worker` becomes lib + bin: `runner` (N loops: claim 1, heartbeat while the handler runs,
  complete/fail), `dispatcher` (claim_outbox + enqueue per subscriber + mark_dispatched in one
  transaction; key `outbox:<id>:<kind>`), `cron` (leader via `pg_try_advisory_lock` on a
  dedicated connection; enqueue with key `cron:<name>:<slot>`), handlers `events.log`,
  `maintenance.cleanup`.
Tests: crash + lease expiry reclaim, stale completion fenced, heartbeat fencing, retry then
dead, final-attempt lease expiry => dead, dispatcher atomic + no double dispatch, cron single
leader + slot dedupe, runner end to end with a flaky handler.

### 4. Commerce: tenants, markets, domains, staff, audit, idempotency
`crates/commerce/src/{tenancy.rs, markets.rs, audit.rs, idempotency.rs}`; validation of slug,
hostnames, market fields. Idempotency: insert-first in the same transaction (a concurrent
duplicate blocks on the unique index, then replays).

### 5. Admin API auth + endpoints
- `api::auth::Jwks` (10 min cache, forced refresh on unknown kid at most every 30 s),
  `StaffUser` / `TenantStaff` extractors (EdDSA only, iss/aud/exp, `email_verified`,
  `X-Tenant-Id` + `platform.staff_membership`), `Role` ordering, `require_fresh_auth()`
  => `401 reauth_required` after 15 min.
- `GET /admin/v1/me`, `GET/POST /admin/v1/markets` (audit + outbox + Idempotency-Key),
  `GET /admin/v1/audit-log`; CORS for `ADMIN_ORIGIN`.
- `GET /internal/v1/resolve?host=` behind `INTERNAL_API_TOKEN`.
Tests: signed test tokens against a local JWKS server; 401/403 matrix; idempotent replay/409.

### 6. CLI
`api admin create-tenant --slug --name --owner-email`, `api admin add-domain --tenant --host
[--market] [--primary]`, `api admin verify-domain --host` (TXT lookup via the mocks DNS stub).

### 7. apps/auth (Better Auth on Hono)
Email+password (sign-up disabled: staff are invited), magic link, email verification, TOTP,
JWT plugin (EdDSA, `iss=http://auth.localhost`, `aud=admin-api`, 5 min, payload
`{email, email_verified, auth_time}`), `/token` refused unless the email is verified;
internal `POST /internal/users` + `/internal/users/invite` (service token) for the CLI.
Compose service + Caddy `auth.localhost`; mocks gain `/dns/txt`.

### 8. OpenAPI + TS clients, README, verification
`make openapi`; full stack e2e: CLI tenant, magic link from Mailpit API, JWT, `/admin/v1/me`,
market create through Caddy, cross-tenant 403; `make lint test`.
