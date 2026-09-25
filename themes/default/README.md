# Default theme

The storefront every shop starts with, and the template merchants fork and the AI editor
changes (spec §9, §12.3). Astro (`output: server`) on the Cloudflare adapter, Solid islands,
Tailwind 4. It runs as an untrusted worker behind the platform edge: it gets page models through
one restricted binding and nothing else (`docs/decisions/runtime-contract.md`).

Visual family: **market stall** (commerce-clarity). Cool ground, white product cards, the price
is the display type, one blue identity hue for chrome and links, amber only on the money action
(add to cart, checkout). Trust facts (delivery, returns, payment) sit next to the buy decision.
No fake urgency: stock is shown as it is, reductions only with an Omnibus reference price.

## Structure

```text
theme.tokens.json        design tokens (colours, fonts, radii) → CSS variables, also used by checkout
astro.config.mjs         platform-owned (adapter, islands, no inline styles, no prefetch)
package.json             platform-owned (dependencies are locked)
public/                  static files served by the edge (favicon)
src/
  layouts/Base.astro     <head> from the page model's seo, skip link, header, main, footer,
                         consent banner, RUM
  pages/                 required routes: / · /c/[...slug] · /p/[slug] · /search ·
                         /pages/[slug] · /blog · /blog/[slug] · 404
  components/            server-rendered building blocks (no JS):
    Header, Footer         trust band, logo, search, account, cart, category nav, popover sheet;
                           newsletter as a plain form (the edge redirects back with the outcome)
    ProductCard, ProductGrid, StockLine
    Listing, Facets        category + search pages, filter/sort GET form, pagination
    ProductDetails, TrustRow, Breadcrumbs, RecommendationSlot, Prose, Icon
  islands/               Solid islands (the only JavaScript on the page)
    SearchBox              typeahead (ARIA combobox) over a plain GET form
    MiniCart               cart button + drawer (<dialog>), free-delivery progress, checkout
    BuyBox                 price, variant picker, add to cart, sticky phone buy bar
    Gallery                scroll-snap gallery, thumbnails, arrows/keys
    FacetForm              behaviour only: submit on change (desktop), close dropdowns
    RecentlyViewed         needs `personalization` consent
    Rum                    Web Vitals for consented, sampled visits (loads web-vitals lazily)
  lib/
    storefront.ts          page-model access (SDK) + JSON-LD serializer
    i18n.ts                t() / tn() / href() for a page — all strings come from the catalog
    images.ts              every `sizes` value (keep in sync with grid/container widths)
    icons.ts, Icon.tsx     the icon set (own drawings, stroke paths)
    cart-store.ts          cart signal shared by islands
    product-store.ts       selected variant photo shared by BuyBox and Gallery
  styles/global.css      Tailwind entry, token mapping, building-block utilities
```

## Contract (what an edit may and may not do)

Enforced by `theme-kit lint`, `astro check`, the edge runtime and `make perf`
(`scripts/theme-gates.sh`):

- Edit only `src/**`, `public/**` and `theme.tokens.json`. `package.json` and
  `astro.config.mjs` belong to the platform; no new dependencies.
- **Data only through `@platform/storefront-sdk`** (`storefront(Astro.request)` in pages,
  `/_p/*` helpers in islands). No `fetch` to other origins, no URL imports, no WebSocket.
  The runtime blocks them anyway (CSP `connect-src 'self'`, no outbound network).
- **No inline scripts or event-handler attributes** (CSP). Behaviour goes into an island.
- **No cookies or device storage** in theme code. Consent is the SDK's
  (`consentStorage("personalization")` for anything remembered on the device, A20).
- `set:html` only for platform-sanitized HTML (`description_html`, `content_html`) and
  `jsonLd()`.
- **Every visible string from the catalog**: `const { t, tn } = i18n(shop)`; islands get the
  subset they use via `pick([...])`. Plural forms: `tn("listing.count", n)` reads
  `listing.count.one|few|many|other`. New strings are added to
  `crates/commerce/src/storefront/messages/{cs,sk,en}.json` (same keys in all three).
- **Links**: page-model hrefs already contain the locale prefix. Links the theme builds itself
  go through `href("/search")` / `productHref(shop, slug)`; island fetches get
  `base={shop.base_path}`.
- **Price claims (Omnibus, A18)**: strikethrough, discount % and "lowest price in 30 days" only
  when the page model sends `reference_price` (+ `discount_percent`). Never compute a
  reduction from anything else.
- **Consent banner** is the platform component `@platform/storefront-sdk/consent-banner`.
  Place it, do not rebuild it. `[data-consent-settings]` on any button reopens it.
- **Performance** (§9.6, A26): LCP ≤ 1.5 s, TBT ≤ 150 ms, CLS ≤ 0.05, JS ≤ 30 kB gzip (home 35)
  including islands loaded by scrolling, ≤ 10 page-model calls per render, axe 0
  serious/critical. Practical rules:
  - one LCP image per page, passed to `Base` as `lcp` (preload + `<img>` from one `sizes`);
    everything else `loading="lazy"`;
  - `sizes` from `lib/images.ts`, matching the real slot (a loose value downloads a 2-4×
    heavier file); change the grid → change the `sizes` line;
  - one web font (Archivo, headings + prices); body text uses the system stack;
  - prefer HTML/CSS (`<details>`, `popover`, `<dialog>`, `:has()`) over an island; an island
    that renders nothing on the server must use `client:idle`, never `client:visible`;
  - do not fetch page models per card (no N+1): what a card needs must be in the card model.
- **Accessibility** (WCAG 2.2 AA): one `h1` per page, landmarks (`header`, `nav` with labels,
  `main#main`, `footer`), visible focus (global `:focus-visible`), 44 px touch targets,
  `prefers-reduced-motion` respected globally, colour never the only signal (stock has words).

## Extension points

| Want to | Where |
|---|---|
| Rebrand (colours, fonts, radii) | `theme.tokens.json`; checkout follows automatically |
| Another font | add the woff2 under `src/fonts/`, its `@font-face` in `global.css`, the preload in `Base.astro`, and the family in `theme.tokens.json` (remove the old preload) |
| Header / navigation | `components/Header.astro` (mobile sheet included) |
| Home sections | `pages/index.astro` (hero, category tiles, featured grid, trust band) |
| Card content | `components/ProductCard.astro` (keep the stanza order; adjust `CARD_SIZES` if the grid changes) |
| Product page layout | `pages/p/[slug].astro`; gallery width ↔ `GALLERY_SIZES` |
| Buttons / chips / surfaces | utilities `btn`, `btn-buy`, `btn-primary`, `btn-secondary`, `chip`, `surface`, `sheet` in `global.css` |
| Cross-sell | `RecommendationSlot` renders whatever `sf.recommendations(...)` returns (WP17) |

## Tokens (`theme.tokens.json`)

Validated by `@platform/theme-kit` (A6): three groups, keys `^[a-z][a-z0-9-]{0,31}$`.

| Group | Values | Used as |
|---|---|---|
| `colors` | `#rrggbb` or `oklch(L C H)` | `--color-<key>` → `bg-<key>`, `text-<key>`, … |
| `fonts` | family list, letters/digits/`,'" -` | `--font-<key>` → `font-<key>` |
| `radius` | `0`, `<n>rem`, `<n>px` | `--radius-<key>` → `rounded-<key>` |

Colour roles: `background` (page ground), `card` (product cards, header, sheets), `foreground`,
`muted` / `muted-foreground` / `subtle` (secondary surfaces and text), `border`, `identity` /
`identity-ink` / `identity-wash` (chrome, links, focus, selection), `panel` /
`panel-foreground` (trust band, footer), `buy` / `buy-hover` (add to cart and checkout
only), `stock-in`, `stock-low`, `sale` / `sale-wash`. Keep text pairs at ≥ 4.5:1 (`identity-ink`
on `card`, `panel-foreground` on `panel`, `foreground` on `buy`). A new token must also be
listed in the `@theme reference` block of `global.css` so Tailwind generates its utilities.

## Checks

```bash
pnpm --filter @platform/theme-default check            # astro check (types)
node packages/theme-kit/src/cli.ts lint themes/default  # contract lint
make theme-build                                        # build, pack, publish locally
make perf                                               # Lighthouse + JS + axe budget
make e2e args=storefront                                # Playwright theme suite
```
