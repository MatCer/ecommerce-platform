# WP0 Foundation Implementation Plan

> For agentic workers: execute task by task, TDD where there is logic, commit after every task.

**Goal:** a monorepo skeleton where `make up` brings the full local stack (Postgres, Meilisearch,
MinIO, Mailpit, stripe-mock, mocks, Caddy, api, worker) up healthy, the Rust API serves health,
readiness and OpenAPI, TS clients are generated from that OpenAPI, and CI enforces all of it.

**Spec:** `docs/superpowers/specs/2026-09-24-platform-design.md` §2, §3, §4, §8.1, §15, §16, §17 (WP0 row).

## Global Constraints

- Rust stable, edition 2024, `cargo clippy --all-targets -- -D warnings`, no `unwrap()` outside tests,
  `thiserror` in libs, `anyhow` only in binaries. `CARGO_BUILD_JOBS=6`.
- Versions pinned (exact in `package.json`, `=`-free but locked in `Cargo.lock`; images by full tag).
- No tenancy/RLS policies, auth, jobs/outbox or business modules (WP1+).
- All host ports configurable via `.env` (defaults in `.env.example`); compose project `wp0` for verification.
- TS strict, no `any`, Biome clean, pnpm only.

## Review Focus

- Problem+json shape and status/code mapping (`crates/platform/src/error.rs`).
- Readiness semantics: `/healthz` never touches dependencies; `/readyz` returns 503 + per-check detail.
- Postgres roles: `app_runtime` has no `BYPASSRLS`, is not owner; migrations run as `app_owner`.
- Secrets only from env; nothing secret in logs, images or committed files.
- OpenAPI drift check actually fails on stale clients.

## Key decisions

- **Object storage client: `object_store` (aws feature)**, not `aws-sdk-s3`. It covers S3, R2 and MinIO
  with one `ObjectStore` trait (tests use `InMemory`), supports presigned URLs (needed in WP2) and
  compiles far faster than the AWS SDK. It reuses `reqwest`, which the Meilisearch check needs anyway.
- **MinIO image: `pgsty/minio` + `pgsty/mc`.** Upstream `minio/minio` images are no longer published
  (Docker Hub repo gone, quay requires auth). `pgsty/*` is the maintained community build of the same
  server, so the spec's "local MinIO" holds.
- **OpenAPI export without a server:** `api openapi` prints the spec; `make openapi` writes
  `openapi.json` (committed snapshot) and runs `openapi-typescript` into both client packages.
- **Migrations:** `make migrate` runs `sqlx migrate run` as `app_owner`; the compose stack runs the
  same migrations through a one-shot `migrate` service (`api migrate`, embedded `sqlx::migrate!`), so
  `make up` needs no host tooling.
- **Container health:** `api healthcheck` subcommand (distroless image has no shell/curl).
- **Spec amendments (§20):** buckets are `public` + `private` (A21); Meilisearch is reported by
  `/readyz` but does not fail it (A30); Caddy's catch-all sends every other host, including
  `checkout.<shop>.localhost` (A1), to the edge placeholder.

## Tasks

### Task 1: Cargo workspace + commerce/testkit skeleton
Files: `Cargo.toml`, `rustfmt.toml`, `crates/{commerce,testkit}/`.
- `commerce::id::new_id() -> Uuid` (UUIDv7, D4). Test: version 7 and monotonic ordering.

### Task 2: platform crate
Files: `crates/platform/src/{lib,config,error,telemetry,db,storage,health,shutdown}.rs`.
- `config`: `HttpConfig`, `DbConfig`, `MeiliConfig`, `S3Config`, each `from_env()` + `from_lookup(&dyn Fn)`.
  Tests: defaults, missing required var, invalid number.
- `error::Error` (thiserror) → `IntoResponse` as `application/problem+json`
  `{type:"about:blank", title, status, detail, code}`; internal errors hide detail and are logged.
  Tests: status/code/content-type per variant, internal detail not leaked.
- `db::pool(&DbConfig)` (lazy pool), `db::ping`. `storage::s3(&S3Config, bucket)`, `storage::ping`
  (NotFound on a sentinel key counts as reachable). `health::readiness(...) -> Readiness`
  (concurrent checks with 2 s timeout each).
- `telemetry::init()` JSON tracing with `RUST_LOG` filter. `shutdown::signal()` (SIGINT/SIGTERM).

### Task 3: migrations
`migrations/<ts>_platform_schema.sql`: `CREATE SCHEMA platform`, usage + default privileges for `app_runtime`.
`#[sqlx::test]` asserts the schema exists.

### Task 4: api crate
Files: `crates/api/src/{main,lib,routes}.rs`, `crates/api/tests/http.rs`.
- Router: `/healthz`, `/readyz`, `/openapi.json`, `/docs` (only `APP_ENV=dev`), problem+json 404 fallback,
  request-id (set + propagate `x-request-id`), `TraceLayer` span with request id, 1 MiB body limit.
- `main`: `serve` (default, graceful shutdown), `openapi`, `migrate`, `healthcheck`.
- Tests (oneshot): healthz 200; openapi lists paths; 404 problem+json; request id generated and echoed;
  413 on oversized body; docs only in dev; readyz 503 with DB ok + Meili down (`#[sqlx::test]`).

### Task 5: worker crate
DB pool, heartbeat log every 30 s (`SELECT 1`), graceful shutdown.

### Task 6: pnpm workspace + TS packages
`package.json`, `pnpm-workspace.yaml`, `biome.json`, `packages/config` (tsconfig base, Tailwind 4 `@theme`
tokens stub), `packages/{admin-client,storefront-sdk}` (generated `schema.d.ts`, `createClient` wrapper,
vitest), `apps/mocks` (Hono `/healthz`, vitest), `scripts/openapi-drift.sh`.

### Task 7: Docker + Caddy + Makefile + .env.example
Compose per spec §3.2/§15, profile `infra`, cargo-chef Dockerfile (distroless nonroot runtime),
Postgres init script (roles + `app`/`app_test`), bucket init, Caddyfile.

### Task 8: CI + README
`.github/workflows/ci.yml` (rust job with Postgres 17 service, ts job, openapi drift), README.

### Task 9: Verification
`make up` healthy, curl via Caddy (`/healthz`, `/readyz`, `/openapi.json`), `make lint test`,
`make openapi` no diff, `make down`.
