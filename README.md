# EU Commerce Platform

Multi-tenant e-commerce SaaS for small and medium CZ/SK shops, EU-ready. A Rust (axum + sqlx)
modular monolith with a Postgres database, a background worker, generated TypeScript API
clients, and a local Docker stack that mirrors production.

Design: [`docs/superpowers/specs/2026-09-24-platform-design.md`](docs/superpowers/specs/2026-09-24-platform-design.md).

## Layout

```text
crates/commerce   business modules (no HTTP, no framework types)
crates/platform   config, problem+json errors, tracing, db pool, S3 storage, health checks
crates/api        axum binary: /healthz, /readyz, /openapi.json, /docs (dev)
crates/worker     background worker binary
crates/testkit    shared test helpers
migrations/       sqlx migrations (run as app_owner)
packages/         shared TS config + generated API clients (admin-client, storefront-sdk)
apps/mocks        Hono service standing in for third-party APIs
docker/           Dockerfiles, Caddyfile, Postgres init script
```

## Prerequisites

- Docker with Compose v2
- Rust stable (`rustup`), plus `sqlx-cli` for `make migrate`:
  `cargo install sqlx-cli --no-default-features --features rustls,postgres --locked`
- Node 24 and pnpm 11 (`corepack enable` picks the pinned version)

## Quick start

```bash
pnpm install
make up          # builds images, starts everything, waits until healthy (creates .env on first run)
curl http://api.localhost:8080/healthz
curl http://api.localhost:8080/readyz
make down
```

`*.localhost` resolves to loopback in browsers and curl, so no `/etc/hosts` edits are needed.

## Local URLs (default ports)

| URL | What |
|---|---|
| http://api.localhost:8080 | Rust API (`/healthz`, `/readyz`, `/openapi.json`, `/docs` Swagger UI) |
| http://mail.localhost:8080 | Mailpit UI (also http://localhost:58025) |
| http://s3.localhost:8080 | MinIO S3 API (`public` bucket is anonymously readable) |
| http://localhost:59001 | MinIO console (`MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD` from `.env`) |
| localhost:55432 | Postgres (`app` and `app_test` databases) |
| http://localhost:57700 | Meilisearch |
| http://localhost:12111 | stripe-mock |

`admin.localhost`, `auth.localhost` and shop hosts (`demo.localhost`, `checkout.demo.localhost`)
answer 502 until their work packages land. Every host port is configurable in `.env`
(see `.env.example`).

All published ports bind to `127.0.0.1` only, because the stack runs with well-known local
credentials. To reach it from another machine (a phone on your LAN, a VM), set
`HOST_BIND=0.0.0.0` (or one LAN IP) in `.env` and run `make up` again. Do that only on a
trusted network.

## Common commands

| Command | What it does |
|---|---|
| `make up` / `make down` | Start / stop the full stack (`docker compose down -v` also wipes data) |
| `make dev-infra` | Dependencies only; then `cargo run -p api` against them (see below) |
| `make migrate` | Apply migrations as `app_owner` |
| `make test` | Rust tests (need Postgres from `make dev-infra`) + vitest |
| `make lint` | rustfmt check, clippy `-D warnings`, Biome, TS typecheck |
| `make fmt` | Format Rust and TS |
| `make openapi` | Regenerate `openapi.json` and the TS clients; commit the result |
| `make openapi-check` | Fail if the generated clients are stale (runs in CI) |
| `make logs s=api`, `make ps` | Logs / status |

Running the API natively against `make dev-infra` (values from `.env.example`):

```bash
make migrate
APP_ENV=dev API_BIND=127.0.0.1:8000 \
DATABASE_URL=postgres://app_runtime:app-runtime-local@localhost:55432/app \
MEILI_URL=http://localhost:57700 S3_ENDPOINT=http://localhost:59000 \
S3_ACCESS_KEY_ID=app-local S3_SECRET_ACCESS_KEY=app-local-secret-key \
S3_BUCKET_PUBLIC=public S3_BUCKET_PRIVATE=private \
cargo run -p api
```

## Database roles

`app_owner` owns the databases and runs migrations. `app_runtime` is what the API and worker
connect as: not an owner and without `BYPASSRLS`, so row-level security applies to it.

## Conventions

- Errors are RFC 9457 `application/problem+json` with a stable `code`.
- TS API clients are generated from the Rust OpenAPI document and committed; CI fails when stale.
- Build and test parallelism is capped (`CARGO_BUILD_JOBS=6`).
