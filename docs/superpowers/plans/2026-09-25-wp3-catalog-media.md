# WP3 Catalog + media: implementation plan

> **For agentic workers:** execute task by task with TDD; commit after every task.

**Goal:** products (variants, options, translations, GPSR, unit price data, parameters, media,
per-country tax categories), a category tree, and image assets uploaded through presigned URLs
and re-encoded by the worker into responsive AVIF/WebP/JPEG|PNG variants in the public bucket,
all behind the Admin API with idempotent creates, audit log and outbox events.

**Spec:** §6 (catalog, media), §7.1, §7.5 (assets), §8.1, §8.3, §14 and amendments A3 (tax
categories per country), A21 (public/private buckets, presigned URLs). A18 is WP4.

## Global Constraints

- Every new `public` table: `tenant_id`, `tenant_isolation` policy for `app_runtime`,
  `ENABLE` + `FORCE ROW LEVEL SECURITY`, composite FKs `(tenant_id, id)` so no row can point
  at another tenant's row. The WP1 catalog guard test enforces this.
- Tax categories are platform reference data (law, not tenant data): `platform.tax_categories`,
  read-only for `app_runtime`. The product -> category mapping is per tenant.
- Business logic in `crates/commerce` (`catalog`, `media`); `api` only parses, authorizes,
  and wraps calls in `tenant_tx` + idempotency; the worker only runs `commerce::media::process`.
- Every mutation writes `audit_log` and publishes an outbox event in the same transaction.
- Staff role is enough for all catalog endpoints ("staff may edit catalog").
- `deny_unknown_fields` on every input; lengths and ranges validated in pure functions with
  unit tests; description HTML sanitized with `ammonia` before storage.

## Design decisions

- **Product document.** `POST /products` creates and `PUT /products/{id}` replaces one
  document: attributes + translations + options + variants + category ids + media + parameter
  values + tax categories. One validation function, one save function, before/after audit diff.
  Variants are upserted by `id` (prices and stock reference them later); every other child
  collection is replaced. Array order = position (this is how media are ordered).
- **Options by code.** `options: [{code, name_i18n, values: [{code, name_i18n}]}]`,
  `variant.option_values: {"color": "red", "size": "m"}` (spec `option_values jsonb`). Each
  variant has exactly one value per option; combinations are unique per product (unique index
  on the jsonb). Readable in search facets and imports.
- **Default variant.** Exactly one when variants exist: the flagged one, else the first.
- **Categories.** Tree via `parent_id` with composite FK; moves take a per-tenant advisory
  lock and reject cycles with a recursive CTE; siblings ordered by `position` (renumbered).
- **Assets.** `pending` (upload URL issued) -> `processing` (verified on complete) ->
  `ready` | `failed`. Original stays in `private` under `uploads/<tenant>/<asset>`; variants go
  to `public` under `media/<tenant>/<sha256-of-bytes>.<ext>` (content-addressed, immutable).
  Verification on complete: size <= 20 MB, `infer` sniff in {jpeg, png, webp, gif}, header
  dimensions <= 12 000 px per side and <= 50 MP. Worker decodes with `image` limits, applies
  EXIF orientation, re-encodes (drops all metadata) to widths 160..1920 (never upscaled).
- **Crates.** `image` 0.25 (pure Rust decoders, JPEG/PNG encoders, AVIF via `ravif`/rav1e),
  `webp` 0.3 (libwebp; `image` only encodes lossless WebP), `infer` 0.22, `ammonia` 4.
  CPU: one media job at a time per worker (semaphore), rav1e single-threaded at speed 8.

## Review Focus

- RLS + composite FKs on every table; cross-tenant tests for each.
- Presigned URL scope (private bucket, one key, 15 min), verification before processing,
  decompression-bomb limits, EXIF stripping.
- Product document validation (option/variant consistency, EAN checksum, slugs, GPSR).
- Category move cycle prevention under concurrency.

## Tasks

### 1. Migration: catalog + tax categories + assets
`migrations/20260925200000_catalog.sql`: `products`, `product_translations`,
`product_options`, `variants`, `parameters`, `product_parameter_values`, `categories`,
`category_translations`, `product_categories`, `assets`, `product_media`,
`product_tax_categories`, `platform.tax_categories` (+ seed, source in comment).
Tests: WP1 guard passes; `crates/commerce/tests/catalog_rls.rs` cross-tenant read/write denied.

### 2. `commerce::catalog` validation (pure)
`ProductInput::validate()`, `ean_valid`, `slug_valid`, `I18n`, `Gpsr`, `sanitize_html`.
Unit tests for every rule.

### 3. `commerce::catalog` services
```rust
products::create(tx, actor, &ProductInput) -> Result<Product, Error>
products::replace(tx, actor, id, &ProductInput) -> Result<Product, Error>
products::get(tx, id) -> Result<Product, Error>
products::list(tx, &ProductFilter, cursor, limit) -> Result<ProductPage, Error>
products::delete(tx, actor, id) -> Result<(), Error>
categories::{create, update, move_to, delete, get, tree}
parameters::{create, update, delete, get, list}
tax::list(tx, country: Option<&str>, at: NaiveDate)
```
Integration tests (`#[sqlx::test]`, runtime role): round trip, SKU conflict, slug conflict,
variant upsert keeps ids, category cycle rejected, events + audit written.

### 4. `commerce::media`
`create_upload`, `complete` (verify), `process` (worker), `get`, `list`, `delete`;
`platform::storage` gains a presigner for the private bucket and the public media base URL.
Tests: verification rejects non-images/oversize/huge dimensions; process produces variants
in the public store, strips EXIF, sets `ready`, publishes `asset.ready`.

### 5. Admin API
`crates/api/src/admin_catalog.rs`, `admin_media.rs`; shared idempotent-create helper.
Tests (`crates/api/tests/catalog.rs`): CRUD, filters + cursor, idempotent replay, 404 across
tenants, audit entries.

### 6. Worker handler, config, compose
`media.process` job; worker gets `Storage`; compose env (`S3_PUBLIC_ENDPOINT`,
`MEDIA_BASE_URL`).

### 7. Testkit fixtures, OpenAPI + TS clients, smoke script
`testkit::catalog` builders; `make openapi`; `scripts/smoke-catalog.sh` runs the full flow
through Caddy + MinIO.
