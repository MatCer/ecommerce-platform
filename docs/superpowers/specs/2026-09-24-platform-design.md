# EU Commerce Platform: v1 Design Spec (M1–M3)

Status: approved for planning 2026-09-24 (owner delegated all remaining decisions: "recommended way, best practice").
Inputs: `docs/scope.md`, `docs/research/architecture.md`, `docs/research/eu-market-agentic.md`.
Scope of this spec: everything needed to build M1, M2 and M3 **running locally** (Docker). Production hosting is described so local choices stay portable, but nothing is provisioned in the cloud.

---

## 1. Goals and non-goals

### Goals
- Multi-tenant SaaS e-commerce for small/medium CZ/SK B2C shops, EU-ready.
- Mobile storefront that meets the performance budget (§9.6) by construction.
- Every merchant has their own storefront code (Hydrogen/Oxygen-style, "approach C"), starting from a polished default theme. In M3, AI edits that code by prompt, behind automated quality gates.
- EU/CZ/SK-native commerce: CZK/EUR, VAT incl. OSS, Omnibus price history, GPSR fields, QR bank transfer, COD, Packeta, legal invoices, Heureka/Zboží/Google feeds.
- Local development is one command: `make up`.

### Non-goals (v1)
- B2B features (v2); the data model stays B2B-ready.
- Public self-signup and automated billing.
- Third-party app marketplace, WASM plugins.
- MCP server, ACP/UCP/AP2 agent checkout.
- Any real cloud resource (R2, PlanetScale, SES, Workers for Platforms). Local stand-ins are listed in §15.

---

## 2. Decision log

| # | Decision | Choice | Why |
|---|---|---|---|
| D1 | Backend language / framework | Rust, **axum 0.8** modular monolith, **sqlx** (Postgres) | Research §1; explicit SQL fits RLS, pricing and reporting |
| D2 | Services | One Rust `api` binary (Storefront API + Admin API routers) + one Rust `worker` binary | Modular monolith; no microservices |
| D3 | Database | **PostgreSQL 17**. Shared schema, `tenant_id` + RLS on every tenant table. Prod target PlanetScale Postgres Frankfurt | Shopify-style pods later; RLS as defense in depth |
| D4 | IDs | UUIDv7 (`uuid` crate `v7`), human numbers (orders, invoices) via per-tenant sequences | Sortable, no enumeration |
| D5 | Jobs | **Transactional outbox + Postgres `jobs` table with `FOR UPDATE SKIP LOCKED`** | No extra broker, no RC crates; one proven pattern |
| D6 | Search | **Meilisearch** (pinned 1.x image), one index per tenant+locale, our own cs/sk normalizer field | Disk-based, cheaper multi-tenant than Typesense; the owner tested both, no winner |
| D7 | Storefront | **Astro + Solid islands + Tailwind 4** theme project per merchant, built to a Cloudflare Worker bundle (`@astrojs/cloudflare`) | Minimal JS by default; TSX that AI writes well; the same bundle runs locally in workerd |
| D8 | Storefront hosting | Prod: Workers for Platforms dispatcher. Local: **edge** service (Node + Miniflare/workerd) with the same routing and bundle format | Same artifact locally and in prod |
| D9 | Checkout/account | Platform-owned Astro+Solid app on reserved paths (`/checkout`, `/account`, `/_p/*`). Never merchant code | Security boundary (research §3) |
| D10 | Admin | **SolidJS SPA** (Vite, Solid Router, TanStack Solid Query, **Kobalte** a11y primitives, Tailwind 4), cs/sk/en | Owner preference + accessible primitives |
| D11 | Staff auth | **Better Auth** service (Node, Hono) in its own `auth` schema. JWT plugin + JWKS, verified by Rust | Research §12; no custom auth protocols |
| D12 | Customer auth | Rust-native, tenant-scoped: argon2id passwords (`argon2` crate), magic links, opaque session tokens (hashed) | Customers must be isolated per shop; Better Auth's account model doesn't fit multi-shop customer identity |
| D13 | API style | REST + JSON, OpenAPI 3.1 generated from Rust (**utoipa**). TS clients generated (`openapi-typescript` + `openapi-fetch`) | Page-model endpoints beat GraphQL waterfalls; one contract |
| D14 | Payments | Stripe **Connect direct charges** (Standard-like accounts, optional `application_fee_amount`), QR bank transfer (SPAYD / PAY by square) + statement matching, COD. Local: stripe-mock + built-in fake gateway | Scope; Stripe docs |
| D15 | Shipping | Packeta (pickup points + home), PPL. Local: mock carrier APIs | Scope |
| D16 | Invoices | PDF via **Typst** (`typst` CLI in the worker image), CZ/SK legal fields, VAT recap in CZK via ČNB rates | Deterministic, high-quality PDFs without headless Chrome |
| D17 | Email | `lettre` SMTP + MJML templates via **mrml** + minijinja variables. Separate transactional/marketing streams. Local: **Mailpit**. Prod: SES SMTP | SMTP keeps local and prod identical |
| D18 | Object storage | S3 API. Local **MinIO**. Prod R2 | S3-compatible, zero egress in prod |
| D19 | Images | Fixed variants generated on upload by the worker (widths 160/320/640/960/1280/1920; AVIF + WebP + original format fallback) | Predictable, cacheable, no runtime transforms locally |
| D20 | Analytics store | Postgres (monthly partitioned `events`) + daily rollups | Research §7; ClickHouse later |
| D21 | AI provider | Anthropic Messages API behind an `ai` module; model IDs configurable (defaults: `claude-opus-5-5` for theme code, `claude-sonnet-5` for admin helpers). Deterministic fake provider for tests/local without a key | Latest models, testable without network |
| D22 | Repo | Monorepo: cargo workspace + pnpm workspace; **Biome** for TS lint/format; rustfmt + clippy `-D warnings` | Solo dev, atomic changes |
| D23 | Local orchestration | Docker Compose + **Caddy** reverse proxy on `*.localhost`; `Makefile` entrypoints | `*.localhost` resolves to loopback in all browsers, so no /etc/hosts edits |
| D24 | Testing | Rust unit + `#[sqlx::test]` integration; Vitest; **Playwright** e2e on the compose stack; **Lighthouse CI** + **axe** budgets | Completion contract requires real-interface checks |
| D25 | Rates and money | Integer minor units (`i64`) + ISO currency; tax rates as `rust_decimal`; gross prices entered for B2C | No floats in money |

---

## 3. System architecture

```text
Browser / shopping agent
        |
   Caddy (local) / Cloudflare (prod)
        |
   edge  (local: Node + Miniflare; prod: WfP dispatch Worker)
     ├── hostname → tenant + market + active theme revision (from API, cached)
     ├── /checkout, /account, /_p/*   → checkout app bundle (platform-owned)
     ├── /feeds/*, /sitemap*.xml, /robots.txt, /llms.txt → API passthrough
     └── everything else              → tenant theme bundle (merchant code)
                    |
        Storefront API (public token + customer session)
                    |
   api  (Rust axum)  ──  Admin API (staff JWT)  ←── admin SPA, AI helpers
     |        |
     |        └── auth (Better Auth, Node)  ← staff login, JWKS
     |
   PostgreSQL 17 (RLS) ── outbox ── worker (Rust)
                                     ├── Meilisearch indexing
                                     ├── emails (SMTP → Mailpit/SES)
                                     ├── images, feeds, invoices (Typst)
                                     ├── flows (abandoned cart, watchdog, reviews)
                                     ├── analytics rollups, ad-platform forwarders
                                     └── webhooks out
   theme-builder (Node, M3) ── builds + checks theme revisions → object storage
   MinIO (S3) ── assets, image variants, theme bundles, invoices, feeds
```

### 3.1 Local vs prod mapping

| Concern | Local (this project) | Prod (later) |
|---|---|---|
| Reverse proxy / TLS | Caddy, http on `:8080` (`HTTP_PORT` overridable) | Cloudflare + Cloudflare for SaaS custom hostnames |
| Storefront runtime | `edge` (Node + Miniflare) | Workers for Platforms dispatch namespace |
| API / worker / auth | Docker containers | Hetzner DE containers (fallback AWS Frankfurt) |
| Postgres | `postgres:17` container | PlanetScale Postgres Frankfurt |
| Search | `getmeili/meilisearch` container | Meilisearch on Hetzner / Meilisearch Cloud EU |
| Object storage | MinIO | R2 (EU jurisdiction) |
| Email | Mailpit | SES eu-central-1 SMTP |
| Payments | stripe-mock + fake gateway; real Stripe test mode if keys set | Stripe Connect |
| Carriers, bank, ČNB, ad platforms | `mocks` service | Real APIs |
| AI | Fake provider; real Anthropic if `ANTHROPIC_API_KEY` set | Anthropic |

### 3.2 Local hostnames (all via Caddy on `http://*.localhost:8080`)
- `admin.localhost` → admin SPA
- `api.localhost` → Rust API (`/storefront/v1/*`, `/admin/v1/*`, `/openapi.json`, `/docs`)
- `auth.localhost` → Better Auth
- `mail.localhost` → Mailpit UI
- `s3.localhost` → MinIO (public-read bucket for assets)
- `<shop>.localhost` (e.g. `demo.localhost`, `demo-sk.localhost`) → edge. Each market can have its own domain.
- `preview-<rev>--<shop>.localhost` → edge, theme preview (M3)

---

## 4. Repository layout

```text
Cargo.toml                 cargo workspace
package.json, pnpm-workspace.yaml, biome.json
Makefile                   up, down, dev-infra, migrate, seed, test, e2e, lint, fmt, openapi
docker-compose.yml         full stack; profile "infra" for deps only
.env.example
crates/
  commerce/                lib: all business modules (§6), SQL, services. No HTTP, no workers-rs.
  platform/                lib: config, db pool + tenant tx, outbox/jobs, storage (S3), mail, ai client, errors, telemetry
  api/                     bin: axum routers (storefront, admin, internal), auth middleware, OpenAPI
  worker/                  bin: job runner, cron scheduler, handlers
  testkit/                 lib (dev-dependency): fixtures, builders, test tenant helpers
migrations/                sqlx migrations (timestamped)
apps/
  admin/                   Solid SPA
  auth/                    Better Auth (Hono, Node)
  edge/                    Node + Miniflare dispatcher (local stand-in for WfP)
  checkout/                Astro + Solid platform app (checkout, account, consent, withdrawal)
  theme-builder/           M3: Node service that builds/checks theme revisions
  mocks/                   Hono service mocking Packeta, PPL, ČNB, Fio/bank, Meta CAPI, GA4, Google Ads, Sklik
themes/
  default/                 the default Astro + Solid theme (the template every merchant forks)
packages/
  storefront-sdk/          typed Storefront API client + helpers (SEO, money, images, consent, events)
  admin-client/            typed Admin API client
  ui/                      shared Solid components (Kobalte-based) for admin + checkout
  theme-kit/               budgets, Lighthouse/axe configs, theme contract validation (used by CI + theme-builder)
  config/                  shared tsconfig, tailwind preset (design tokens)
e2e/                       Playwright suites against the compose stack
fixtures/                  demo feeds (Heureka/Google XML), CSVs, images
docs/
```

Rules:
- Business logic lives only in `crates/commerce`. It stays independent of axum and workers-rs, so it can run anywhere.
- No crate per capability. Modules are folders inside `commerce`.
- The TS API clients are generated from `/openapi.json` and committed. CI fails if they're stale.

---

## 5. Tenancy, identity and authorization

### 5.1 Tenants, markets, domains
- `platform.tenants(id, slug, name, status, plan, application_fee_bps, legal_entity jsonb, settings jsonb, created_at)`
- `platform.domains(hostname unique, tenant_id, market_id, is_primary, verified_at)`
- `markets(id, tenant_id, code, name, country_codes text[], currency, default_locale, locales text[], price_list_id, tax_mode, is_default)`
- Hostname → (tenant, market) resolution is served by `GET /internal/v1/resolve?host=` (internal token). The edge caches it (60 s TTL + purge on change).

### 5.2 Row-level security
- Every tenant-owned table has `tenant_id uuid not null`. Composite FKs and unique constraints include `tenant_id`.
- Policies: `USING (tenant_id = current_setting('app.tenant_id')::uuid)` + `WITH CHECK` same. `FORCE ROW LEVEL SECURITY` on every tenant table.
- The runtime role `app_runtime` is a non-owner without `BYPASSRLS`. Migrations run as `app_owner`.
- `platform::db::tenant_tx(pool, tenant_id)` begins a transaction and runs `SELECT set_config('app.tenant_id', $1, true)`. A missing setting makes `current_setting` error, so queries fail closed.
- `platform.*` tables (tenants, domains, platform_admins) have no RLS and are only reachable via explicit platform services.
- The integration test suite includes cross-tenant read/write attempts for every module (must fail).

### 5.3 Staff (merchants and their employees)
- Better Auth (`apps/auth`): email + password, magic link, email verification, TOTP 2FA. Its own `auth` schema and its own DB role.
- Admin SPA session: Better Auth cookie on `auth.localhost`. The SPA gets a short-lived JWT (5 min, `aud=admin-api`) via the JWT plugin.
- Rust validates the JWT against the cached JWKS, then loads `staff_members(tenant_id, user_id, role)`. Roles: `owner`, `admin`, `staff` (staff: no settings, payments config, staff management or exports).
- Requests pick a tenant with the `X-Tenant-Id` header. The API verifies the membership. Superadmin: `platform.platform_admins(user_id)`.
- Every mutating Admin API call writes `audit_log(tenant_id, actor, action, entity, entity_id, diff jsonb, at)`.

### 5.4 Customers (shoppers)
- `customers(id, tenant_id, email citext, name, phone, password_hash null, email_verified_at, locale, created_at)`, unique `(tenant_id, email)`.
- Guest checkout by default. The account is optional: set a password after the order, or use a magic link any time.
- Sessions: a random 256-bit token in the `sid` cookie (HttpOnly, Secure in prod, SameSite=Lax, host-only on the shop domain). Only a SHA-256 hash is stored in `customer_sessions`. 30-day sliding expiry.
- Magic links: single use, 15 min, hashed at rest, rate-limited per email + IP.
- Password hashing: argon2id with OWASP parameters.

### 5.5 Storefront API credentials
- Every theme and the checkout app call the Storefront API with a **public storefront token** bound to one tenant (`X-Storefront-Token`). The token only grants public reads + cart/checkout/customer operations for that tenant.
- The edge forwards `X-Forwarded-Host` and the resolved market. The API re-validates that the market belongs to the token's tenant.
- Customer-scoped calls also carry the `sid` cookie. The edge forwards `Cookie` only on `/checkout`, `/account` and `/_p` routes, never to theme bundle subrequests.
- The Admin API is never reachable with a storefront token.

---

## 6. Business modules (`crates/commerce`)

| Module | Owns |
|---|---|
| `tenancy` | markets, domains, tenant settings, legal entity, staff membership |
| `catalog` | products, variants, options, categories, parameters (facets), translations, media links, GPSR, unit price |
| `pricing` | price lists, variant prices, price history (Omnibus), money, tax rates, VAT calculation, OSS |
| `promotions` | sales (automatic discounts), coupons, evaluation engine |
| `inventory` | stock per variant, reservations, back-in-stock transitions |
| `customers` | customers, addresses, sessions, magic links, groups (B2B-ready), consent records |
| `cart` | carts, lines, totals (always recalculated) |
| `checkout` | checkout sessions, shipping/payment selection, order placement (idempotent) |
| `payments` | payment intents, gateways (Stripe, fake), bank transfer QR, statement matching, COD, refunds |
| `shipping` | shipping methods, rates, carriers (Packeta, PPL), labels, tracking |
| `orders` | orders, lines, status machine, events timeline, manual orders/edits, returns/withdrawals |
| `invoicing` | invoice series, invoices, credit notes, proformas, ČNB rates, PDF jobs |
| `content` | pages, blog posts, menus, legal templates, redirects |
| `media` | assets, image variants |
| `search` | index projection, cs/sk normalizer, query building, facets |
| `recommendations` | stats rollups, co-purchase, seasonal collections, affinity |
| `analytics` | events ingestion, rollups, dashboards |
| `marketing` | subscribers, segments, campaigns, flows (abandoned cart), watchdog, review invites |
| `reviews` | reviews, verification, moderation |
| `feeds` | export feeds (Google, Heureka, Zboží), import (feeds, CSV) |
| `notifications` | transactional email templates + sending |
| `webhooks` | subscriptions, signed deliveries |
| `themes` | theme revisions, active pointer, preview tokens (M1: default theme per tenant; M3: AI) |
| `ai` | AI helpers, usage metering, theme-edit sessions (M3) |

Module rules:
- Each module exposes a service API (plain Rust functions taking a `&mut TenantTx` or a pool + tenant). Modules call each other's services, never each other's tables.
- Side effects to other systems happen only via outbox events.

---

## 7. Data model essentials

Only the non-obvious parts are listed. Every table also has `tenant_id`, `created_at`, `updated_at`, UUIDv7 `id`.

### 7.1 Catalog
- `products(status draft|active|archived, brand, tax_class standard|reduced|second_reduced|zero, gpsr jsonb {manufacturer, eu_responsible_person, safety_info, warnings}, unit_measure null|kg|l|m|m2|pcs, unit_quantity numeric null, heureka_category text null, google_category text null)`
- `product_translations(product_id, locale, name, slug, description_html, short_description, seo_title, seo_description)`, unique `(tenant_id, locale, slug)`
- `product_options(product_id, position, name_i18n jsonb)`, `variants(product_id, sku, ean, option_values jsonb, weight_g, position, is_default)`
- `parameters(key, name_i18n, kind text|number|bool, unit, filterable)`, `product_parameter_values(product_id, variant_id null, parameter_id, value_i18n jsonb|number)`
- `categories(parent_id, position, image_asset_id)` + `category_translations(locale, name, slug, description_html, seo_*)` + `product_categories(product_id, category_id, position)`
- `product_media(product_id, variant_id null, asset_id, position, alt_i18n)`

### 7.2 Pricing, promotions, inventory
- `price_lists(code, currency, prices_include_tax bool default true)`, `variant_prices(price_list_id, variant_id, amount_minor, compare_at_minor null)`
- `price_history(variant_id, price_list_id, effective_amount_minor, source base|sale, valid_from, valid_to null)`. Written by triggers in the pricing service whenever the effective price changes (base change, sale start/end). The Omnibus "lowest price in the last 30 days" = min over the window before the current discount started.
- `tax_rates(country_code, tax_class, rate numeric(5,2), valid_from)`. Seeded with CZ/SK + all EU standard/reduced rates.
- `sales(name, starts_at, ends_at, kind percent|fixed, value, targets jsonb {product_ids, category_ids, all})`, `coupons(code citext, kind percent|fixed|free_shipping, value, min_subtotal_minor, starts_at, ends_at, usage_limit, per_customer_limit, used_count)`, `coupon_redemptions`
- `inventory_levels(variant_id, on_hand, reserved, track bool, allow_backorder bool)`. Reservations are made at order placement and released on cancel or payment timeout.

### 7.3 Customers, cart, checkout, orders
- `customer_addresses`, `customer_groups` (B2B-ready, unused in v1 UI), `consent_records(subject_type anon|customer|email, subject_id, purpose analytics|marketing|personalization|reviews, granted bool, text_version, source, ip_hash, at)`. Append-only; the latest record per purpose wins.
- `carts(token_hash, market_id, customer_id null, email null, locale, currency, status open|converted|abandoned, last_activity_at)`, `cart_lines(variant_id, quantity)`, `cart_coupons`
- `orders(number, market_id, customer_id null, email, locale, currency, status, payment_status, fulfillment_status, subtotal_minor, discount_minor, shipping_minor, payment_fee_minor, tax_minor, rounding_minor, total_minor, shipping_method_snapshot jsonb, payment_method code, pickup_point jsonb null, notes, placed_at, idempotency_key unique per tenant)`
- `order_lines(variant_id null, sku, name, options_label, quantity, unit_gross_minor, discount_minor, tax_rate, tax_minor, total_minor)`, `order_addresses(kind billing|shipping, ...)`, `order_events(kind, data jsonb, actor)`
- Status machines (enforced in Rust, tested exhaustively):
  - order `status`: `pending → confirmed → processing → shipped → delivered`. Terminal states: `cancelled`, `returned`, `partially_returned`.
  - `payment_status`: `unpaid → authorized → paid → partially_refunded → refunded`, plus `failed`, `expired`.
  - `fulfillment_status`: `unfulfilled → label_created → shipped → delivered`, plus `returned`.

### 7.4 Payments, shipping, invoicing
- `payments(order_id, method stripe|bank_transfer|cod, status, amount_minor, currency, provider_ref, variable_symbol, qr_payload, expires_at)`, `refunds(payment_id, amount_minor, reason, status, provider_ref)`
- `bank_transactions(account, booked_at, amount_minor, currency, variable_symbol, counterparty, raw jsonb, matched_payment_id null)`
- `shipping_methods(market_id, carrier packeta_pickup|packeta_home|ppl|personal_pickup, name_i18n, price_rules jsonb, free_over_minor null, cod_allowed, cod_fee_minor)`, `shipments(order_id, carrier, carrier_ref, tracking_number, tracking_url, label_asset_id, status)`
- `invoice_series(kind invoice|credit_note|proforma, prefix, year, next_number)`, `invoices(series_id, number, order_id, issued_at, taxable_supply_date, due_at, currency, exchange_rate_czk null, supplier jsonb, customer jsonb, lines jsonb, vat_recap jsonb, totals jsonb, pdf_asset_id, status)`. Immutable once issued; corrections only via credit notes.
- `exchange_rates(date, currency, rate_to_czk)` from the ČNB daily fixing (job at 14:45 CET; mocked locally).

### 7.5 Content, media, themes
- `pages(kind page|legal|blog_post, status, published_at, blocks jsonb)` + `page_translations(locale, title, slug, body_blocks jsonb, seo_*)`, `menus(handle, items jsonb)`, `redirects(from_path, to_path, code 301|302)` unique `(tenant_id, from_path)`
- `assets(key, mime, bytes, width, height, sha256, variants jsonb)`
- `theme_revisions(number, parent_id null, source_key, bundle_key null, status draft|building|checking|ready|failed|published|superseded, checks jsonb, created_by, prompt null, published_at)`, `theme_active(tenant_id pk, revision_id)`

### 7.6 Growth
- `events(id, tenant_id, at, type, anon_id, customer_id, session_id, market_id, props jsonb, consent_purposes text[])`, partitioned by month; `daily_metrics(date, market_id, metric, dims jsonb, value)`
- `product_stats_daily(date, product_id, views, add_to_carts, purchases, revenue_minor)`, `co_purchases(product_a, product_b, count_90d)`, `collections(kind manual|seasonal, schedule, product_ids)`, `customer_affinity(customer_id, dim category|brand, key, score)`
- `subscribers(email, status pending|subscribed|unsubscribed|bounced|complained, locale, market_id, customer_id null, confirm_token_hash)`, `segments(name, rules jsonb)`, `campaigns(name, segment_id, subject_i18n, blocks jsonb, status, scheduled_at, stats jsonb)`, `campaign_sends(campaign_id, subscriber_id, status, sent_at, opened_at, clicked_at)`
- `flows(kind abandoned_cart|review_invite, enabled, steps jsonb)`, `flow_runs(flow_id, subject_ref, step, next_at, status active|completed|cancelled)`
- `watches(email, variant_id, kind back_in_stock|price_drop, target_minor null, status pending|active|fired|cancelled, confirm_token_hash)`
- `reviews(product_id, order_line_id, customer_name, rating 1..5, title, body, status pending|published|rejected, verified bool, locale)`

### 7.7 Platform plumbing
- `outbox(id, tenant_id, type, payload jsonb, created_at, dispatched_at null)`
- `jobs(id, tenant_id null, queue, kind, payload jsonb, run_at, attempts, max_attempts, locked_until, last_error, idempotency_key unique null, status queued|running|done|dead)`
- `webhook_subscriptions(url, events text[], secret)`, `webhook_deliveries(subscription_id, event_id, status, attempts, response_code, next_at)`
- `audit_log`, `ai_usage(tenant_id, feature, model, input_tokens, output_tokens, cost_micros, at)`

---

## 8. APIs

### 8.1 Conventions
- Base paths `/storefront/v1`, `/admin/v1`, `/internal/v1`. JSON, `snake_case`. Errors are RFC 9457 `application/problem+json` with a stable `code`.
- Pagination: cursor-based (`?cursor=&limit=` max 100). Timestamps RFC 3339 UTC. Money `{ "amount_minor": 12900, "currency": "CZK", "formatted": "129,00 Kč" }`.
- Mutations accept `Idempotency-Key`. It's required on order placement, payments and refunds.
- OpenAPI served at `/openapi.json`, Swagger UI at `/docs` in dev.
- Limits: request body 1 MB (uploads 20 MB via presigned URLs), rate limits per token + IP (tower-governor).

### 8.2 Storefront API (page models)
Theme-facing, public, cacheable unless marked private:
- `GET /shop` (tenant, market, locales, currencies, menus, legal pages, consent config, theme settings, tracking config)
- `GET /pages/home`, `GET /pages/category/{slug}?filters&sort&page`, `GET /pages/product/{slug}`, `GET /pages/search?q&filters`, `GET /pages/cms/{slug}`, `GET /pages/blog`, `GET /pages/blog/{slug}`
- `GET /search/suggest?q` (typeahead), `GET /recommendations?context=product:{id}|cart|home`: private if personalized
- `GET /redirects/resolve?path` (edge uses it on 404)
- Cart (private): `POST /cart`, `GET /cart`, `POST /cart/lines`, `PATCH /cart/lines/{id}`, `DELETE /cart/lines/{id}`, `POST /cart/coupons`, `DELETE /cart/coupons/{code}`
- Checkout (private, used by the checkout app): `PUT /checkout/contact`, `PUT /checkout/addresses`, `GET /checkout/shipping-methods`, `PUT /checkout/shipping`, `GET /checkout/payment-methods`, `PUT /checkout/payment`, `POST /checkout/place-order`, `GET /orders/{token}` (confirmation page by an unguessable order token)
- Customer (private): `POST /customer/magic-link`, `POST /customer/login`, `POST /customer/logout`, `GET /customer`, `GET /customer/orders`, addresses CRUD, `POST /customer/password`
- Consent/events: `POST /consent`, `POST /events` (batched beacon)
- Marketing: `POST /newsletter/subscribe`, `POST /newsletter/confirm`, `POST /unsubscribe` (RFC 8058 one-click), `POST /watches`, `POST /watches/confirm`, `POST /reviews` (with review token)
- Withdrawal: `POST /withdrawals` (order token + lines)

Every page model includes `seo {title, description, canonical, alternates[{locale,href}], json_ld[]}` and the cache hints `cache {public, max_age, tags[]}`.

### 8.3 Admin API
Resource CRUD for every module + bulk endpoints (`POST /products/bulk`) + actions (`POST /orders/{id}/ship`, `/refund`, `/cancel`, `/invoices/{id}/credit-note`, `/imports`, `/exports`, `/themes/revisions/{id}/publish`, `/ai/...`). Uploads: `POST /assets/uploads` → presigned PUT → `POST /assets/{id}/complete`.

### 8.4 Internal API
Called by edge, checkout and theme-builder with a service token: `GET /internal/v1/resolve?host=`, `GET /internal/v1/themes/active?tenant=`, `POST /internal/v1/themes/revisions/{id}/status` (builder callbacks).

### 8.5 Webhooks out
Events: `order.created|paid|cancelled|shipped|refunded`, `product.created|updated|deleted`, `inventory.changed`, `customer.created`. HMAC-SHA256 signature header (`X-Signature: t=<ts>,v1=<hex>`), exponential retries up to 24 h, then dead.

---

## 9. Storefront, themes, edge (approach C)

### 9.1 Theme project contract (`themes/default`)
- An Astro project with `output: server`, `@astrojs/cloudflare` adapter, `@astrojs/solid-js`, Tailwind 4. Dependencies are **locked by the platform**: a theme can't add packages (`package.json` is platform-owned; theme-builder rejects changes to it).
- Required routes: `/`, `/c/[...slug]` (category), `/p/[slug]` (product), `/search`, `/pages/[slug]`, `/blog`, `/blog/[slug]`, `404`. Locale prefix for non-default locales (`/sk/...`, `/en/...`) configured per market.
- Data only via `@platform/storefront-sdk` (`createStorefront(context)` → typed page-model fetchers). No `fetch` to other origins (enforced by a lint rule in theme-kit + CSP `connect-src 'self'` on theme responses).
- `theme.config.ts` exports design tokens (colors, typography, radius, spacing scale) → the checkout app uses the same tokens, so checkout matches the brand.
- Islands (Solid, `client:visible`/`client:idle`/`client:load` only for above-the-fold interactivity): variant picker, add-to-cart + mini cart, search typeahead, facet filters (progressive enhancement: filters are plain links/forms that work without JS), image gallery, consent banner (platform component from the SDK), newsletter form.
- Header/footer/nav from `GET /shop`.

### 9.2 Default theme quality bar
- A polished, conversion-focused, mobile-first design: sticky add-to-cart on mobile PDP, free-shipping progress bar, cart cross-sell, trust row (delivery estimate, returns, payment icons), Omnibus lowest-price label next to discounts, unit price, stock state, reviews summary (M2), breadcrumb, JSON-LD (Product, Offer, BreadcrumbList, Organization, AggregateRating in M2).
- Built with the `frontend-design`/`impeccable` skills: a deliberate aesthetic + tokenized design system, no generic look.
- Accessible: semantic landmarks, focus-visible, 4.5:1 contrast, keyboard-operable islands, `prefers-reduced-motion`.

### 9.3 Edge (local stand-in for the WfP dispatcher)
`apps/edge`: a Node service using the **Miniflare** API (workerd) to run Worker bundles.
1. Resolve `Host` → tenant/market/active revision via the internal API (cached 60 s; invalidated by a `POST /_edge/purge` from api on changes).
2. Route reserved paths to the **checkout** bundle; `/feeds/*`, `/sitemap*.xml`, `/robots.txt`, `/llms.txt` to the API; everything else to the tenant theme bundle for `revision_id`.
3. Theme bundles are loaded from object storage (`themes/{tenant}/{revision}/bundle.tar.zst`), unpacked to a local cache dir, and one Miniflare worker instance runs per active revision (LRU of 50).
4. Static assets under `/_astro/*` are served directly from the unpacked bundle with immutable cache headers.
5. HTML caching: an in-memory LRU keyed by `(host, path+normalized query, market, locale, revision)`, honoring the page model's `cache.max_age` (default 60 s) + `stale-while-revalidate` 300 s. Only for GET without `sid`/cart cookies. Purged by tags via `/_edge/purge`.
6. Injects headers: `X-Tenant`, `X-Market`, `X-Storefront-Token`, security headers (CSP per route class, `Referrer-Policy`, `Permissions-Policy`, `X-Content-Type-Options`).
7. Theme bundles run with no bindings except a `STOREFRONT` service binding to the API origin. Outbound fetch to other hosts is blocked (Miniflare `outboundService` allowlist).

If Miniflare can't host the Astro bundle faithfully (a risk tested in WP4), the fallback is running the bundle via `wrangler dev`-compatible workerd config generation. The decision is recorded in the WP4 PR.

### 9.4 Checkout app (`apps/checkout`)
- Astro + Solid, platform-owned, same bundle for all tenants, themed via the tenant's tokens.
- One-page checkout: contact → delivery (Packeta widget loaded on interaction, in an iframe; address with autocomplete off-platform none in v1) → payment (Stripe Payment Element loaded only when Stripe is selected; QR transfer; COD) → summary with legal checkboxes (terms, withdrawal info, optional marketing/reviews consents, unchecked by default) → place order → confirmation (QR code for bank transfer, order token link).
- Account: login (password / magic link), orders, addresses, set password, withdrawal form (EU withdrawal "button" flow).
- Consent banner + preferences page (`/_p/consent`), events beacon endpoint (`/_p/e` → API).

### 9.5 SEO and agent readiness
- Canonicals, `hreflang` alternates across market domains/locales, XML sitemaps per market (index + chunks of 10k), `robots.txt`, JSON-LD on all templates, OpenGraph.
- Facet URLs: only curated category pages are indexable; filtered URLs are `noindex,follow` with a canonical to the base category.
- `/llms.txt` per shop: shop description, key categories, links to the public OpenAPI of the Storefront API and the feeds.
- Redirects: the edge asks `redirects/resolve` on 404 and serves 301s.

### 9.6 Performance budget (acceptance criteria)
Lab measurement (Lighthouse CI, mobile preset: Moto G Power emulation, 4G throttling) on the seeded demo shop for home, category, product:
- LCP ≤ 1.5 s, TBT ≤ 150 ms (INP proxy), CLS ≤ 0.05
- JS ≤ 30 kB gzip on category and product pages (all first-load scripts); home ≤ 35 kB
- 0 third-party origins in the initial load
- LCP image: preloaded AVIF with explicit dimensions and `fetchpriority=high`
- Speculation Rules (`prerender` on hover/`moderate` eagerness for same-origin nav links) + cross-document View Transitions
- Fonts: self-hosted, subsetted (latin-ext), `font-display: swap`, max 2 files preloaded
- axe: 0 serious/critical violations

Field targets for prod (RUM): LCP p75 < 1.5 s, INP p75 < 100 ms, CLS p75 < 0.05. RUM ships in M1 via the events beacon (web-vitals attribution, sampled 10 %).

### 9.7 Theme revisions and publishing
- M1: every tenant gets revision #1 = the default theme with its tokens (`theme_active`). Bundles are built by a script (`make theme-build TENANT=…`) that runs the same pipeline as the M3 builder, minus AI.
- M3: theme-builder pipeline, §12.3.

---

## 10. Commerce core

### 10.1 Money and tax
- `Money { minor: i64, currency: Currency }`. Arithmetic is checked. Formatting per locale (`cs-CZ`: `1 290,00 Kč`; `sk-SK`: `12,90 €`).
- B2C prices are gross (VAT-inclusive). Line VAT = `round_half_up(gross × rate / (100 + rate))` per line. The document VAT recap sums per rate.
- OSS: tenant setting `oss_registered`. When on and the ship-to country (EU) differs from the establishment country, the destination country's rate for the product's tax class applies. The **gross price stays constant**, the net changes (documented policy). When off, the origin country rate applies.
- Cash rounding: COD and cash orders in CZK are rounded to whole CZK. The difference is a `rounding_minor` line, not taxed (standard CZ practice).
- The pricing engine is a pure function `price_cart(input) -> PricedCart` in `commerce::pricing`, with property tests (totals = Σ lines, VAT recap consistent, no negative totals).

### 10.2 Promotions
- Sales change the effective price, so they're visible on product pages with a strikethrough + Omnibus label, and recorded in `price_history`.
- Coupons apply at cart level. Stacking: at most one coupon + active sales. Free-shipping coupons zero the shipping line.
- The Omnibus reference price is shown only when a price reduction is announced (sale/compare-at). The reference is the lowest effective price in the 30 days before the reduction started.

### 10.3 Cart and checkout
- The cart token is in a first-party cookie `cart` (HttpOnly, host-only). Anonymous carts expire after 30 days of inactivity.
- Totals are recomputed on every read; the client never sends prices.
- `place-order` runs in one transaction:
  1. validate the checkout (addresses, shipping method availability for the country, payment method allowed for the shipping method)
  2. re-price
  3. reserve stock (fail if insufficient unless backorder)
  4. increment coupon usage
  5. create the order + lines + payment record
  6. write outbox `order.created`

  Idempotency key: `(tenant, cart_id)`.
- Payment timeouts: Stripe 1 h, bank transfer configurable (default 7 days, with reminders on day 3 and 6), then auto-cancel + release stock.

### 10.4 Payments
- **Stripe (Connect):** the tenant stores a connected account ID (onboarding via Stripe-hosted onboarding link; a stub locally). A PaymentIntent on the connected account (direct charge) with `application_fee_amount = total × tenant.application_fee_bps / 10000` when > 0. The checkout embeds the Payment Element (cards, Apple/Google Pay). Webhook `payment_intent.succeeded|payment_failed` → payment status → order `confirmed`. Webhook signatures are verified.
- **Fake gateway** (local/tests): a checkout page `/_p/fake-pay/{payment}` with Succeed/Fail buttons that posts a signed fake webhook. Enabled only when `PAYMENTS_FAKE=1`.
- **Bank transfer:**
  - variable symbol = numeric order number
  - QR: **SPAYD** for CZK (`SPD*1.0*ACC:<IBAN>*AM:<amount>*CC:CZK*X-VS:<vs>*MSG:<shop>`) and **PAY by square** for EUR/SK (LZMA-compressed, base32hex; implemented per spec with test vectors)
  - QR rendered as SVG
  - matching from bank statements: upload **camt.053** XML or **Fio-style CSV/ABO** in admin, or a periodic fetch from a Fio API adapter (mocked locally). Matches on VS + amount + currency. Partial and over-payments are flagged for manual resolution.
- **COD:** a fee from the shipping method, cash rounding for CZK, the payment is marked paid when the shipment is delivered (carrier webhook/poll) or manually.
- **Refunds:** Stripe via API; bank/COD refunds are recorded manually (the admin shows the IBAN from the withdrawal form).

### 10.5 Shipping
- **Packeta:** pickup point selection via the official widget (checkout, loaded on demand), home delivery, packet creation + label PDF via the Packeta API, tracking status polling. Mocked locally with the same request/response shapes.
- **PPL:** shipment creation + labels + tracking via the PPL CPL API (OAuth client credentials). Mocked locally.
- **Rates:** flat per method + free-over threshold + optional weight tiers per market. COD availability + fee per method.

### 10.6 Orders
- Admin: list with filters, detail with timeline, status actions, manual order creation (staff builds a cart for a customer), order edits before shipping (lines/qty/address → re-price → issues a credit note + new invoice if already invoiced), packing slip PDF, bulk label printing.
- Withdrawal (EU): the customer submits a withdrawal via the account or the order-token link (the legal "withdrawal button" flow) → `withdrawal` record + email confirmation → the merchant receives goods → refund + credit note.

### 10.7 Invoicing (CZ/SK)
- When issued: on payment for prepaid methods, on shipment for COD. A proforma for bank transfer on order placement (optional setting).
- Fields: supplier name, address, IČO/DIČ (IČ DPH for SK), registry entry; customer; invoice number; issue date; taxable supply date (DUZP); due date; VS; bank account; lines with rates; VAT recap per rate; for non-CZK invoices of CZ VAT payers, the VAT recap is also shown in CZK using the ČNB rate of the DUZP date.
- Numbering: `{prefix}{YYYY}{seq:05}` per series; gapless, assigned inside the issuing transaction (row lock on the series).
- PDF: Typst template per locale (cs, sk, en), rendered by the worker, stored in S3, attached to the email.
- Credit notes reference the original invoice. Invoices are immutable.
- CZ EET 2.0 and SK B2B e-invoicing (Jan 2027) are **v2**, but invoice data is kept structured (`lines`, `vat_recap` jsonb) so later ISDOC/e-invoice export is a projection.

### 10.8 Import and export
- **Feed import:** Heureka XML (`SHOP/SHOPITEM`, `ITEMGROUP_ID` → variants, `PARAM` → parameters, `CATEGORYTEXT` → category tree, `IMGURL`/`IMGURL_ALTERNATIVE` → media, `URL` → redirects) and Google Merchant RSS/XML (`g:item_group_id`, `g:product_type`). Streaming parse (`quick-xml`), up to 100k items, idempotent upsert by SKU/ITEM_ID, a dry-run report first.
- Image download: SSRF-safe fetcher (no private/link-local IPs, http(s) only, 20 MB cap, 10 s timeout; the allowlist of `mocks`/fixtures hosts is enabled only in dev).
- **CSV import:** customers and orders (history) with a column-mapping UI, preview, validation report.
- **Redirects:** each imported product's old `URL` path → the new product URL (301).
- **Exports:** Google Merchant (RSS 2.0 `g:`), Heureka, Zboží (Heureka-compatible + Zboží extensions) per market, regenerated hourly + on catalog change (debounced), served at `/feeds/{market}/{google|heureka|zbozi}.xml`.
- **Data export for merchants:** a full tenant export (JSON Lines per table + assets manifest) as a zip job.

---

## 11. Growth (M2 unless marked M1)

### 11.1 Search (M1)
- Meilisearch index `t_{tenant_short}_{locale}` per tenant+locale. Documents = products with:
  - `name`, `brand`, `skus`, `eans`, `description_text`, `category_ids`, `category_path`
  - `params.{key}` (only purchasable combinations are listed per variant group; see below), `price_{market}`, `in_stock`, `popularity`
  - `name_folded`, `name_stems`
- Variant-correct facets: one document per product with a `variants` array of attribute combos; facet filters on combos use nested prefixed keys `combo:{color}|{size}` so "red + XL" only matches if one variant is red and XL.
- **cs/sk normalizer** (`commerce::search::lang`): lowercase, diacritics folding, a light suffix stemmer (a rule list derived from Snowball Czech 3.1 + hand-written Slovak rules), stopwords. Applied at indexing (to `*_stems`) and at query time. Covered by fixture tests (≥ 200 query/expected pairs in `fixtures/search/`).
- Ranking: exact SKU/EAN → exact name → typo-tolerant. Typos are disabled for `skus`/`eans`.
- Index updates via outbox → worker (debounced per product). Full rebuild with an index swap (`POST /admin/v1/search/rebuild`).
- Browsers never talk to Meilisearch. Only the API does, with the master key kept in the API/worker env.

### 11.2 Recommendations (M2)
- Nightly + hourly rollups into `product_stats_daily`, `co_purchases` (orders from the last 90 days, min support 3).
- Strategies: `bestsellers(category?, market)` with time decay; `bought_together(product)`; `seasonal` (merchant-scheduled collections, and month-of-year bestsellers from last year when there's data); `recently_viewed` (kept only in `localStorage` on the device, never sent to the server; rendered by an island from product IDs via the public product endpoint); `personalized` (category/brand affinity from events, **only with the `personalization` consent**).
- Every result is filtered by availability, market and status. It falls back to bestsellers.
- Storefront endpoint `GET /recommendations` + slots in the default theme (PDP "bought together", cart cross-sell, home "for you"/bestsellers), plus newsletter product blocks.

### 11.3 Tracking and analytics (M1 core, M2 ad platforms)
- **Browser:** a tiny beacon (< 2 kB) from the SDK sends `page_view`, `view_item`, `add_to_cart`, `begin_checkout`, `web_vitals` to `/_p/e` → the API. No third-party scripts.
- **Server:** authoritative events (`purchase`, `refund`) from the outbox.
- **Consent purposes:**
  - strictly necessary (always)
  - `analytics`: aggregate analytics with anon id
  - `marketing`: ad platforms
  - `personalization`

  Without `analytics` consent only cookieless aggregate counters are recorded (no anon id, no cross-page linkage).
- **Admin dashboard (M1):** revenue, orders, AOV, sessions, conversion rate, funnel, top products, top searches, zero-result searches, and Web Vitals p75 by template.
- **Ad-platform forwarders (M2):**
  - Meta Conversions API, GA4 Measurement Protocol, Google Ads (offline/enhanced conversions via upload), Sklik (Seznam conversion API)
  - run by the worker, only with `marketing` consent
  - hashed identifiers per each platform's spec
  - dedupe with event IDs; retries; per-tenant credentials encrypted at rest (AES-GCM with a platform key from env)
  - mocked locally

### 11.4 Email
- **Transactional (M1):** order confirmation, payment received, bank transfer instructions + reminders, shipped (+ tracking), delivered, invoice/credit note, magic link, password reset, withdrawal confirmation, watchdog/newsletter confirmations.
- **Templates:**
  - MJML (mrml) + minijinja, localized, tenant-branded (logo, tokens)
  - editable subject/intro text per tenant; the layout is platform-owned
  - plain-text alternative
- **Streams:** `transactional` and `marketing`, with separate SMTP credentials/config sets, separate from-addresses, and `List-Unsubscribe` + `List-Unsubscribe-Post: List-Unsubscribe=One-Click` on marketing.
- **Deliverability:**
  - suppression list (bounces/complaints) checked before every send
  - sending domain setup UI shows the DKIM/SPF/DMARC records to add (prod); locally everything goes to Mailpit

### 11.5 Newsletter (M2)
- Double opt-in with stored consent evidence. Segments are an allowlisted rule builder (locale, market, subscribed since, purchased category/brand, order count, total spent, last order before/after, engaged) compiled to parameterized SQL.
- Campaigns: block editor (heading, text, image, button, product grid, **personalized products** block → per-recipient recommendations with a fallback). Send via jobs in batches of 500 with per-tenant throttle. Click tracking via redirect links. No open tracking by default (privacy; optional setting with consent).
- Copy is generated once per segment (M3 AI assist), not per recipient.

### 11.6 Flows (M2)
- **Abandoned cart:** enrolled when a cart has an email + `marketing` consent, is ≥ 1 h inactive and has ≥ 1 line. Steps default: 1 h reminder, 24 h reminder, 72 h with an optional single-use coupon. Stops on order, unsubscribe, empty cart. A restore link rebuilds the cart.
- **Watchdog:** a double-opt-in watch per variant: back in stock, or price drop below a target / any drop. Triggered by `inventory.changed` / `price.changed` outbox events. Fires once, then closes.
- **Review invites:** N days after delivery (default 7) if the customer checked the reviews consent at checkout. A tokenized review form, verified-purchase flag.

### 11.7 Reviews (M2)
- Moderation queue in admin; publish/reject; merchant reply. Only published reviews are shown. `AggregateRating` JSON-LD.
- Omnibus disclosure: the shop's "how we verify reviews" section is auto-generated into the legal pages (verified = linked to a delivered order line).

---

## 12. AI (M3)

### 12.1 AI gateway (`platform::ai` + `commerce::ai`)
- An Anthropic Messages API client (reqwest, streaming not required for helpers) with retries/timeouts, structured JSON outputs validated with `serde` + JSON Schema.
- Per-tenant monthly token quota + `ai_usage` metering. The admin shows usage.
- A **fake provider** returns deterministic fixtures (used in tests and when no key is configured).
- AI Act transparency: AI-generated fields are flagged (`ai_generated_at` in translations/content) and labelled in admin. The storefront shows no AI chatbot in v1.

### 12.2 Admin helpers
- Product description generate/rewrite (tone presets, length, uses parameters + existing text), SEO title/meta, category descriptions.
- Translate product/category/page/menu content across tenant locales. Glossary per tenant (brand terms not translated).
- **Bulk edit by prompt:** "raise prices of all Nike shoes by 5 % in SK market", "add parameter material=cotton to T-shirts". The LLM returns a **structured change plan** over allowlisted operations (set field, adjust price by %, add/remove category, set parameter) + a target selector. The API resolves the targets, shows a preview diff (count + sample), and the staff confirms → a bulk job. The LLM never writes to the DB directly.
- Everything goes through the Admin API authorization and the audit log.

### 12.3 AI theme editing (approach C)
- **theme-builder** (`apps/theme-builder`, Node, Docker):
  - Workspace per revision: copy of the parent revision source (from S3) with a pre-installed, locked `node_modules` (no registry access; network egress only to the Anthropic API).
  - **Agent loop:**
    - Claude with tools: `list_files`, `read_file`, `write_file`, `delete_file` (restricted to `src/**`, `public/**`, `theme.config.ts`), `run_checks`
    - system prompt = the theme contract (§9.1) + SDK reference + design tokens + budget rules
    - max 25 tool turns, max 3 check-repair cycles
  - **Gates:**
    - `astro check` + `tsc --noEmit`, theme-kit contract lint (no foreign fetch, no new deps, required routes present, no `set:html` on user data except sanitized CMS HTML)
    - `astro build`
    - bundle budget (JS per route)
    - Lighthouse mobile on the preview (home/category/product) against §9.6
    - axe
    - Playwright smoke (browse → add to cart → go to checkout)
    - visual screenshots (stored for the merchant)
  - Output: a bundle tarball + a check report → `theme_revisions.status = ready|failed` (callback to the internal API).
- **Admin UX:** a prompt box + history of revisions, a live preview iframe (`preview-<rev>--<shop>.localhost`, `noindex`, never cached publicly, no real checkout), a changed-files diff, the check report (budget numbers, a11y), **Publish** (atomic pointer switch + edge purge) and **Rollback** (repoint to any previous `ready` revision).
- Merchants can also "reset to default theme" (new revision from the latest default theme + their tokens).
- Safety:
  - theme code never gets secrets
  - bundles run with only the storefront service binding
  - checkout/account are not theme code
  - CSP on theme responses: `script-src 'self'`, `connect-src 'self'`, `frame-src` Packeta/Stripe only on checkout routes

---

## 13. Background processing
- `worker` binary: N job loops (configurable, default 4) `SELECT … FROM jobs WHERE status='queued' AND run_at<=now() ORDER BY run_at FOR UPDATE SKIP LOCKED LIMIT 10`.
  - lease via `locked_until`
  - retries with exponential backoff + jitter, `max_attempts` per kind
  - dead-letter status visible in the admin (superadmin)
- **Outbox dispatcher:** polls `outbox` (and `LISTEN/NOTIFY` wakeup) → fan-out into jobs per subscriber (search, webhooks, email, analytics, flows, cache purge). Idempotency key `outbox_id:handler`.
- **Per-tenant fairness:** at most `max_concurrent_per_tenant` running jobs per tenant (default 2), enforced in the claim query. `ponytail:` simple cap; weighted fair queuing if tenants starve.
- **Cron** (in the worker, one leader via `pg_try_advisory_lock`):
  - every 5 min: ČNB rates, abandoned-cart scan, payment timeouts, feed regeneration (debounced), tracking polling
  - hourly: stats rollups
  - nightly: co-purchases, partition maintenance, session cleanup
- Every handler is idempotent. Email sends record a `sent` row before calling SMTP with a `Message-ID` derived from the idempotency key. Crash duplicates are possible and accepted.

---

## 14. Security and compliance checklist
- **AuthZ everywhere:** tenant from verified credentials only; object-level checks in services; RLS as defense in depth; cross-tenant tests per module.
- **Input:** serde with `deny_unknown_fields` on admin mutations, validated lengths/ranges, 1 MB body limit, upload MIME sniffing (`infer`) + size limits + image re-encode (strips EXIF).
- **Web:** CSRF (SameSite=Lax + Origin check on state-changing cookie-auth routes), CSP per route class, HSTS in prod, no secrets in client bundles, sanitized rich text (`ammonia`) for CMS/product HTML.
- **SSRF:** a single `platform::http::safe_client` for merchant-supplied URLs (imports, webhooks).
- **Secrets:** env only, `.env` git-ignored, per-tenant third-party credentials encrypted at rest.
- **Privacy:**
  - consent records, customer data export + erase (GDPR art. 15/17: anonymize orders, keep invoices per tax law)
  - retention: events 13 months, sessions 30 days
  - IPs hashed with a rotating salt
- **Legal content:** templates (cs/sk/en) for terms, privacy, cookies, withdrawal form, complaints procedure, review verification. The merchant fills in placeholders; the platform flags missing legal entity fields before go-live.
- **Accessibility:** WCAG 2.2 AA target for the default theme, checkout and admin.
- **Logging:** structured, request IDs, no PII/secrets (emails masked).

---

## 15. Local development and operations
- `make up` starts everything via `docker compose up -d --build`: postgres, meilisearch, minio (+ bucket init), mailpit, stripe-mock, mocks, caddy, auth, api, worker, edge, admin (static build served by Caddy), and in M3 theme-builder.
- `make dev-infra` starts only the dependencies. Apps then run natively (`cargo run -p api`, `pnpm --filter admin dev`) against them for fast iteration.
- `make migrate`, `make seed` (demo tenant "Demo Shop" with markets CZ `demo.localhost` + SK `demo-sk.localhost`, 300 products imported from `fixtures/feeds/heureka-demo.xml` with Czech/Slovak names, images from fixtures, shipping and payment methods configured, staff `owner@demo.localhost` / printed password).
- `make test` (Rust + TS unit/integration), `make e2e` (Playwright on the stack), `make perf` (Lighthouse CI + axe), `make lint`, `make fmt`, `make openapi` (regenerate clients).
- **Resource caps:** `CARGO_BUILD_JOBS=6`, Playwright `workers: 4`, Lighthouse runs serially.
- **Observability:** `tracing` JSON logs, `/healthz` (liveness) and `/readyz` (DB + Meili + S3), optional Sentry DSN. The API and worker expose Prometheus `/metrics` (request latency, job lag, queue depth) for prod.
- **Backups (prod runbook only):** provider PITR + nightly logical dump to object storage + a quarterly restore drill. Documented in `docs/runbook.md`.
- **CI (GitHub Actions):** fmt/lint, Rust tests with a Postgres service, TS tests, OpenAPI client drift check, build images. e2e/perf run on `main` and labelled PRs (budget-heavy).

---

## 16. Testing strategy and quality gates
- **TDD for all domain logic:** pricing, VAT, rounding, promotions, Omnibus, SPAYD/PAY by square, invoice numbering, status machines, search normalizer, segment compiler, flows.
- **Integration (`#[sqlx::test]`):** every service against real Postgres with RLS on and the runtime role, including cross-tenant negative tests.
- **API contract tests:** OpenAPI generated + snapshot. Clients compile.
- **E2E (Playwright), each milestone's acceptance suite:**
  - M1:
    - staff login → create product → visible on the storefront → search finds it (with and without diacritics)
    - add to cart → checkout with COD / bank transfer (QR shown, statement import marks it paid) / Stripe fake
    - order in admin → label → shipped email in Mailpit → invoice PDF
    - feeds valid (XML schema checks)
    - import from a Heureka feed with redirects working
  - M2: newsletter double opt-in → campaign → personalized block; abandoned cart email sequence (time-travel via a test clock endpoint in dev); watchdog fires on restock; review invite → review published → rating in JSON-LD; recommendations present; ad forwarders hit the mocks only with consent.
  - M3: AI description/translation with the fake provider; bulk edit preview + apply; theme edit prompt (fake provider returns a scripted patch) → checks → preview → publish → storefront changed → rollback restores it.
- **Perf/a11y:** `make perf` must pass §9.6 for the default theme (M1) and for AI revisions (M3 gate).
- **Definition of done per work package:**
  - tests green, clippy/biome clean, OpenAPI clients regenerated
  - the real flow exercised on the compose stack
  - Astra review findings resolved
  - PR squash-merged

---

## 17. Milestones and work packages (revised after Astra review, supersedes the original list)

Each WP = one branch + PR: an Opus agent implements it in a worktree, Astra reviews, the findings get fixed, then it is squash-merged. Every WP lists prerequisites and must be acceptance-testable at merge time. Security tests belong to the WP that owns the code.

"M1 = local pilot acceptance" (everything verified locally against mocks). Real-provider validation (Stripe live test mode, Packeta/PPL sandboxes, SES, bank scans of QR codes) is a separate pre-launch checklist in `docs/runbook.md`.

### M1
| WP | Content | Prereqs |
|---|---|---|
| WP0 Foundation | workspace, Docker stack, Makefile, config, problem+json, tracing, health, OpenAPI → TS clients, CI | none |
| WP1 Identity, tenancy, jobs | tenants/domains/markets, roles + grants + RLS + `tenant_tx`, membership bootstrap (A8), Better Auth + JWT contract (A9), staff roles, audit log, superadmin CLI, local domain verification stub, outbox + leased jobs + cron leader (A14), idempotency store (A12) | WP0 |
| WP2 Runtime + trust-boundary spike | Astro/adapter/Miniflare/workerd version matrix and artifact contract (A22), edge gateway prototype with the origin split (A1), cache policy (A2), restricted theme binding (A7), a full-island product page measured against the budget (A26), AI-edit feasibility on 10 real prompts against the theme contract. Outputs: `docs/decisions/runtime-contract.md` + reusable edge/SDK skeleton code | WP0 |
| WP3 Catalog + media | products/variants/options/parameters/categories/translations/GPSR/unit price, per-country tax categories (A3), assets (public/private split, A21) + image variants job, Admin API CRUD | WP1 |
| WP4 Money, tax, pricing, promotions, inventory | money, VAT liability config (A3), pricing algorithm (A15), sales/coupons, effective-price intervals + Omnibus (A18), stock movements (A13), cash rounding (A16). Pure domain + tables + Admin API | WP3 |
| WP5 Admin shell + catalog UI | Solid SPA, login, tenant switcher, i18n, catalog/media/prices/inventory/tax screens | WP3 (WP4 API for price screens) |
| WP6 Storefront runtime | production-quality edge gateway (origin split, cache allowlist, header stripping), Storefront API page models + authorization matrix (A4), SDK, checkout-origin app skeleton, one shared default artifact (A30), SEO basics, redirects | WP2, WP3, WP4 |
| WP7 Search | per-variant documents (A23), cs/sk normalizer + fixtures, facets, search/category/typeahead endpoints, degraded-mode readiness (A30) | WP3, WP4 |
| WP8 Default theme | the full polished design (frontend-design), mini-cart island via the cart capability, search UI, GPSR display, Omnibus labels, consent banner, RUM beacon, JSON-LD, perf + a11y gate green (`make perf`) | WP6, WP7 |
| WP9 Customers, consent, mail core | customer accounts on the checkout origin (A5), sessions, consent model (A20), order/payment/fulfillment state machines (A13), email infra with send states + suppression (A14, A29), transactional templates skeleton | WP6 |
| WP10 Checkout + order placement | one-page checkout on the checkout origin, cart handoff (A1), shipping method selection (Packeta widget, PPL, home), payment method selection, idempotent place-order, stock reservation, fake payment adapter, order confirmation | WP9, WP4 |
| WP11 Payment adapters | payment attempts + provider events (A10, A11), Stripe Connect direct charges, bank transfer (SPAYD, PAY by square 1.2.0 with golden vectors, A25) + statement import/matching, COD tender/collector/remittance (A16), payment timeouts + late-payment exceptions | WP10 |
| WP12 Fulfillment, invoicing, returns | admin orders UI, labels (Packeta/PPL mocks), shipment → stock commit, invoice scenarios (A17) with Typst, credit notes, ČNB rates, refunds, withdrawal flow (A19), packing slips | WP11 |
| WP13 Content, import, feeds, privacy | CMS pages/blog/menus, legal templates + go-live validation, redirects admin, feed import with mappings (A28), CSV import, channel serializers (Google/Heureka/Zboží), tenant data export, customer access/erasure with retention exceptions (A29) | WP3, WP12 |
| WP14 Analytics, webhooks, ops | server counters + consented events (A20), rollups, dashboard, Web Vitals RUM, outbound webhooks (SSRF-safe, A21), `/metrics`, local backup + restore drill (A29), runbook incl. real-provider checklist | WP10 |
| WP15 M1 acceptance | full M1 e2e suite, perf/a11y gates, manual keyboard checks (A26), seed polish, README/runbook | all M1 |

Parallelizable (disjoint files): WP2 ∥ WP1; WP5 ∥ WP4; WP7 ∥ WP6 (after both prereqs); WP13 ∥ WP14.

### M2
| WP | Content | Prereqs |
|---|---|---|
| WP16 Reviews | review storage, review tokens, moderation, verified flag, JSON-LD, Omnibus disclosure, theme integration | WP12 |
| WP17 Recommendations | rollups, strategies, storefront slots, consent-gated personalization | WP14 |
| WP18 Email marketing | subscribers + double opt-in, segments, campaigns + block editor incl. personalized product block, one-click unsubscribe | WP17, WP9 |
| WP19 Flows | flow engine, abandoned cart, watchdog, review invites, dev test clock | WP16, WP18 |
| WP20 Ad-platform forwarders | Meta CAPI, GA4 MP, Google Ads, Sklik with `ads` consent + mocks + settings | WP14 |
| WP21 M2 acceptance | M2 e2e suite, perf re-check | all M2 |

### M3
| WP | Content | Prereqs |
|---|---|---|
| WP22 AI gateway + admin helpers | Anthropic client + fake, quotas/metering, descriptions/SEO/translations, bulk-edit change plans + preview + apply, AI labels | WP5 |
| WP23 Theme builder pipeline | sandboxed per-revision builds (A6), revisions, gates, preview authorization (A21), publish/rollback/reset | WP8 |
| WP24 AI theme editing | agent loop (outside the sandbox), prompt UX, diff, check report | WP23, WP22 |
| WP25 M3 acceptance | M3 e2e suite, final security review, docs | all |

## 18. Risks and mitigations
1. **Miniflare can't faithfully host Astro Worker bundles** → WP4 validates it first; fallback: workerd config generation.
2. **Theme bundles exceed the JS budget once islands grow** → CI budget in WP4 from day one; islands audited in review.
3. **AI theme edits fail the gates often** → the repair loop (3 cycles), a narrow contract, a well-structured default theme; the metric (pass rate) is logged per revision.
4. **CZ/SK search quality** → normalizer fixtures; Meilisearch settings versioned; zero-result search report in the dashboard.
5. **Scope/time** → WPs are vertical slices; M1 remains shippable without M2/M3.
6. **Legal details (EET 2.0, SK e-invoicing, exact consent rules)** are v2 or need legal review; the data kept structured.

## 19. Deferred (explicitly out of M1–M3)
B2B (price lists per customer, net terms, VIES, company accounts, quick order), AI chat assistant, MCP/ACP/UCP/AP2, auth.md, Comgate/GoPay, more carriers, Ecomail/Fakturoid/Pohoda integrations, Heureka Ověřeno zákazníky, Shopify/Woo API importers, EET 2.0, SK e-invoicing, self-signup + billing automation, social login, extra themes, ML recommendations, A/B testing, gift cards, loyalty, digital/subscription products, multiple warehouses, app marketplace/WASM, production provisioning.

---

## 20. Amendments after Astra review (binding; override conflicting text in §1–§16)

Numbering A1–A30 matches the review findings (`docs/research/spec-review-astra.md`).

**A1: Origin split.**
- Theme code (untrusted) runs on the shop origin (`demo.localhost`, prod `shop.cz`).
- Checkout, account, withdrawal and consent preferences run on a separate **checkout origin** `checkout.<shop-host>` (`checkout.demo.localhost`), served by the platform-owned checkout app.
- The customer session cookie (`sid`) exists only on the checkout origin (host-only).
- The shop origin holds only the `cart` capability cookie (HttpOnly, host-only, `Path=/_p/cart`).
- **Checkout handoff:**
  - `POST /_p/checkout/start` on the shop origin (edge-owned) mints a single-use handoff token (60 s, hashed at rest)
  - it 303-redirects to `checkout.<host>/start?h=<token>`
  - that endpoint exchanges the token for a checkout-origin cart cookie and redirects to `/`
- No credentialed CORS between origins. Until the WP23 sandbox exists, only platform-reviewed theme code is deployed (true for M1/M2: only the default theme exists).

**A2: Cache policy (edge-owned, themes can't override it upward).**
- Cache only `GET`/`HEAD` of an explicit allowlist: theme page routes (`/`, `/c/*`, `/p/*`, `/pages/*`, `/blog*`, `/search` without personalization) and immutable assets.
- Never cache:
  - anything on the checkout origin, `/_p/*`, previews
  - URLs containing capability tokens (`token`, `h`, `sig` query params)
  - responses with `Set-Cookie`, `Cache-Control: private|no-store`, or requests with `Authorization`
- The edge strips client-supplied `X-Tenant`, `X-Market`, `X-Storefront-*`, `X-Forwarded-*` before resolving.

**A3: VAT liability.**
- The tenant tax profile holds:
  - `establishment_country`, `vat_payer` bool, `vat_id` (DIČ), `sk_ic_dph` (separate field)
  - `distance_sales_mode`: `origin_threshold` (the merchant confirms eligibility for the EU €10k exception; stored with the confirmation timestamp) or `destination` (OSS or local registration)
- Each market lists allowed ship-to countries. Checkout blocks countries not covered by the profile.
- Tax categories are per country: `tax_categories(country, code, rate, valid_from)`. Products map to a category per country (`product_tax_categories`), defaulting to `standard`.
- A non-VAT-payer charges no VAT and issues invoices without VAT.
- Gross prices stay constant across the destination rate (unchanged policy).
- The docs flag that the setup must be confirmed by the merchant's accountant.

**A4: Storefront authorization matrix.**
- Browsers never call `api.localhost` directly. Only through the edge gateway on the same origin.

| Operation | Where | Credential |
|---|---|---|
| Public catalog/page reads | theme SSR via the restricted binding; islands via `/_p/public/*` | tenant context injected by the edge |
| Cart read/write | shop origin `/_p/cart/*` | `cart` capability cookie (opaque, 256-bit, hashed at rest, rotated on checkout handoff) |
| Checkout, account, withdrawal | checkout origin | checkout cart cookie + `sid` session |
| Order status page | checkout origin `/o/<token>` | order capability token (read-only, 90 days) |

- Cart → account merge happens on login on the checkout origin (the cart is attached to the customer; lines merged by variant).

**A5: Customer credentials.**
- Setting or changing a password requires a verified-email magic link consumed within the last 10 minutes, or the current password.
- Password reset = magic link. Tokens are single-use, consumed atomically (`UPDATE … WHERE used_at IS NULL RETURNING`).
- A password change revokes all other sessions.
- Guest orders are linked to an account only after that email is verified via a magic link.
- Redirect targets after login: relative paths on the checkout origin only.

**A6: Theme build sandbox (WP23).**
- Each build runs as a disposable container (`docker run --network none --read-only`, a writable tmpfs work dir, read-only `node_modules` volume, `--pids-limit`, CPU/memory/time limits, no credentials, non-root).
- The AI controller runs outside the sandbox and only exchanges files.
- Archives are validated (no absolute paths, no `..`, no symlinks, expanded size ≤ 50 MB).
- Design tokens are schema-validated `theme.tokens.json`, not TS.

**A7: Restricted theme binding.**
- Theme workers get exactly one binding, `STOREFRONT`: a platform wrapper worker that exposes only the public page-model/catalog operations. It injects the immutable tenant/market and rejects any request carrying credentials.
- Global outbound fetch is denied.
- The checkout app has a separate binding with checkout capabilities.
- The edge purge endpoint and builder callbacks use distinct service tokens.

**A8: RLS bootstrap and queues.**
- Membership lookup uses a `SECURITY DEFINER` function `platform.staff_membership(user_id, tenant_id) returns role` owned by `app_owner`. No business access happens before it succeeds.
- `outbox` and `jobs` live in schema `queue`, with no RLS. They're accessible only via `SECURITY DEFINER` functions (`queue.enqueue`, `queue.claim`, `queue.complete`, `queue.fail`, `queue.heartbeat`) granted to `app_runtime`.
- Handlers then run tenant work inside `tenant_tx`.
- Code review rule: tenant tables are never queried outside `tenant_tx`. The runtime role without context fails closed.
- Tests cover connection reuse after commit, rollback and task cancellation.

**A9: Staff JWT contract.**
- Better Auth JWT plugin: `iss=http://auth.localhost`, `aud=admin-api`, `exp=5m`, EdDSA (Ed25519). The JWKS is fetched by Rust with a 10-minute cache and a forced refresh on unknown `kid` (at most once per 30 s).
- The JWT lives in SPA memory only.
- JWTs are issued only after email verification, and after 2FA if the user enabled it.
- Rust re-checks membership on every request.
- Sensitive operations (staff management, payment/tax settings, exports, theme publish) require `auth_time` within 15 minutes, otherwise `401 reauth_required`.
- CORS: `admin.localhost` only, with credentials, for the auth origin.

**A10: Payment attempts.**
- `place-order` commits the order + a `payment_attempts` row + an outbox event.
- Right after commit, the API creates the provider intent with idempotency key = attempt ID, stores `provider_ref`/`client_secret_ref`, and returns it.
- If that fails, the client retries via `POST /checkout/orders/{id}/payment-attempts/{attempt}/init` (idempotent). A failed payment → a new attempt on the same order.
- Status endpoint: `GET /checkout/orders/{id}/payment`.
- A payment succeeding after timeout/cancellation → an order `exception` flag + a refund task. Stock is never silently restored.

**A11: Stripe webhooks.**
- The raw event is persisted in `provider_events(provider, event_id unique, account, payload, processed_at)` before the 200.
- It's processed asynchronously. The event must match the tenant's connected account, livemode flag, provider object, currency and expected amount.
- Only `payment_intent.succeeded` confirms. Out-of-order events never regress `paid`.
- Refunds via API with an idempotency key.
- Application fee refund policy: refund the fee proportionally (`refund_application_fee=true`).
- `account.updated` capability loss disables Stripe at checkout.

**A12: Idempotency.**
- `idempotency_keys(tenant_id, operation, key, request_hash, response jsonb, status, created_at)`, unique `(tenant_id, operation, key)`. The same key with a different hash → `409 idempotency_conflict`. Retention 24 h.
- One order per cart: unique `orders.cart_id`.
- Place-order locks the cart row (`FOR UPDATE`), checks the cart `version`, stock rows, coupon usage and refundable balance, all in one transaction.

**A13: Inventory and lifecycle.**
- `stock_movements(variant_id, quantity, kind reserve|release|commit|restock|adjust, ref_type, ref_id)` with a unique `(kind, ref_type, ref_id, variant_id)`.
- `inventory_levels` is updated in the same transaction.
- Reservation on placement; release on cancel/expiry; commit on shipment; restock on return receipt (merchant-confirmed).
- COD orders are `confirmed` on placement.
- Returns are per line and quantity (`return_lines`); the order status derives from the line states (no terminal `partially_returned`).
- M1: no financial edits after payment. Use cancel/refund + a replacement order. Non-financial edits (address before label, notes) are allowed.

**A14: Jobs and email delivery.**
- Claim sets `lease_owner`, `lease_token`, `locked_until` (60 s). The claim query also reclaims `running` jobs with expired leases.
- `heartbeat` extends the lease. `complete`/`fail` require a matching `lease_token` (fencing).
- The outbox dispatcher inserts fan-out jobs and sets `dispatched_at` in the same transaction.
- Email sends: `email_messages(status pending|sending|accepted|uncertain|failed)`, set to `accepted` only after the SMTP 250. A crash while `sending` → `uncertain` → retried once for transactional mail (duplicates tolerated), never for marketing.

**A15: Pricing algorithm (ordered, persisted).**
1. Line base gross = effective unit price × qty.
2. Automatic sales are already in the effective price.
3. Coupon discounts are allocated to eligible lines proportionally to line gross, using the largest-remainder method in minor units.
4. VAT per line on the discounted line gross: `round_half_up(gross × r/(100+r))`.
5. Shipping and payment fees are ancillary: their gross is split across the VAT rates of the goods proportionally to the goods' discounted gross per rate (largest remainder), and VAT is computed per portion.
6. Cash rounding is applied last as a separate line. Tax treatment is per jurisdiction config, default outside the VAT base (flagged for accountant confirmation).

- The allocation results are persisted on `order_lines`/`order_charges`.
- Refunds reverse the original allocations proportionally. The residual goes to the last line. Historical orders are never re-priced.

**A16: COD.**
- `payments` records `tender` (cash|card|unknown) and `collector` (carrier|merchant).
- Cash rounding applies only when tender = cash:
  - CZK to whole koruna
  - EUR (SK) to €0.05 per SK rules
- States: `delivered` → `collected` (carrier report or manual) → `remitted` (carrier payout import or manual confirmation with an audit entry).

**A17: Invoice scenarios (VAT payer, CZ/SK).**
1. Prepaid (card or bank transfer): payment received before dispatch → one tax document on the payment date (DUZP = payment date) that also serves as the final invoice. If goods are not shipped, a credit note.
2. COD: invoice on dispatch (DUZP = dispatch date).
3. Non-VAT-payer: invoice without VAT, no recap.
4. Partial return/cancellation: a credit note referencing the original allocations.

- Proformas are not tax documents.
- ČNB rate: the latest published fixing on or before the DUZP (weekends/holidays → the previous business day). If unavailable → issuing is retried and the admin warns.
- SK uses DIČ and IČ DPH as separate fields.
- Document currency and the statutory CZK recap are stored separately.
- The docs state "templates require accountant approval before real use".

**A18: Omnibus.**
- `price_intervals(variant_id, price_list_id, amount_minor, valid_from, valid_to, cause base|sale|tax)` materialized, including **future scheduled** sale start/end (written when a sale is created or changed).
- The reference price = the minimum over the intervals overlapping `[reduction_start − 30 d, reduction_start)`.
- A progressive (chained) reduction keeps the reference from before the first reduction in the chain.
- A product younger than 30 days → the minimum since launch.
- The storefront discount % and strikethrough are computed only against the reference price. `compare_at` is never shown as a reduction basis.
- Imported products without 30 days of history show no reduction claims until history exists.
- Coupons available to all customers (published codes) count as price reductions for the reference.

**A19: Withdrawal.**
- A public page `checkout.<host>/withdraw`, linked from the footer, order emails and the account: order number + email → an emailed confirmation link (proves control) → shows the lines → an explicit "Confirm withdrawal" step → an immediate durable receipt (email containing the full declaration, stored as a document).
- Tracked separately: `delivered_at`, `declared_at`, `goods_received_at` / `return_proof_at`, `refund_due_at` (declared + 14 d).
- The refund may wait for goods or proof of dispatch.
- The refund covers the standard outbound shipping and uses the original payment method (bank refunds use the IBAN from the form).

**A20: Consent.**
- Purposes: `analytics`, `ads`, `personalization`, `email_marketing`, `review_invites`. Recorded per tenant (controller) and subject.
- The server resolves consent from `consent_records` at execution time (sending, forwarding, enrollment steps). Beacon-supplied purposes are never trusted.
- Before consent: only server-side minimized counters (the edge counts page requests per route template/day, without identifiers). No device storage.
- "Recently viewed" requires `personalization`.
- Dashboard funnel metrics are labelled "consented sessions".

**A21: SSRF and artifacts.**
- The `safe_client`:
  - resolves DNS itself and connects only to public unicast IPs (v4+v6)
  - re-validates every redirect (max 3)
  - strips credentials/cookies
  - caps response size and decompression (20 MB)
  - 10 s timeout
- Buckets:
  - `public` holds only re-encoded media
  - `private` holds invoices, labels, exports, theme sources/bundles, and is served via short-lived (5 min) presigned URLs after authorization
- Preview access is an HMAC-signed token bound to tenant + revision + expiry (1 h). The admin iframe is `sandbox="allow-scripts allow-same-origin allow-forms"` on the preview origin only.

**A22: Runtime contract first.** WP2 pins the Astro / `@astrojs/cloudflare` / Miniflare / workerd versions, and documents the module manifest, compatibility flags and asset binding. Assets are served content-addressed under `/_astro/` (theme) and are retained across revisions. Tested: restart, eviction, publish, rollback.

**A23: Search documents.**
- One Meilisearch document per sellable variant (`product_id`, variant options as facet fields, `price_<market>`, `in_stock`, `active_in_markets`). `distinctAttribute = product_id` at query time.
- Facet counts are **not displayed** in M1. Facet values with zero matches are shown disabled.
- Results are rehydrated from Postgres (current price/stock) before responding.
- WP5/WP7 adversarial fixtures: cross-variant false matches, three options, unavailable variants, market prices, multi-select filters.

**A25: Bank transfer.**
- PAY by square is **v1.2.0**, implemented per the official spec (field order, CRC32, LZMA raw params, header, base32hex). Golden vectors come from the reference `bysquare` npm library (generated once, committed), plus a manual bank-app scan item on the pre-launch checklist.
- SPAYD: escaping per the spec (`*` → `%2A`), `ACC` IBAN(+BIC).
- VS: numeric order number ≤ 10 digits, unique per tenant and receiving account.
- Statement lines are identified by bank transaction ID (duplicate imports are ignored).
- Matching is scoped to tenant + receiving account.

**A26: Performance measurement.**
- "JS budget" = all executable JS transferred until network idle, **plus** the deferred islands triggered by scrolling the full page (measured by a Playwright script).
- Speculation rules are delivered via the `Speculation-Rules` response header (external JSON), so there's no inline script.
- CSP uses hashes for any unavoidable inline script.
- Lab TBT and field INP are reported separately.
- Manual keyboard/focus checks of checkout (including pickup-point and payment selection) are part of WP15.

**A27: Meilisearch ops.**
- Index names use the full tenant UUID (`t_<uuid>_<locale>`). The image is pinned by digest.
- Separate keys: the search-only key for the API query path, the admin key for the worker.
- Indexing events carry a version; stale versions are dropped.
- Readiness: search is reported as a degraded component, not a core-readiness failure.

**A28: Feeds.**
- Separate serializers per channel (Google, Heureka, Zboží) with semantic fixtures.
- Imports use `import_mappings(tenant_id, source, external_id, entity_type, entity_id)`, default to `draft`, report missing fields, never invent price history.
- Redirect collisions are reported (first wins).
- Imported historical orders are `archived` and never trigger payments, stock movements or emails.

**A29: M1 acceptance owners.**
- GPSR display (WP8)
- Legal-content go-live validation (WP13)
- Customer access/erasure (WP13)
- Transactional suppression (WP9)
- Domain verification stub (WP1)
- Local backup + restore drill (WP14)

**A30: Simplify M1.**
- One shared immutable default-theme artifact. Tenants point to it via a revision row until their code diverges in M3.
- No Miniflare instance LRU beyond "one instance per distinct artifact".
- Per-tenant job fairness is deferred: a simple global queue, `ponytail:` noted.
- Search is out of core readiness.
