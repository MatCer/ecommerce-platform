# WP2 Runtime + Trust-Boundary Spike Implementation Plan

> For agentic workers: execute task by task, TDD where there is logic, commit after every task.

**Goal:** prove that Astro + Solid theme bundles run in the local edge (Node + Miniflare/workerd)
behind a gateway that owns tenancy, caching, headers and the checkout handoff, measure a
full-island product page against the §9.6 budget, check whether AI edits within the theme
contract are feasible, and leave reusable skeleton code plus `docs/decisions/runtime-contract.md`.

**Spec:** §3, §9 (9.1–9.7), §12.3, §17 WP2 row, amendments A1, A2, A4, A6, A7, A22, A26, A30.

## Global Constraints

- No tenancy/auth/DB work (WP1). Tenant resolution is a static JSON host map behind the
  `SiteResolver` interface whose `Site` shape is the future `GET /internal/v1/resolve` response.
- Storefront API is a stub (`apps/mocks`, `/storefront/v1/*`, fixture data) until WP6.
- Exact version pins in the pnpm catalog; workerd/miniflare must be the build wrangler ships.
- TS strict, no `any`, Biome clean. Playwright workers ≤ 4, Lighthouse serial.
- Docker project `wp2`, ports from the task prompt, `docker-compose.override.yml` git-ignored.

## Review Focus

- Trust boundary: a theme worker gets only `STOREFRONT` (+ read-only `ASSETS` of its own
  artifact); tenant comes only from the Host via a server-side request context; no outbound
  fetch **or TCP connect**.
- Cache policy: every A2 "never cache" rule has a test; themes cannot extend TTLs.
- Handoff: single use, 60 s, hashed at rest, host-bound, `__Host-` cookie on the checkout origin.
- Header stripping before resolution; CSP with hashes only for Astro/Solid bootstrap scripts.

## Key decisions

- **Artifact = content-addressed directory** `<root>/<id>/{manifest.json,server/,client/}`
  produced by `theme-kit pack` from `astro build`. Compatibility date/flags come from the
  platform (`RUNTIME` in theme-kit), not from the theme.
- **Per-request context id** instead of trusting anything the worker sends: the edge opens a
  context (tenant, market, artifact), passes an opaque id in `x-platform-ctx`, the Node-side
  binding resolves it and rejects ids issued to another artifact.
- **Miniflare 5 `workers[]` API** (the README still documents v3 options).

## Tasks

1. Version matrix: catalog pins, `themes/default` minimal Astro+Solid build, run in Miniflare.
2. `packages/theme-kit`: `packArtifact`, manifest schema, CSP hashes, tokens schema + Vite plugin.
3. `apps/edge`: runtime pool, sites resolver, bindings, cache policy, handoff, gateway, server.
   Tests: hostile theme fixture (A7), header hygiene, cache rules (A2), publish/rollback/
   eviction/restart (A22), cart + handoff (A1/A4).
4. `packages/storefront-sdk`: page-model types (§8.2 stubs), `createStorefront`, cart client,
   money/image helpers, consent-aware beacon.
5. `apps/mocks` stub Storefront API with fixtures + generated AVIF images.
6. `themes/default` probe: product + category pages with all islands; `apps/checkout` skeleton.
7. `packages/theme-kit` measurement: Playwright (A26 JS until idle + scroll) + Lighthouse mobile.
8. Docker compose + Caddy wiring; end-to-end verification through Caddy.
9. AI-edit feasibility: 10 prompts, gates, results in `docs/decisions/ai-edit-prompts.md`.
10. Decision record `docs/decisions/runtime-contract.md`.
