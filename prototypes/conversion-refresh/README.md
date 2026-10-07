# Conversion refresh prototype (SHOP-81)

Static HTML, no JavaScript, Tailwind 4. Design source of truth for the visual refresh of
`themes/conversion` (epic SHOP-74). Direction approved at STOP 1: `direction/index.html`.
Hub: `index.html`. Pages: `home.html`, `listing.html`, `product.html`, `product-cart.html`,
`nav.html`, `search.html`. Screenshots: `shots/<page>-<390|1440>.jpg`.

Build CSS: `cd prototypes/conversion-refresh && pnpm dlx @tailwindcss/cli@4.3.3 -i src/styles.css -o styles.css --minify`.
`src/tokens.css` is the future `theme.tokens.json` (23 colours, 2 fonts, 4 radii);
`src/styles.css` is the future `global.css` (same `@theme reference` block, same class vocabulary).

## Design rationale

Same placements as today, a 2026 execution. The 2018 feel came from the palette (navy, neon
green, blue, red), unshipped Roboto at 12–14 px, three stacked header bands, small radii and
white boxed cards on grey. All of that goes; nothing moves.

### Type (Figtree, one variable file, 19 kB)

| Role | Phone | Desktop | Weight / tracking |
|---|---|---|---|
| Hero display (`text-display`, `-lg`) | 36 px / 1.05 | 56 px / 1.02 | 700, −0.02 / −0.025 em |
| Page h1 (PDP, listing) | 30 px | 40 px | 700, −0.015 em, lh 1.12 |
| Section h2 | 24 px | 32 px | 700, −0.015 em |
| PDP price | 32 px | 36 px | 700, tabular, sale red |
| Card price | 17 px | 18 px | 700, tabular; struck reference 14 px/500 subtle |
| Card name (h3) | 15 px | 16 px | 500 (the price is the loudest, the name is quiet) |
| Body, descriptions | 16–17 px / 1.5 | 16–17 px | 400 |
| UI (nav, chips, buttons, trust rows) | 15 px | 15 px | 500–600 |
| Meta (Omnibus on cards, stock, breadcrumbs, SKU) | 13 px | 13 px | 400–600 |
| Eyebrow | 12 px | 12 px | 700, uppercase, +0.1 em (the only 12 px) |

Floor is 13 px for anything a shopper reads. Headings get `text-wrap: balance`; one-letter
Czech prepositions are bound with NBSP.

### Spacing and rhythm

4 px base: 4 / 8 / 12 / 16 / 20 / 24 / 32 / 40 / 48 / 64 / 80. Section gap 48 px phone,
80 px desktop (`mt-12 md:mt-20`), heading to content 20 / 28 px. Grid gaps 12 × 32 phone,
24 × 40 desktop. Container 80 rem, 16 / 24 px gutters. Columns: home rails 2 (1.5 visible,
68 vw) / 4; listing 2 / 3 (next to the 15–17 rem filter column); spotlight 4 with a 2×2 lead;
category tiles 3 / 6. `lib/images.ts` `sizes` stay valid: the slots have the same widths.

Section headings are left-aligned with an inline "Zobrazit vše →" on the right baseline, the
subtitle (ForYou) sits under the heading, not centred.

### Colour roles (palette B, Ink + Cobalt, unchanged values)

Ink is the identity: header text, links, primary buttons, selected chips, focus ring, applied
filter chips. Cobalt `buy` appears only on "Přidat do košíku" and "K pokladně" (white label,
7.4:1). Red `sale` only on a claimed reduction: the price, the struck reference next to it, the
−N % pill, always with the 30-day lowest price visible. `panel` is the footer and the
navigation sheet; the header is white. Measured pairs: every text pair ≥ 5.0:1 (worst: sale on
sale-wash 5.00), rating ≥ 4.0:1 on card, muted and review-wash. No token tweaks were needed.

### Radius "Soft" and surfaces

8 (thumbnails, small media) / 12 (chips, inputs, select) / 16 (photo boxes, cards' photo,
surfaces, muted panels) / 24 (hero container, category promo, buy panel, drawer, sheets,
guarantee box). Buttons, badges, swatches, search field, stepper, applied filters: pill.
Card shadows are gone; one `shadow-lift` on the hero inset photo and sheets, `shadow-sheet`
on drawers, `shadow-bar` on the sticky phone bar.

### Header

Two bands plus a slim trust line:

1. Sticky white bar, 56 px phone / 64 px desktop: menu button (all widths, opens the full
   tree in a native popover sheet), logo, inline category nav from 1280 px, search field from
   768 px (pill, muted ground), account, cart with a count pill. On phones the search is a
   button that opens a top popover with the field (`autofocus`), so the sticky bar stays 56 px.
2. Category row below 1280 px (44 px, scrollable, current item underlined).
3. Trust line (36 px, muted, 13 px, not sticky): free delivery threshold · returns · payments,
   phones show the first only. Repeated above the footer.

### Card anatomy (DOM order = ProductCard.astro order)

Photo box 4:5, muted ground, 16 px radius → badges top-left (−N % red pill, "Novinka" white
pill) → phone-only swipe dots bottom-centre (when the product has more than one photo) →
second photo on hover for pointer devices (`<source media>`). Text block: name (h3, first in
DOM, shown third), price row (shown first: sale price red + struck reference), Omnibus line
when reduced ("Nejnižší cena za 30 dní: 1 521 Kč", 13 px, visible), then one row with the
stock words on the left and colour swatches (16 px dots, `aria-label` with the colour names)
on the right. The whole card is the link.

### PDP

7fr / 5fr. Gallery: snap rail (one photo visible, thumbnails are anchors to the slides on
desktop, dots on phones), 16 px radius. Buy column sticky: brand (muted), h1, rating link to
`#reviews` (stars + 4,8 + count), short description, then the buy panel (24 px radius, muted):
price row with the −N % pill, Omnibus line, VAT note, colour swatches (44 px, selected ring),
size pills (unavailable size struck), size chart link, stock words + SKU, the cobalt CTA
(56 px), delivery date and delivery cost line, returns line, payments line. Under it: the
watch form (`<details>` surface) and the guarantee box. Section tabs are sticky under the
header on phones. Reviews keep the 4 / 8 split with the histogram. Sticky phone bar: compact
price + variant/stock words + CTA.

### Cart

`<dialog>` right drawer (27 rem, 24 px radius on the left) from 768 px, bottom sheet
(92 dvh, 24 px top radius) on phones. Header with count, free-delivery progress on a muted
band, line items with pill stepper (44 px targets), cross-sell rail of mini cards, footer with
total incl. VAT, shipping note, cobalt checkout CTA, "Pokračovat v nákupu".

### Motion

Only CSS: sheet and drawer slide (`@starting-style`, `allow-discrete`), second-photo fade,
tile photo scale on hover, colour transitions on buttons and chips, cart-count bump keyframe,
`@view-transition { navigation: auto }`. All reduced to 0.01 ms under
`prefers-reduced-motion`.

## Performance guards built into the markup

One LCP image per page (`fetchpriority="high"`, preloaded in `<head>`), everything else
`loading="lazy" decoding="async"` with width/height; Figtree preloaded; no scripts, no inline
styles besides swatch colours; no autoplay; hover photos only download on `(hover: hover) and
(pointer: fine)`.

## Prototype-only conventions

- `.is-open` on a `.sheet` and a `<dialog open>` + `.scrim` show states the real theme opens
  with the Popover API / `showModal()`.
- Dashed "Slot:" boxes on the home page mark room for future modules (logo strip, shop
  reviews, stats). They are not part of the theme.
- Product photos are the demo seed fixtures (`img/`), 640×800, < 105 kB each.

## Gates (2026-10-07, STOP 2)

Lighthouse 12 mobile, simulated throttling, median of 3, run sequentially.

| Page | Served as-is (python, uncompressed CSS, 640px JPEG) | Theme-like (gzip CSS/HTML, 480px WebP) |
|---|---|---|
| home | LCP 3451 ms, img 1093 kB | LCP 1351 ms, img 178 kB |
| listing | LCP 3677 ms, img 815 kB | LCP 1352 ms, img 140 kB |
| product | LCP 2701 ms, img 308 kB | LCP 1277 ms, img 55 kB |

TBT 0 ms, CLS 0.000, JS 0 kB, CSS 9.5 kB gz, font 19.6 kB on every page. The live theme measures 1354 ms LCP on
home under the same harness (it serves AVIF with `sizes` from `/media`), so the theme-like column is the comparable one.
The as-is column fails the 1.5 s budget only because of the hosting: render delay from bandwidth contention with
full-size JPEGs and uncompressed CSS. Headroom is thin (~150 ms): the port must keep AVIF + `sizes` and the font preload.

axe (wcag2a/aa, 2.1, 2.2 AA) at 390 and 1440: 0 violations on all 7 pages.
