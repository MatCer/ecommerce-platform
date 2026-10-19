# Storefront browsing

A shopper browses the demo shop's theme (default theme `themes/conversion`) served by the edge:
home, categories with filters and sort, search, product pages with variants, and the cart drawer.

## Sub-features

- `sf-home` renders the home page with navigation, hero and bestsellers.
- `sf-category` lists a category with filters and sort.
- `sf-search` finds products and shows an empty state for no match.
- `sf-pdp` shows a product, switches variants and adds to cart.
- `sf-cart` opens the cart drawer, changes quantity and hands off to checkout.

## How to get to it (user POV)

- Open `http://demo.localhost:38080/` (CZ) or `http://demo-sk.localhost:38080/` (SK).
- Choose a category in `navigation "Hlavní navigace"`, e.g. `/c/obleceni`.
- Type into `combobox "Hledat"`, or open `/search?q=mikina`.
- Open a product, e.g. `/p/mikina-fleece`.

## Driving it with Playwright

Preconditions:

- Held stack up and seeded; `stack.sh doctor 3` shows 200 for the shop.
- The context has the consent cookie for the shop origin and `locale: "cs-CZ"`.

- **Spec run.** Run `stack.sh e2e 3 --project chromium --no-deps storefront/theme.spec.ts`. Exit 0;
  failures leave traces under the `--output` dir.
- **Home.** Go to `/`. `heading "Nová kolekce" [level=1]` and `navigation "Hlavní navigace"`
  are visible; capture `locator("body").ariaSnapshot()` and a screenshot.
- **Product and variant.** Go to `/p/mikina-fleece`, wait until no `astro-island[ssr]` remains,
  click the `label` chips `Cihlová` and `L`. `radio "L"` is checked and `Kód: LN-025-CIHLOVA-L`
  is visible in `main`.
- **Add to cart.** Click `button "Přidat do košíku"` in `main`. `dialog /Košík/` opens with
  `link "Mikina Fleece"`; `button "Košík, položek: 1"` exists in the header.
- **Quantity.** Click `button "Přidat"` in the drawer. The drawer shows `2 položky`.
- **Handoff.** Click `button "K pokladně"`. The URL becomes `http://checkout.demo.localhost:38080/...`
  (continue in [checkout](./checkout.md)).
- **Search empty state.** Go to `/search?q=xyzzyqq`. `Nic jsme nenašli.` is visible.

## Gotchas

- Variant radios are visually hidden inside chip labels: click the `label`, not the radio.
- Clicking before hydration does nothing; wait for the islands (the specs' `hydrated()` helper).
- Without the consent cookie the consent dialog covers the page and intercepts clicks.
- Search depends on Meilisearch; while it is reindexing the shop shows degraded search, which is
  not a theme bug.
