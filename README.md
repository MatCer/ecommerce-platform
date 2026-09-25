# EU Commerce Platform

Multi-tenant e-commerce SaaS for small and medium CZ/SK shops, EU-ready. A Rust (axum + sqlx)
modular monolith with a Postgres database, a background worker, generated TypeScript API
clients, and a local Docker stack that mirrors production.

Design: [`docs/superpowers/specs/2026-09-24-platform-design.md`](docs/superpowers/specs/2026-09-24-platform-design.md).

## Layout

```text
crates/commerce   business modules (no HTTP, no framework types)
crates/platform   config, problem+json errors, tracing, db pool, S3 storage, health checks
crates/api        axum binary: health, OpenAPI, Admin API (/admin/v1), Internal API, superadmin CLI
crates/worker     job runner, outbox dispatcher, cron leader
crates/testkit    shared test helpers
migrations/       sqlx migrations (run as app_owner)
packages/         shared TS config + generated API clients (admin-client, storefront-sdk)
apps/auth         Better Auth (Hono): staff sign-in, magic links, TOTP, EdDSA JWTs + JWKS
apps/mocks        Hono service standing in for third-party APIs (incl. a DNS TXT stub)
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
| http://api.localhost:8080 | Rust API (`/healthz`, `/readyz`, `/openapi.json`, `/docs` Swagger UI, `/admin/v1`) |
| http://auth.localhost:8080 | Better Auth (`/api/auth/*`, JWKS at `/api/auth/jwks`) |
| http://mail.localhost:8080 | Mailpit UI (also http://localhost:58025) |
| http://s3.localhost:8080 | MinIO S3 API (`public` bucket is anonymously readable) |
| http://localhost:59001 | MinIO console (`MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD` from `.env`) |
| localhost:55432 | Postgres (`app` and `app_test` databases) |
| http://localhost:57700 | Meilisearch |
| http://localhost:12111 | stripe-mock |

`admin.localhost` and shop hosts (`demo.localhost`, `checkout.demo.localhost`) answer 502 until
their work packages land. `/internal/*` on `api` and `auth` is never proxied by Caddy; it is for
services on the compose network only. Every host port is configurable in `.env`
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
| `make sqlx-prepare` | Refresh `.sqlx/` (offline `query!` data) after SQL changes; commit it |
| `make admin args="..."` | Superadmin CLI in the api container (see below) |
| `make logs s=api`, `make ps` | Logs / status |

Running the API natively against `make dev-infra` (values from `.env.example`):

```bash
make migrate
APP_ENV=dev API_BIND=127.0.0.1:8000 \
DATABASE_URL=postgres://app_runtime:app-runtime-local@localhost:55432/app \
MEILI_URL=http://localhost:57700 S3_ENDPOINT=http://localhost:59000 \
S3_ACCESS_KEY_ID=app-local S3_SECRET_ACCESS_KEY=app-local-secret-key \
S3_BUCKET_PUBLIC=public S3_BUCKET_PRIVATE=private \
AUTH_JWKS_URL=http://auth.localhost:8080/api/auth/jwks ADMIN_ORIGIN=http://admin.localhost:8080 \
INTERNAL_API_TOKEN=local-internal-api-token-0123456789abcdef \
cargo run -p api
```

## Tenants and staff sign-in

Staff accounts are invite-only (no public sign-up). A superadmin creates a tenant with its
default CZ market, the `<slug>.localhost` domain and an owner, who gets a magic link by email:

```bash
make admin args="create-tenant --slug demo --name 'Demo shop' --owner-email owner@example.com"
# open the link from http://mail.localhost:8080; it signs in and verifies the address
make admin args="add-domain --tenant demo --host shop.example.cz"      # prints the TXT record
make admin args="verify-domain --host shop.example.cz"                 # checks it (DNS stub)
```

The admin SPA (and anything else) then gets a 5-minute JWT from
`GET http://auth.localhost:8080/api/auth/token` (session cookie) and calls the Admin API with
`Authorization: Bearer <jwt>` and `X-Tenant-Id: <tenant uuid>`. Mutations accept an
`Idempotency-Key` header. `scripts/smoke-staff-flow.sh` runs the whole flow against the stack,
including the cross-tenant 403.

## Catalog and media

Products are written as one document (`POST /admin/v1/products`, `PUT /admin/v1/products/{id}`):
attributes, GPSR, translations, options, variants (kept by `id`), categories, media order,
parameter values and per-country tax categories (`GET /admin/v1/tax-categories`, EU-27 rates
seeded in the migration; unmapped countries use `standard`).

Images: `POST /admin/v1/assets/uploads` returns a presigned `PUT` URL into the private bucket
(`http://s3.localhost:8080/private/...`), `POST /admin/v1/assets/{id}/complete` verifies the file
and the worker writes AVIF/WebP/JPEG (or PNG) variants at 160-1920 px to the public bucket
(`http://s3.localhost:8080/public/media/...`). `scripts/smoke-catalog.sh` runs the whole flow.

## Database roles and tenancy

`app_owner` owns the databases, every table and function, and runs migrations. `app_runtime` is
what the API, worker and CLI connect as: it owns nothing and has no `BYPASSRLS`. `auth_service`
(Better Auth) owns only the `auth` schema.

Tenant tables live in `public` with `tenant_id`, a `tenant_isolation` policy and forced RLS (a
test enforces this for every table). Code reaches them only through `platform::db::tenant_tx`,
which sets the transaction-local `app.tenant_id`; without it, queries fail. The `queue` schema
(outbox, jobs) is reachable only through `SECURITY DEFINER` functions.

## Conventions

- Errors are RFC 9457 `application/problem+json` with a stable `code`.
- TS API clients are generated from the Rust OpenAPI document and committed; CI fails when stale.
- Build and test parallelism is capped (`CARGO_BUILD_JOBS=6`).
