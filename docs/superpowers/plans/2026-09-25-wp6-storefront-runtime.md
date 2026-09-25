# WP6 Storefront runtime: implementation plan

> **For agentic workers:** execute task by task with TDD; commit after every task.

**Goal:** replace WP2's stubs with the real thing: a Rust Storefront API (page models, cart,
checkout handoff, SEO files) behind storefront tokens, the edge resolving sites and loading
theme artifacts from the platform, generated SDK types, and a demo seed that makes
`demo.localhost` / `demo-sk.localhost` browsable end to end.

**Spec:** §5.5, §8.1, §8.2, §9, §10.3, §17 (WP6) and amendments A1, A2, A4, A7, A12, A21, A22,
A30. Builds on `docs/decisions/runtime-contract.md` (WP2) and `docs/follow-ups.md`.

## Global Constraints

- Business logic in `crates/commerce` (`storefront`, `cart`, `redirects`, `themes`); `api`
  only parses, authenticates and opens `tenant_tx`. Storefront router in its own files
  (`crates/api/src/storefront/`).
- Every new tenant table: `tenant_id`, RLS + FORCE, composite FKs, cross-tenant test.
- Storefront calls: `X-Storefront-Token` → tenant (401 otherwise); `X-Market` must be a market
  of that tenant (403 `market_mismatch`); a client `X-Tenant` that disagrees is 403. Admin API
  never accepts a storefront token (it only knows staff JWTs).
- Capability tokens (cart, handoff): 256-bit random, hex, only SHA-256 stored, compared by
  hash lookup inside the tenant's RLS scope.
- Page models: every one has `seo {title, description, canonical, alternates[], json_ld[],
  robots}` and `cache {public, max_age, tags[]}`. Money is `{amount_minor, currency,
  formatted}`.
- Edge keeps every WP2 security test green; new behaviour gets its own tests.
- No new runtime dependency unless justified in the PR.

## Design decisions

- **Storefront tokens** live in `platform.storefront_tokens` (public by design, like Shopify's
  public storefront token, so stored in plain text: the edge needs it from `resolve`).
  Rotation keeps the old token valid for 5 minutes (> the edge's 60 s resolver cache) and
  purges the edge.
- **Listing seam for WP7:** `commerce::storefront::listing(tx, ctx, &ListingQuery) ->
  Listing { product_ids, total, facets }` (Postgres: one candidate query + a pure
  filter/sort/facet step). Cards are hydrated separately by `cards(tx, ctx, ids)`, which WP7
  reuses to rehydrate Meilisearch hits (A23). Facet counts are not shown; zero-match values are
  `disabled`.
- **Images** carry URLs (the pipeline's keys are content-addressed, no URL convention):
  `{alt, width, height, src, srcset, srcset_webp}` with same-origin `/media/<key>` paths. The
  SDK's `lcpImage()` returns the preload link and `<img>` attributes from one `sizes` value.
- **Cart** (`commerce::cart`): `carts` with `shop_token_hash` / `checkout_token_hash`,
  `version` bumped on every change, `cart_lines`, one `cart_coupons` row (stacking: one
  coupon). Totals via `price_cart` on every read; VAT for the cart's ship-to country, defaulting
  to the market's first country (A3). An invalid coupon stays attached but unapplied with a
  reason.
- **Handoff (A1/A4):** `POST /cart/handoff` (shop capability) revokes the shop capability and
  mints a 60 s single-use handoff token bound to tenant + market + cart; `POST
  /checkout/handoff {token}` consumes it atomically and mints the checkout-scoped capability.
  The edge stops keeping state.
- **Themes (A30):** `platform.theme_artifacts` (content-addressed ids, files in the private
  bucket under `artifacts/<id>/…`), `platform.artifact_channels` (`default-theme`,
  `checkout`), per-tenant `theme_revisions` + `theme_active`. Publishing the default artifact
  appends a revision for every tenant that follows the default and moves `theme_active`.
  `resolve` returns the active artifact, retained ones (previous 3 or 7 days) and the checkout
  artifact. The edge downloads missing artifacts from `GET /internal/v1/artifacts/{id}/{path}`
  and verifies the content address before use. Deviation: per-file objects instead of
  `bundle.tar.zst` (no archive format to parse; integrity by the content address).
- **Redirects:** `redirects` table + Admin CRUD; `to_path` is a relative path only (no open
  redirects). The edge asks `GET /redirects/resolve?path=` when the theme renders 404.
- **SEO files:** `/files/robots.txt`, `/files/llms.txt`, `/files/sitemap.xml` (index) and
  `/files/sitemap-<n>.xml` (10k URLs each, hreflang alternates), per market (= per host).
- **i18n:** platform message catalogs (cs/sk/en) compiled into the API; `/shop` returns only
  the active locale's messages.
- **Seed:** `api admin seed-demo`, idempotent per step, all through the commerce services;
  images are hue-shifted variants of the fixture photos pushed through the real upload →
  complete → process pipeline.
- **OpenAPI:** `api openapi --storefront` emits the storefront subset; the SDK's types are
  generated from it (`schema.d.ts`) and `types.ts` only re-exports named schemas.

## Review Focus

- Tenant/market binding of every storefront query (token → tenant, market re-validated).
- Capability tokens: hashing, scope (shop vs checkout), rotation at handoff, single use.
- Omnibus claims on cards and PDP only against the reference (A18), never `compare_at`.
- Edge: artifact download integrity, cache allowlist unchanged, `/media` scoped to the tenant.

## Tasks

1. **Schema** — migration `20260927000000_storefront.sql`: storefront tokens, redirects,
   carts/lines/coupons, checkout handoffs, theme artifacts/channels/revisions/active. RLS test.
2. **Redirects** — `commerce::redirects` (validate, CRUD, resolve) + Admin API + tests.
3. **Storefront tokens + resolve** — token on tenant creation, rotation service + admin
   endpoints, `Resolved` extended with token + artifacts; tests incl. cross-tenant misuse.
4. **Storefront context + page models** — `commerce::storefront` (context, images, cards,
   listing, product, home, shop, messages) + `api::storefront` router; tests.
5. **Cart + handoff** — `commerce::cart` + endpoints; tests (pricing/VAT, scopes, rotation,
   single use, cross-tenant).
6. **SEO files** — sitemap index/chunks, robots, llms; tests.
7. **Artifacts** — themes service, internal artifact file endpoint, `api admin
   publish-artifacts`; tests.
8. **Seed** — `api admin seed-demo` + `make seed`.
9. **OpenAPI + SDK** — storefront subset, generated types, `lcpImage()`, clients regenerated.
10. **Edge** — API resolver, artifact fetcher + verification, handoff via API, redirects on
    404, tenant-scoped `/media`; tests. Docker/compose/Caddy/Makefile wiring.
11. **Theme + checkout** — compile against generated types, real data, cart totals + VAT in
    checkout; theme-kit call budget in the smoke gate.
12. **Verification** — stack up, seed, theme-build, Playwright smoke (cs + sk), cache and
    token-misuse checks, `make perf`, `make lint test`; Astra review; PR.
