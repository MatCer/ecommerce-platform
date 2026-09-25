# WP8 Default theme: implementation plan

> **For agentic workers:** execute task by task with TDD; commit after every task.

**Goal:** turn the WP2/WP6 probe in `themes/default` into the polished default theme every
merchant forks: full page set, i18n from the platform catalogs, locale prefixes per market,
consent banner as a platform component, consented RUM + recently viewed, WCAG 2.2 AA, and
`make perf` green on seeded data for home, category, product and search.

**Spec:** §9.1, §9.2, §9.5, §9.6, §11.3, §14, §17 WP8, amendments A1, A2, A4, A6, A18, A20,
A26. Runtime contract: `docs/decisions/runtime-contract.md`; AI-edit gaps:
`docs/decisions/ai-edit-prompts.md`.

## Global Constraints

- Theme contract: data only via `@platform/storefront-sdk`; no inline scripts/handlers, no
  cookie access, `set:html` only for `description_html`/`content_html`/`jsonLd()`; package.json
  and astro.config.mjs are platform-owned; tokens only in `theme.tokens.json` (A6 schema).
- Every theme string comes from `ShopModel.messages` (cs/sk/en catalogs in
  `crates/commerce/src/storefront/messages/*.json`, same keys in all three, tested).
- Islands get only the messages they use (`pick`), so a page serializes one language, few keys.
- Budgets (§9.6/A26): LCP ≤ 1.5 s, TBT ≤ 150 ms, CLS ≤ 0.05, JS ≤ 30 kB gz (home 35) incl.
  scroll-triggered islands, ≤ 10 storefront calls per render, 0 third-party origins, axe 0
  serious/critical (WCAG 2.2 AA tags). Keep the WP2 fixes: one `lcpImage()` for preload + img,
  real-slot `sizes`, one preloaded web font, only the LCP image in gallery SSR HTML.
- Omnibus (A18): strikethrough, discount % and "lowest price in 30 days" render only when the
  page model sends `reference_price` (its claim flag); `compare_at` does not exist in the
  storefront model.
- Consent (A20): no device storage and no beacon before a choice; RUM needs `analytics`,
  recently viewed needs `personalization`. The banner is a platform component in the SDK.
- Machine limits: `CARGO_BUILD_JOBS=6`, Playwright ≤ 4 workers, Lighthouse serial.

## Review Focus

- Locale prefix correctness end to end (edge strip → API context → prefixed hrefs, canonical,
  hreflang) without cache poisoning (locale is part of the edge cache key).
- Consent: nothing written before a choice; graceful when `/_p/consent` 404s (WP9 builds it).
- A11y of islands: combobox, drawer dialog, variant radios, focus return, reduced motion.
- JS headroom on the PDP.

## Design decisions

- **Aesthetic:** stays in the platform family *market stall* (commerce-clarity, set in WP2):
  cool ground, white cards, price as display type, amber only on buy actions. WP8 refines it
  into a finished system (type scale, spacing rhythm, shadows, header/footer, states) instead
  of switching families. The home page gets a *shallow* hero (brand line + CTA beside the
  category tiles), because the family forbids a tall hero above the products.
- **Locale prefixes (WP6 gap):** the edge strips `/<locale>` when the locale is one of the
  market's non-default locales, and renders with that locale (the request context and the
  API `X-Locale` carry it; the cache key already includes the locale). The theme renders the
  unprefixed route. The API builds every page-model href with `Context::path()` (prefix +
  path) and canonicals with `Context::page_url()`; hreflang alternates cover every
  (market, locale). `ShopModel.base_path` (`""` or `/en`) is what the theme prefixes its own
  links with (`/search`, `/p/<slug>`). Islands call `/_p/public/*` under the same prefix.
  Cart routes stay unprefixed (cookie path `/_p`). The seed gives the CZ market `en` as a
  second locale to exercise it.
- **Consent banner = SDK platform component** (`@platform/storefront-sdk/consent-banner`,
  Solid, plain CSS on the theme's token variables, so it follows the brand but a theme edit
  cannot restyle it into a dark pattern). Choice stored as the strictly necessary `consent`
  cookie only after a choice, then `POST /_p/consent {purposes}`; a 404/network error is
  ignored (endpoint arrives with WP9). A `consent:open` event / `[data-consent-settings]`
  button reopens it (footer "Cookie settings").
- **RUM:** `web-vitals` standard build instead of `/attribution` (≈ 2.5 kB less when sampled).
- **Recently viewed:** `consentStorage("personalization")` in the SDK (localStorage, no-op
  without consent); stores slug/name/image only. No prices: a stored price goes stale and
  would be a misleading price indication (batch card lookup is a WP10 gap).
- **Second card image:** `ProductCard.images[1]` rendered `loading="lazy"` and shown on
  hover/focus via CSS only (opacity), on hover-capable pointers; mobile does not download it
  (`<picture>` with a `(hover: hover)` media source).
- **Free-shipping bar:** only when `shop.free_shipping_threshold`/`cart.free_shipping_remaining`
  exist (today never; WP10). Cross-sell/recommendation slots render nothing until WP17 data.
- **Payment/carrier marks:** own neutral SVG/text marks (no third-party logos).

## Tasks

1. **API locale paths** (`crates/commerce/src/storefront/{mod,pages,product}.rs`, seed):
   `Context::{path, page_url}`, `alternates(ctx, |market, locale| …)`, `ShopModel.base_path`
   and `locales` (all market locales), brand facet label via catalog, seed CZ locales
   `[cs, en]`. Unit tests for path/alternates; storefront integration test for `/en` hrefs.
   `make openapi` → SDK schema.
2. **Edge locale prefix** (`apps/edge/src/gateway.ts`, `sites.ts`): `Site.locales`, strip the
   prefix for theme renders and `/_p/public/*`; tests (render gets `x-locale`, unknown or
   default prefix is not stripped, cache keys differ per locale).
3. **Catalog strings**: every new theme key in cs/sk/en.
4. **SDK**: `consent.ts` (state, write, POST, open event), `ConsentBanner.tsx`,
   `consentStorage`, `web-vitals` standard build, `base` for island fetches; vitest.
5. **Theme foundation**: tokens, `global.css` (type scale, utilities, motion), Base layout
   (head/SEO, skip link, landmarks), Header (logo, nav w/ subcategories, mobile nav drawer,
   search, cart), Footer (menus, legal, withdrawal link to `checkout.<host>/withdraw`,
   payment/carrier marks, newsletter, cookie settings, market/locale switch).
6. **Pages**: home (hero, categories, bestsellers = `featured`), category/search (toolbar,
   facet form PE, active-filter chips, sort, pagination, empty states), product (gallery, buy
   box, sticky mobile ATC, price/Omnibus/unit price, stock, trust row, GPSR, parameters,
   breadcrumbs, recently viewed, recommendation slot), CMS page + blog shells, 404.
7. **Gates**: `measure.ts` pages incl. search, axe `wcag22aa`; `make perf` green.
8. **e2e** `e2e/storefront/*.spec.ts` + screenshots `docs/screenshots/wp8/`.
9. **Docs**: `themes/default/README.md` (structure, extension points, AI-edit rules, tokens).
10. **Verify + Astra review + PR.**
