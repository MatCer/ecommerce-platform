# WP17 Recommendations: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** real recommendations (spec §11.2, §7.6, A20, A2, A4, A23): rollups of product stats
and co-purchases, a decayed popularity score that feeds search (closes the WP7 placeholder),
merchant collections, five strategies with visibility filtering and a fallback chain, a public
and a private storefront variant, theme slots (PDP bought together, home bestsellers/for you,
cart drawer cross-sell, recently viewed with live prices) and admin settings + a staff "why
recommended" view.

**Architecture:**
- Migration `20261006000000_recommendations.sql`: `product_stats_daily` (per market),
  `co_purchases` (symmetric, support ≥ 3), `product_scores` (decayed sales score and
  popularity per market), `product_popularity` (the value in the search documents),
  `collections` (manual | seasonal with a schedule window), `customer_affinity`,
  `recommendation_settings`. All tenant-owned: RLS + FORCE + policy + grants.
- `commerce::recommendations`:
  - `rollup`: stats for a day range (consented `view_item`/`add_to_cart` events + placed,
    non-cancelled orders, authoritative), co-purchases (orders of the last 90 days, distinct
    orders per pair, min support 3), scores (half-life 14 days over 90 days), popularity for
    search (debounced: stored only when it moves by > 10 % or ≥ 1 → reindex job per changed
    product), customer affinity (customers whose current `personalization` consent is granted,
    from their orders).
  - `collections`: CRUD + validation (Admin API, audit).
  - `settings`: per-tenant toggles + excluded products.
  - `strategies`: `Target` (product/category/home/cart/collection/recent), chain per target,
    candidate generators (bought_together, bestsellers(category?), seasonal, personalized,
    recently_viewed, newest), one pipeline that filters (excluded, current/cart, duplicates,
    not sold in market, out of stock) and explains itself (`Explained`) for the debug view.
- Worker: `recommendations.rollup` hourly (2 days of stats; 14 days at the 03:00 UTC slot;
  400 days when the tenant has no stats yet or the job asks for a backfill), then the search
  reindex jobs for changed popularity in a second transaction (versioned, A27).
- Search documents: `popularity` from `product_popularity`.
- Storefront API `GET /recommendations?context=&limit=&ids=`: public (cacheable hints) unless
  the edge forwarded a cart capability or consent subject (then `Cache-Control: private,
  no-store`, `cache.public = false`). Personalization and recently viewed only when
  `consent_records` grant `personalization` to the subject at request time (A20).
- Edge: `/_p/recommendations` (shop origin, GET, also under a locale prefix) forwards the cart
  cookie and consent subject; never cached. SSR (`STOREFRONT` binding) and `/_p/public/*`
  never carry them, so they can only get the public variant.
- Theme: PDP slot (SSR, public, title by strategy), home featured = recommendations (page
  model), `ForYou` gate island on home (loads only with personalization consent), cart drawer
  cross-sell (drawer is already lazy), recently viewed stores ids and rehydrates prices.
- Admin: Collections page (create, pick products, schedule, preview), Recommendations page
  (strategy toggles, exclusions, why-recommended debug).
- Seed: 90 days of demo order history (+ a few orders a year ago) with co-purchase patterns,
  and a backfill rollup job.

## Global constraints

- A20: personalization/recently viewed only with server-resolved `personalization`;
  affinity uses only events captured with `personalization` among their consent purposes;
  nothing crosses tenants (RLS on every table, cross-tenant test).
- A2/A4: personalized or cart-aware responses are private/no-store; SSR stays public.
- A23 + §11.2: every result is active, priced in the market, purchasable; deduped against the
  current product/cart; falls back to bestsellers (then newest, so a new shop is never empty).
- Budgets: no new first-load JS on PDP/category; home only a tiny consent gate; drawer and
  personal lists are lazy chunks. SSR adds no new calls (home page model embeds it).
- No `unwrap()` outside tests, sqlx macros with `.sqlx/`, TS strict, Biome clean.

## Review focus

- Consent gating (server-side), cache headers of the private variant, edge forwarding.
- Rollup SQL: decay, support threshold, symmetry, window, cancelled orders, idempotency.
- Popularity debounce + reindex versioning.
- Visibility filtering and fallback chain; exclusions.
- Admin authz (settings: admin role), audit, input validation (ids, schedule, sizes).

## Tasks

1. **Migration + RLS test.** Tables above; cross-tenant test in `crates/commerce/tests/recommendations.rs`.
2. **Rollups** (`recommendations::rollup`): tests for stats from events + orders, co-purchase
   support/symmetry/window/cancelled, decay order, popularity debounce, affinity consent.
3. **Search popularity**: `ProductData.popularity` from `product_popularity`; doc test.
4. **Worker job + cron**: handler, schedule, reindex enqueue; worker test.
5. **Collections + settings** (domain + Admin API + audit), validation tests.
6. **Strategies + pipeline** with explain; tests for fallback, visibility, exclusions, dedupe,
   consent, recent ids validation; pure tests for context parsing and personalized ranking.
7. **Storefront endpoint** (public vs private) + home page model; API tests for cache headers.
8. **Admin debug endpoint** (`GET /admin/v1/recommendations/explain`).
9. **Edge** `/_p/recommendations` + tests.
10. **OpenAPI + clients**, SDK helpers.
11. **Theme**: PDP title by strategy, home featured/for you, cart drawer cross-sell, recently
    viewed rehydration; messages cs/sk/en.
12. **Admin UI**: Collections, Recommendations (settings + debug); i18n cs/sk/en.
13. **Seed history** + backfill job.
14. **E2E** `e2e/storefront/recommendations.spec.ts`; `make lint test`, perf, smoke images.
