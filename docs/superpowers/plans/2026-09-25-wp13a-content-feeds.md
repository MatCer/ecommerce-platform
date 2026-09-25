# WP13a Content, legal, feed import/export: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** CMS pages, blog posts and menus with a typed block set; platform legal templates
(cs/sk/en) with a go-live validation (A29); Heureka/Google feed import with a dry run,
mappings and redirects (A28, A18, A21); per-market Google/Heureka/Zboží export feeds (A28);
search fallback indexing and tenant synonyms; edge purges on catalog/content changes (A2).
WP13b (CSV order/customer import, tenant export, GDPR access/erasure) is out of scope.

**Architecture:** business logic in `commerce::{content, legal, feeds, search}`; the SSRF-safe
HTTP client in `platform::http` (A21); edge purges in `platform::edge` (shared by API and
worker); HTTP in `api::{admin_content, admin_feeds}` + storefront page models; worker jobs
`feeds.import`, `feeds.export`, `search.synonyms`, `edge.purge`; admin screens in
`apps/admin`; block rendering in `themes/default`.

## Global constraints

- Every tenant table: `tenant_id`, RLS + `FORCE`, composite FKs, a cross-tenant test.
- Rich text is sanitized with `ammonia` on write (`catalog::sanitize_html`); block links are
  same-shop paths or `https:` URLs only.
- Merchant URLs (feed URLs, image URLs from feeds) are fetched only through
  `platform::http::SafeClient`: own DNS resolution, public unicast IPs only, every redirect
  re-validated (max 3), no credentials/cookies, size + decompression cap, timeout. A dev-only
  host allowlist (`SAFE_FETCH_ALLOW_HOSTS`, e.g. `mocks`) lets fixtures be served locally.
- Imports: `import_mappings(tenant, source, external_id, entity_type, entity_id)`, new
  products are `draft`, prices go in with `imported: true` (no reduction claims for 30 days,
  A18), `compare_at` is never set from a feed, redirect collisions are reported and the first
  wins, images go through the media pipeline (re-encoded, A21).
- Feed exports: one serializer per channel, only active products with a price in the
  market's price list; a reduction is exported only when Omnibus allows a claim (A18).
- Big feeds never pass through the 1 MB API body limit: uploads use a presigned PUT into the
  private bucket, like media.

## Review focus

- SSRF: DNS rebinding (connect to the resolved, validated address), redirects to private IPs,
  decompression bombs, userinfo in URLs, non-http(s) schemes, IPv4-mapped IPv6.
- XML parsing: no DTD/entity expansion (quick-xml does not expand external entities), item and
  field caps, malformed input fails with a report instead of a panic.
- Import idempotency: a retried apply job resumes without duplicates (mappings + SKU lookup).
- Content: stored XSS through blocks (rich text, button hrefs, menu URLs, FAQ answers).
- Legal pages: templates clearly marked "not legal advice"; go-live checks cannot be bypassed.

## Tasks

1. **Migration** `20260930000000_content_feeds.sql`: `pages`, `page_translations`, `menus`,
   `legal_entities`, `import_runs`, `import_mappings`, `feed_exports`, `search_synonyms`;
   RLS + grants.
2. **platform::http::SafeClient** (A21): `fetch(url, limits) -> Bytes`; tests for blocked
   IPs/schemes/redirects/size caps against a local server with/without the allowlist.
3. **platform::edge::EdgePurge** (moved from `api::edge`) + `tags()`; worker `edge.purge`
   subscriber for `product.*`, `price.changed`, `inventory.changed`, `category.*`,
   `page.*`, `menu.updated`.
4. **commerce::content**: blocks (`heading`, `rich_text`, `image`, `button`,
   `product_grid`, `faq`) with validation/sanitizing; pages/blog CRUD (Admin API), menus
   (Admin API); storefront models `/pages/cms/{slug}`, `/pages/blog`, `/pages/blog/{slug}`;
   `/shop` menus + legal links from published pages. Cross-tenant test.
5. **commerce::legal**: markdown templates cs/sk/en (terms, privacy, cookies, withdrawal
   form + instructions, complaints, review verification) with placeholders from the legal
   entity + tax profile, a tiny markdown→blocks converter, `install` (draft pages), the legal
   entity Admin API and `GET /admin/v1/go-live` (legal entity fields, tax profile, published
   required legal pages per market locale, active products without GPSR manufacturer).
6. **commerce::feeds::parse**: streaming `quick-xml` readers for Heureka and Google into
   `FeedItem`s, caps, money parsing; fixtures `fixtures/feeds/heureka-demo.xml`,
   `google-demo.xml` (≥100 items, cs/sk names, variants, params, images on `mocks`).
7. **commerce::feeds::import**: runs (upload via presigned PUT or URL), dry-run report
   (counts, missing fields, collisions), apply (categories, parameters, products/variants,
   images, prices, stock, redirects, mappings) in batches with progress; worker job; Admin
   API. Tests: dry run on fixtures, apply idempotency, redirect collision first-wins.
8. **commerce::feeds::export**: Google/Heureka/Zboží serializers with semantic fixture tests
   (`fixtures/feeds/export/*.xml`); worker regeneration (hourly cron + debounced on
   catalog/price/stock events); stored in the private bucket; served at
   `/storefront/v1/files/feeds/{market}/{channel}.xml` (edge passthrough exists).
9. **Search**: fallback documents for locales without a translation (market default locale
   text); `search_synonyms` Admin API + worker job applying normalized synonyms to every
   index.
10. **OpenAPI + clients**, SDK page-model types, theme block rendering (`Blocks.astro`),
    menus in header/footer.
11. **Admin UI**: pages/blog editor with a block list, menus editor, legal entity + templates
    + go-live checklist, imports (upload/URL, dry-run report, apply, history), feeds status,
    search synonyms. Accessible (labels, keyboard, focus, states).
12. **Demo seed + e2e**: seed legal entity, legal pages, a blog post and menus; Playwright
    `e2e/admin/content.spec.ts`, `e2e/storefront/content.spec.ts`; `make lint test perf`,
    `scripts/smoke-images.sh`.
