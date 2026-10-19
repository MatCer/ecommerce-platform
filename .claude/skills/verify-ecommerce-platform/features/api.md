# API and staff access

The Rust API serves health, readiness, the OpenAPI document and the Admin API, which needs a
5-minute staff JWT from Better Auth plus `X-Tenant-Id`. Staff sign in by magic link from Mailpit.

## Sub-features

- `api-health` answers `/healthz` and `/readyz` with per-dependency checks.
- `api-openapi` serves `/openapi.json` and Swagger UI at `/docs`.
- `api-auth` refuses Admin API calls without a valid token as RFC 9457 problem+json.
- `api-staff` signs a staff user in, mints a JWT and enforces tenant membership (cross-tenant 403).

## How to get to it (user POV)

- Native API: `http://127.0.0.1:$API_PORT` (SKILL.md A).
- Held stack: `http://api.localhost:38080`; staff sign-in at `http://admin.localhost:38080/login`.

## Driving it with curl and the smoke scripts

Preconditions:

- Native API ready (`/readyz` ok) or a held stack up. Staff flows need the held stack.

- **Health.** `curl -sS http://127.0.0.1:$API_PORT/readyz` prints
  `{"status":"ok","checks":{"database":"ok","meilisearch":"ok","storage":"ok"}}`.
- **OpenAPI.** `curl -sS http://127.0.0.1:$API_PORT/openapi.json | jq -c '{title: .info.title, paths: (.paths|length)}'`
  prints `Commerce Platform API` and the path count of this branch.
- **Auth gate.** `curl -sS -D - http://127.0.0.1:$API_PORT/admin/v1/me` returns `401`,
  `content-type: application/problem+json`, `"code":"missing_token"`.
- **Staff flow.** In the held copy run `scripts/smoke-staff-flow.sh`: creates two tenants, signs in
  by magic link, calls `/admin/v1/me` and markets with an `Idempotency-Key` replay, and checks the
  cross-tenant `403`. Exit 0 and its step log are the proof.
- **Admin SPA sign-in.** At `/login` with English UI, fill `label "Email"` with
  `owner@lnen.example`, click `button "Email me a sign-in link"`, open the newest magic link from
  Mailpit for that address. The admin dashboard loads signed in.
- **Side effect.** Confirm writes through SQL or a second GET, e.g. the audit log
  `GET /admin/v1/audit-log` with the JWT and `X-Tenant-Id`.

## Gotchas

- On the native API the database is empty: no demo tenant, so admin calls with a real JWT see no
  memberships; prove tenant behavior on the held stack.
- `/storefront/*` is not reachable natively without the edge's token, and `api.localhost` returns
  404 for it by design.
- Staff invitations answer 503 on the native API (auth `/internal` is not proxied).
- Better Auth rate-limits sign-ins per client IP; sign in once per run and reuse the session.
