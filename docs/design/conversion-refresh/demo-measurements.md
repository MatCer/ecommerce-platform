# Storefront demo measurements

Collected 2026-10-07, from this machine in Slovakia. Inspiration and internal moodboard only. No reference CSS/markup copied into the repository; no repository code changed.

## Method and scope

- Playwright from the existing repository install, Chrome `/usr/bin/google-chrome`, desktop **1440×900**, mobile **390×844**, device scale 1. Fresh browser contexts per target/width; `en-US` locale; stores could choose their own region/currency. These are viewport comparisons, not a touch-device emulation.
- Numbers are `getComputedStyle` values or DOM bounds, not visual guesses. Raw JSON preserves selectors, text samples, computed styles, rectangles, rendered-font evidence, URLs, and failures. Tables round to 2 decimals; raw precision is retained. **d/m = desktop/mobile**, px unless stated.
- H1 means an actual visible home `h1`, excluding logo/accessibility text. Many themes use `h2`, another element, SVG, or raster text in the hero: missing H1s are explicitly `n/m`; an additional first-heading column gives observed text scale and its tag. H2 means the first later section heading, including a semantic H3 when that is how the demo implements its section title. Later slides are excluded where hidden. Dynamic slides can change between captures.
- Body size/weight/leading refers to the computed body root; the font column includes a rendered paragraph sample where available. **†** = CSS family with a custom font confirmed by Chrome, whose platform name is anonymized as “OTS derived font”; its exact internal family is `n/m (Chrome anonymization)`. **‡** = computed family only; rendered-family verification is `n/m (no glyph evidence for that sample)`. Other names are Chrome’s actual rendered families. The raw JSON includes complete CSS fallback stacks and glyph counts.
- Card values use the sampled collection grid, falling back to a home card where unavailable. Home grids are also listed. A featured carousel is identified as overflow rather than interpreted as a full grid. Grid counts use computed CSS tracks when all tracks fit the viewport; overflow strips count complete visible cards instead of offscreen tracks. Occupied product positions alone can miss promotional tiles.
- Section gap is the median vertical distance from the preceding section’s last rendered image/text edge to the next section’s first rendered image/text edge, limited to inspected main-content sections with measurable contents. It includes interior section whitespace; it is not an inferred theme spacing token. Raw section bounds and each gap are included.
- Container max-width is reported only when a computed constraint exists; current content width is labelled separately. Gutters are the sampled outer content container’s left inset plus padding, not the collection filter sidebar. Layout verification may scroll collection grids to activate lazy cards. Header height includes all measured bands above content; bands do not count icons/menus within one row.
- Colors are body background/text and the sampled purchase button. Ratios use sRGB luminance and foreground-alpha compositing. “Select size”/sold-out labels are preserved where no active ATC is shown. Transparent or nonuniform backgrounds without a reliable solid contrast are `n/m`. Sale colors require an identifiable sale-price element; a “Sale price” accessibility label alone does not prove a discount.
- Mobile sticky ATC was observed after scrolling two viewport heights; `n` is limited to that observation. Returns/delivery checks use the rendered purchase form/adjacent parent’s text. Card shadows/borders describe the resting sampled card/image, not hover states. Missing variants on a single-option product are not assumed to prove theme support is absent.
- Six viewport JPEGs per target: `research_notes/Storefront theme inspiration/refs/ (local only, not committed) <target>-home-top-<width>.jpg`, `home-down`, `pdp-top`; each below 400,000 bytes. Home-down uses an intended scroll of 1800/1688 px, clamped if needed. Actual offsets and byte sizes are in JSON.
- Lighthouse **12**, performance only, mobile form factor, default Lighthouse simulated throttling; three cold runs per target, strictly sequential. Per-metric medians are shown. JS/font sizes are transferred **decimal kB**; font files are unique requested font URLs. Lighthouse uses its own default mobile emulation, separate from the 390 px visual capture. Newsletter/consent behavior can affect these unmodified home audits; these are lab results for the sampled URLs, not field CWV or intrinsic theme-only scores.
- Every Shopify median excludes real stores and `n/m`; home H1 has only two eligible demo samples, and explicit computed container max-width only one. It is a descriptive statistic over these nine sampled demos, not a design recommendation. Raw rows retain both widths and sampled-page differences.

## Type scale

| Target | Body px d/m | H1 px d/m | First visible hero/text heading px d/m (tag) | Section H2 px d/m | Card name px d/m | Card price px d/m | PDP title px d/m | PDP price px d/m | Button label px d/m | Font families by role |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Horizon | 14 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | 56 / 48 div/div | 24 / n/m (no section heading sample) | 14 / 14 | 12 / 12 | 32 / 32 | 14 / 14 | 14 / 14 | body: Inter, sans-serif ‡<br>H1: n/m (no text sample)<br>H2: Inter, sans-serif †<br>card: Inter, sans-serif †<br>PDP: Inter, sans-serif †<br>price: Inter, sans-serif †<br>button: Inter, sans-serif † |
| Horizon Atelier | 12 / 12 | 120 / 72 | 120 / 72 h1/h1 | 24 / 24 | 12 / 12 | 12 / 12 | 48 / 28 | 12 / 12 | 12 / 12 | body: "Red Hat Text", sans-serif ‡<br>H1: Newsreader, serif †<br>H2: Newsreader, serif †<br>card: "Red Hat Text", sans-serif †<br>PDP: Newsreader, serif †<br>price: "Red Hat Text", sans-serif †<br>button: "Red Hat Text", sans-serif † |
| Horizon Ritual | 14 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | n/m (hero text absent or image/SVG) / n/m (hero text absent or image/SVG) —/— | 48 / 36 | 14 / 14 | 14 / 14 | 32 / 32 | 14 / 14 | 14 / 14 | body: Geist, sans-serif ‡<br>H1: n/m (no text sample)<br>H2: Chivo, sans-serif †<br>card: Geist, sans-serif †<br>PDP: Chivo, sans-serif †<br>price: Geist, sans-serif †<br>button: Geist, sans-serif † |
| Horizon Fabric | 14 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | n/m (hero text absent or image/SVG) / n/m (hero text absent or image/SVG) —/— | 32 / 32 | 14 / 14 | 14 / 14 | 32 / 32 | 12 / 12 | 14 / 14 | body: Geist, sans-serif ‡<br>H1: n/m (no text sample)<br>H2: Geist, sans-serif †<br>card: Geist, sans-serif †<br>PDP: Geist, sans-serif †<br>price: Geist, sans-serif †<br>button: Geist, sans-serif † |
| Prestige (Couture) | 13 / 13 | n/m (no visible home h1) / n/m (no visible home h1) | 32 / 22.15 p/p | 28 / 20.12 | 12 / 12 | 12 / 12 | 22 / 18.06 | 18 / 16.03 | 12 / 12 | body: Poppins, sans-serif †<br>H1: n/m (no text sample)<br>H2: Jost, sans-serif †<br>card: Jost, sans-serif †<br>PDP: Jost, sans-serif †<br>price: Jost, sans-serif †<br>button: Poppins, sans-serif ‡ |
| Symmetry | 14 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | 60 / 40 h2/h2 | 34 / 27 | 14 / 14 | 14 / 14 | 34 / 27 | 23.6 / 20.24 | 14 / 14 | body: Montserrat, sans-serif †<br>H1: n/m (no text sample)<br>H2: Montserrat, sans-serif †<br>card: Montserrat, sans-serif ‡<br>PDP: Montserrat, sans-serif †<br>price: Montserrat, sans-serif †<br>button: Montserrat, sans-serif ‡ |
| Be Yours | 16 / 15 | 64 / 42.6 | 64 / 42.6 h1/h1 | 50 / 33.75 | 16 / 13 | 18 / 18 | 35 / 25.35 | 22 / 22 | 14 / 14 | body: Assistant, sans-serif †<br>H1: Jost, sans-serif †<br>H2: Jost, sans-serif †<br>card: Assistant, sans-serif †<br>PDP: Jost, sans-serif †<br>price: Jost, sans-serif †<br>button: Jost, sans-serif † |
| Concept | 16 / 16 | n/m (no visible home h1) / n/m (no visible home h1) | n/m (hero text absent or image/SVG) / 32 —/h2 | 48.5 / 32 | 18.54 / 16 | 15.27 / 14 | 40 / 24 | 21.95 / 18 | 15.27 / 14 | body: Inter, sans-serif †<br>H1: n/m (no text sample)<br>H2: Inter, sans-serif †<br>card: Inter, sans-serif †<br>PDP: Inter, sans-serif ‡<br>price: Inter, sans-serif †<br>button: Inter, sans-serif † |
| Impact | 16 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | 60 / 40 p/p | 36 / 24 | 16 / 14 | 16 / 14 | 48 / 32 | 20 / 18 | 16 / 14 | body: Barlow, sans-serif †<br>H1: n/m (no text sample)<br>H2: Barlow, sans-serif †<br>card: Barlow, sans-serif †<br>PDP: Barlow, sans-serif †<br>price: Barlow, sans-serif †<br>button: Barlow, sans-serif ‡ |
| Glossier | 16 / 16 | n/m (no visible home h1) / n/m (no visible home h1) | 20 / 18 h2/h2 | 12 / 12 | 14 / 14 | 14 / 14 | 32 / 28 | 14 / 14 | 14 / 14 | body: Apercu<br>H1: n/m (no text sample)<br>H2: Apercu<br>card: Apercu<br>PDP: Apercu Medium<br>price: Apercu<br>button: Apercu |
| Rothy's | 14 / 12 | n/m (no visible home h1) / n/m (no visible home h1) | n/m (hero text absent or image/SVG) / n/m (hero text absent or image/SVG) —/— | n/m (no section heading sample) / n/m (no section heading sample) | 14 / 12 | 14 / 12 | 40 / 24 | 16 / 16 | 16 / 16 | body: <br>H1: n/m (no text sample)<br>H2: n/m (no text sample)<br>card: <br>PDP: Grifo S<br>price: <br>button:  |
| Tecovas | 16 / 16 | 42 / 36 | 42 / 36 h1/h1 | 42 / 36 | 18 / 18 | 14 / 13 | 24 / 22 | 18 / 16 | 16 / 16 | body: Mundial<br>H1: Borax VF<br>H2: Borax VF<br>card: Lorimer No 2<br>PDP: Lorimer No 2<br>price: Lorimer No 2<br>button: Lorimer No 2 |
| Footshop | 15 / 15 | 24 / 22 | 24 / n/m (hero text absent or image/SVG) h2/— | 18 / 22 | 15 / 15 | 15 / 15 | 24 / 22 | 20 / 20 | 15 / 15 | body: Neue Haas Unica W1G<br>H1: Foot Medium<br>H2: Foot Medium<br>card: Neue Haas Unica W1G, Neue Haas Unica W1G Medium<br>PDP: Foot Medium<br>price: Foot Medium<br>button: neue-haas-unica, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen, Ubuntu, "Helvetica Neue", Arial, sans-serif ‡ |
| Vuch | 14 / 14 | n/m (no visible home h1) / n/m (no visible home h1) | n/m (hero text absent or image/SVG) / n/m (hero text absent or image/SVG) —/— | 32 / 24 | 20 / 18 | 20 / 18 | 28 / 18 | 24 / 24 | 20 / 18 | body: Nunito Sans 12pt ExtraLight<br>H1: n/m (no text sample)<br>H2: Sofia Pro<br>card: Nunito Sans 12pt ExtraLight<br>PDP: Sofia Pro<br>price: Sofia Pro<br>button: Nunito Sans 12pt ExtraLight |
| **Medians across Shopify demos** | 14 / 14 | 92 / 57.3 | 60 / 40 | 34 / 29.5 | 14 / 14 | 14 / 14 | 34 / 28 | 18 / 16.03 | 14 / 14 | — |


## Weight, tracking and leading

Cells: weight; letter-spacing; line-height. `normal` is retained as computed, not replaced by an invented px value.

| Target | Body desktop | Body mobile | H1 desktop | H1 mobile | Section heading sampled |
| --- | --- | --- | --- | --- | --- |
| Horizon | 400; normal; 22.4px | 400; normal; 22.4px | n/m (no visible h1) | n/m (no visible h1) | Featured products |
| Horizon Atelier | 400; normal; 19.2px | 400; normal; 19.2px | 200; normal; 120px | 200; normal; 72px | New this season |
| Horizon Ritual | 400; normal; 19.6px | 400; normal; 19.6px | n/m (no visible h1) | n/m (no visible h1) | Latest arrivals |
| Horizon Fabric | 400; normal; 19.6px | 400; normal; 19.6px | n/m (no visible h1) | n/m (no visible h1) | New this season |
| Prestige (Couture) | 400; normal; 21.45px | 400; normal; 21.45px | n/m (no visible h1) | n/m (no visible h1) | Dresses |
| Symmetry | 300; normal; 22.4px | 300; normal; 22.4px | n/m (no visible h1) | n/m (no visible h1) | “We believe in two things: the pursuit of quality in all we do, and looking after one another. Everything else should just take care of itself.” |
| Be Yours | 400; normal; 28.8px | 400; normal; 27px | 700; -1.28px; 64px | 700; 0.852px; 42.6px | New Arrivals |
| Concept | 400; normal; normal | 400; normal; normal | n/m (no visible h1) | n/m (no visible h1) | We believe in the power of sound |
| Impact | 400; normal; 25.6px | 400; normal; 22.4px | n/m (no visible h1) | n/m (no visible h1) | Lightweight luxury earphones |
| Glossier | 400; normal; 18.4px | 400; normal; 18.4px | n/m (no visible h1) | n/m (no visible h1) | SHOP ALL |
| Rothy's | 400; 0.5px; 20px | 300; 0.5px; 16px | n/m (no visible h1) | n/m (no visible h1) | n/m (heading absent) |
| Tecovas | 400; normal; 24px | 400; normal; 24px | 550; normal; 50.4px | 550; normal; 43.2px | Being brave isn't about being different — it's about being yourself. |
| Footshop | 400; normal; 22.0005px | 400; normal; 22.0005px | 500; normal; 30px | 500; normal; 27.9994px | Než se město probudí |
| Vuch | 400; normal; 21px | 400; normal; 21px | n/m (no visible h1) | n/m (no visible h1) | Doporučené produkty |
| **Medians across Shopify demos** | 400; n/m (computed normal; no px value); 21.92px | 400; n/m (computed normal; no px value); 21.92px | 450; -1.28px; 92px | 450; 0.85px; 57.3px | — |


## Shape & spacing

Radii are desktop samples; both widths are preserved in JSON. They are computed declarations; percentage values are explicitly unresolved in px. Zero-size shadow declarations have no visible effect and count as no shadow. Max-width `none` is the measured declaration, not a claim that the entire theme has no container limit.

| Target | Card image radius | Card container radius | Button radius | Chip radius | Input radius | Drawer/sheet radius | Section gap d/m | Grid column gap d/m | Container max-w / current width | Side gutter d/m | Header bands d/m | Header total height d/m | Card shadow / border |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Horizon | 0 | 0 | 14 | 32 | 0 | 0px | 48 / 33.59 | 16 / 12 | none; observed 1360 | 40 / 0 | 1 / 1 | 66.38 / 60 | n / n |
| Horizon Atelier | 0 | 0 | 0 | n/m (no rendered sample) | 0 | 0px | 82 / 61.39 | 16 / 12 | none; observed 1360 | 40 / 0 | 1 / 1 | 74.38 / 60 | n / n |
| Horizon Ritual | 0 | 0 | 0 | 0 | 0 | 0px | 49 / 61.59 | 16 / 12 | none; observed 1360 | 40 / 0 | 1 / 1 | 66.38 / 60 | n / n |
| Horizon Fabric | 0 | 0 | 2 | 100 | 0 | 0px | 48 / 64.64 | 0 / 0 | n/m (no explicit constraint); observed 1440 | 0 / 0 | 1 / 1 | 66.38 / 60 | n / n |
| Prestige (Couture) | 0 | 0 | 0 | 0 | 0 | 0px | 128 / 65 | 60 / 10 | none; observed 1344 | 48 / 10 | 2 / 2 | 130.48 / 103 | n / n |
| Symmetry | 0 | 0 | 0 | 50% (computed; not uniform px) | 0 | n/m (no open sheet sample) | 81.7 / 83.87 | 30 / 16 | none; observed 1440 | 70 / 16 | 2 / 2 | 100.5 / 80.98 | n / n |
| Be Yours | 0 | 0 | 0 | n/m (no rendered sample) | 0 | n/m (no open sheet sample) | 282.4 / 184.3 | 30 / 10 | 1400px; observed 1400 | 70 / 15 | 4 / 3 | 201.94 / 126.25 | n / n |
| Concept | 0 | 15.16 | 60 | 5 | 0 | n/m (no open sheet sample) | 313.5 / 140 | 18.19 / 12 | none; observed 1425 | 48 / 20 | 2 / 2 | 160.66 / 144.66 | n / n |
| Impact | 0 | 6 | 60 | 4 | 0 | 0px | 168 / 120 | 24 / 8 | none; observed 1344 | 48 / 20 | 2 / 2 | 141.98 / 96.69 | n / n |
| Glossier | 0 | 0 | 0 | n/m (no rendered sample) | 0 | n/m (no open sheet sample) | 61 / 33 | 16 / 12 | 1920px; observed 1440 | 16 / 12 | 2 / 2 | 71.98 / 97.38 | n / n |
| Rothy's | 0 | 0 | 0 | 50% (computed; not uniform px) | 0 | n/m (no open sheet sample) | 36 / 78.63 | n/m (no measured grid gap) / n/m (no measured grid gap) | n/m (no explicit constraint); observed 1440 | 0 / 16 | 2 / 2 | 107 / 105 | n / y |
| Tecovas | 0 | 0 | 4 | 8 | 0 | 0px | 929.3 / 491.79 | 16 / 8 | none; observed 1440 | 80 / 16 | 3 / 2 | 156 / 88 | n / n |
| Footshop | 4 | 0 | 4 | 0 | 0 | n/m (no open sheet sample) | 70 / 56 | 15 / 10 | none; observed 1440 | 24 / 12 | 2 / 2 | 113 / 103 | n / n |
| Vuch | 0 | 0 | 0 | 50% (computed; not uniform px) | 3 | n/m (no open sheet sample) | n/m (fewer than two measurable sections) / n/m (fewer than two measurable sections) | 16 / 8 | 1684px; observed 1425 | 20 / 16 | 2 / 2 | 153 / 150 | n / n |
| **Medians across Shopify demos** | 0 | 0 | 0 | 4.5 | 0 | 0 | 82 / 65 | 18.19 / 12 | 1400 | 48 / 10 | 2 / 2 | 100.5 / 80.98 | — |


## Color

Desktop samples. Mobile/alternate section schemes remain in raw JSON; current-price labels are not treated as proof of a sale.

| Target | Page bg | Body text | Purchase/ATC bg | Label text | Contrast | Sale-price color | Purchase label measured |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Horizon | #ffffff | #000000 α=0.81 | #000000 | #ffffff | 21:1 | n/m (no visible identifiable sale price) | Add to cart (1) |
| Horizon Atelier | #ffffff | #000000 | #000000 | #ffffff | 21:1 | n/m (no visible identifiable sale price) | Add to cart (1) |
| Horizon Ritual | #ffffff | #000000 | #000000 | #ffffff | 21:1 | n/m (no visible identifiable sale price) | Add to cart (1) |
| Horizon Fabric | #ffffff | #030302 α=0.76 | #030302 | #ffffff | 20.63:1 | n/m (no visible identifiable sale price) | Add to cart (1) |
| Prestige (Couture) | #ffffff | #5c5c5c | #ffffff | #5c5c5c | 6.69:1 | n/m (no visible identifiable sale price) | Add to cart |
| Symmetry | #ffffff | #232323 | #ffffff | #232323 | 15.72:1 | #232323 | Add to cart |
| Be Yours | #ffffff | #000000 | #ffffff | #000000 | 21:1 | #ff2d16 | Add to cart |
| Concept | #ffffff | #171717 | #171717 | #ffffff | 17.93:1 | n/m (no visible identifiable sale price) | Add to cart - $3,149.00 |
| Impact | #f0f0f0 | #1a1a1a | #4d523c | #ffffff | 8.12:1 | n/m (no visible identifiable sale price) | Add to cart |
| Glossier | #ffffff | #000000 | #e8e8e8 | #000000 | 17.14:1 | n/m (no visible identifiable sale price) | Add to bag €115 |
| Rothy's | #ffffff | #03143b | #ffffff | #03143b | 17.99:1 | n/m (no visible identifiable sale price) | Select a size |
| Tecovas | #fcf9f4 | #000000 | #a94619 | #ffffff | 5.87:1 | n/m (no visible identifiable sale price) | Select Your Size |
| Footshop | #ffffff | #5a5a5a | #ffffff | #5a5a5a | 6.9:1 | n/m (no visible identifiable sale price) | Vyberte velikost |
| Vuch | #ffffff | #000000 | #86294a | #ffffff | 8.64:1 | n/m (no visible identifiable sale price) | Přidat do košíku |
| **Medians across Shopify demos** | — | — | — | — | 20.63:1 | — | — |


## Card anatomy & PDP

Anatomy is ordered by rendered vertical position. Controls overlaid on the image therefore appear before the text stack. Compare-at/rating/quick-add are listed only when present in the sampled card; absence is not a statement about theme feature support.

| Target | Card: top → bottom | Home columns d/m | Collection columns d/m | Desktop gallery : info | Variants | Mobile sticky ATC | Returns/delivery near ATC |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Horizon | image 0.8:1; no badge in sample → name → price; swatches n; collection sample | 4 / 2 | 5 / 2 | 909.33:466.66 px (1.95:1); gallery block / none | buttons | n (at 2 screens) | n |
| Horizon Atelier | image 0.8:1; no badge in sample → name → price; swatches n; collection sample | 1 / 1 | 5 / 2 | 708:680 px (1.04:1); gallery block / none | n/m (no variant selector identified on sample) | n (at 2 screens) | n |
| Horizon Ritual | image 0.8:1; no badge in sample → name → price; swatches n; collection sample | 1 / 1 | 3 / 2 | 909.33:466.66 px (1.95:1); gallery block / none | buttons | n (at 2 screens) | n |
| Horizon Fabric | image 0.8:1; no badge in sample → name → price; swatches n; collection sample | 4 + overflow / n/m (grid not identified) | 5 / 2 | 913.33:466.66 px (1.96:1); gallery block / none | buttons | n (at 2 screens) | n |
| Prestige (Couture) | image 0.7:1; badge bottom-left, bg #ffffff, r 0px → name → price → rating; swatches y; collection sample | n/m (grid not identified) / 1 + overflow | 3 / 2 | 821.59:442.39 px (1.86:1); gallery flex / none | buttons | y | n |
| Symmetry | image 0.67:1; no badge in sample → name → price → rating; swatches n; collection sample | 4 + overflow / 2 + overflow | 4 / 2 | 603:490 px (1.23:1); gallery block / none | buttons | y | n |
| Be Yours | image 0.8:1; badge top-left, bg #ffffff, r 0px → name → price; swatches n; collection sample | 3 / 1 + overflow | 4 / 1 + overflow | 650:650 px (1:1); gallery block / none | n/m (no variant selector identified on sample) | y | n |
| Concept | image 1:1; no badge in sample → rating → name → price; swatches y; collection sample | n/m (grid not identified) / n/m (grid not identified) | 4 / 2 | 832.19:428.7 px (1.94:1); gallery grid / 523.984px 288.203px | buttons | y | n |
| Impact | image 1:1; badge top-left, bg #ffffff, r 0px → name → rating → price; swatches y; collection sample | 4 + overflow / 1 + overflow | 4 / 2 | 720:480 px (1.5:1); gallery grid / 64px 608px | buttons / swatch links | y | n |
| Glossier | image 0.8:1; no badge in sample → name → price → compare-at → quick-add; swatches n; collection sample | 3 / 1 + overflow | 4 / 2 | 932:400 px (2.33:1); gallery block / none | n/m (no variant selector identified on sample) | y | n |
| Rothy's | image 0.75:1; badge top-left, bg #ffffff, r 0px → quick-add → name → price; swatches n; collection sample | 2 / 3 | 4 / 2 | 888:448 px (1.98:1); gallery block / none | buttons | n (at 2 screens) | y |
| Tecovas | image 0.8:1; badge top-left, bg #696f42 α=0.93, r 2px → name → price → quick-add; swatches n; collection sample | n/m (grid not identified) / n/m (grid not identified) | 3 / 2 | 613:450 px (1.36:1); gallery block / none | buttons | n (at 2 screens) | n |
| Footshop | image 1:1; no badge in sample → name → price; swatches n; collection sample | 2 / 1 | 4 / 2 | 873.59:470.39 px (1.86:1); gallery block / none | custom dropdown (opened and verified desktop) | n (at 2 screens) | y |
| Vuch | image 0.77:1; no badge in sample → name → price; swatches n; collection sample | 4 + overflow / 1 + overflow | 4 / 2 | 712.5:672.5 px (1.06:1); gallery block / none | custom color tiles (linked products) | n (at 2 screens) | y |
| **Medians across Shopify demos** | — | 4 / 1 | 4 / 2 | 821.59:466.66 px; ratio 1.86:1 | — | — | — |


## Lighthouse mobile

| Target | LCP seconds | TBT ms | CLS | JS transfer kB | Font transfer kB | Font files | Runs |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Horizon | 6.63 | 1.5 | 0.0000 | 436.02 | 211.04 | 4 | 3 |
| Horizon Atelier | 6.33 | 27 | 0.0000 | 492.59 | 150.99 | 3 | 3 |
| Horizon Ritual | 5.37 | 27 | 0.0000 | 476.69 | 120.73 | 4 | 3 |
| Horizon Fabric | 6.2 | 29 | 0.0000 | 472.6 | 185.37 | 5 | 3 |
| Prestige (Couture) | 9.18 | 39.5 | 0.0000 | 575.63 | 44.73 | 4 | 3 |
| Symmetry | 11.72 | 174 | 0.0003 | 631.98 | 165.72 | 6 | 3 |
| Be Yours | 14.06 | 196 | 0.0000 | 583.98 | 132.71 | 6 | 3 |
| Concept | 7.1 | 137.5 | 0.0000 | 475.27 | 227.83 | 5 | 3 |
| Impact | 10.61 | 41 | 0.0000 | 566.53 | 114.57 | 3 | 3 |
| Glossier | 12.97 | 505.5 | 0.0736 | 2969.9 | 119.58 | 7 | 3 |
| Rothy's | 36.4 | 964.5 | 0.0252 | 3545.89 | 143.17 | 4 | 3 |
| Tecovas | 6.13 | 165 | 0.0016 | 1403.64 | 161.8 | 5 | 3 |
| Footshop | 9.04 | 841 | 0.1844 | 2360.18 | 93.58 | 3 | 3 |
| Vuch | 6.63 | 686 | 0.0002 | 1724.66 | 149.36 | 4 | 3 |
| **Medians across Shopify demos** | 7.1 | 39.5 | 0.0000 | 492.59 | 150.99 | 4 | 3 each |


## Exact URLs used

### Horizon

- Official preset page: [horizon](https://themes.shopify.com/themes/horizon/presets/horizon). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://theme-horizon-demo.myshopify.com/).

- Collection: [exact URL](https://theme-horizon-demo.myshopify.com/collections/all).

- PDP: [exact URL](https://theme-horizon-demo.myshopify.com/products/michael-shaggy-wool-cardigan-324-1?variant=50385529667905).

- Lighthouse input: [home URL](https://theme-horizon-demo.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) horizon-home-top-1440.jpg), [mobile home](refs/horizon-home-top-390.jpg), [desktop two screens down](refs/horizon-home-down-1440.jpg), [mobile two screens down](refs/horizon-home-down-390.jpg), [desktop PDP](refs/horizon-pdp-top-1440.jpg), [mobile PDP](refs/horizon-pdp-top-390.jpg).



### Horizon Atelier

- Official preset page: [atelier](https://themes.shopify.com/themes/atelier/presets/atelier). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://theme-atelier-demo.myshopify.com/).

- Collection: [exact URL](https://theme-atelier-demo.myshopify.com/collections/bags-1).

- PDP: [exact URL](https://theme-atelier-demo.myshopify.com/products/mini-isla-suede-caramel?variant=50126854848810).

- Lighthouse input: [home URL](https://theme-atelier-demo.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) atelier-home-top-1440.jpg), [mobile home](refs/atelier-home-top-390.jpg), [desktop two screens down](refs/atelier-home-down-1440.jpg), [mobile two screens down](refs/atelier-home-down-390.jpg), [desktop PDP](refs/atelier-pdp-top-1440.jpg), [mobile PDP](refs/atelier-pdp-top-390.jpg).



### Horizon Ritual

- Official preset page: [ritual](https://themes.shopify.com/themes/ritual/presets/ritual). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://theme-ritual-demo.myshopify.com/).

- Collection: [exact URL](https://theme-ritual-demo.myshopify.com/collections/new).

- PDP: [exact URL](https://theme-ritual-demo.myshopify.com/products/rose-11-bag-1?variant=45042658443453).

- Lighthouse input: [home URL](https://theme-ritual-demo.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) ritual-home-top-1440.jpg), [mobile home](refs/ritual-home-top-390.jpg), [desktop two screens down](refs/ritual-home-down-1440.jpg), [mobile two screens down](refs/ritual-home-down-390.jpg), [desktop PDP](refs/ritual-pdp-top-1440.jpg), [mobile PDP](refs/ritual-pdp-top-390.jpg).



### Horizon Fabric

- Official preset page: [fabric](https://themes.shopify.com/themes/fabric/presets/fabric). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://theme-fabric-demo.myshopify.com/).

- Collection: [exact URL](https://theme-fabric-demo.myshopify.com/collections/womenswear-children-only).

- PDP: [exact URL](https://theme-fabric-demo.myshopify.com/products/tefnut-beach-vest-in-black?variant=44434289131573).

- Lighthouse input: [home URL](https://theme-fabric-demo.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) fabric-home-top-1440.jpg), [mobile home](refs/fabric-home-top-390.jpg), [desktop two screens down](refs/fabric-home-down-1440.jpg), [mobile two screens down](refs/fabric-home-down-390.jpg), [desktop PDP](refs/fabric-pdp-top-1440.jpg), [mobile PDP](refs/fabric-pdp-top-390.jpg).



### Prestige (Couture)

- Official preset page: [couture](https://themes.shopify.com/themes/prestige/presets/couture). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://prestige-theme-couture.myshopify.com/).

- Collection: [exact URL](https://prestige-theme-couture.myshopify.com/collections/shop).

- PDP: [exact URL](https://prestige-theme-couture.myshopify.com/products/edie-cascade-wrap-mini-dress-black).

- Lighthouse input: [home URL](https://prestige-theme-couture.myshopify.com); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) prestige-home-top-1440.jpg), [mobile home](refs/prestige-home-top-390.jpg), [desktop two screens down](refs/prestige-home-down-1440.jpg), [mobile two screens down](refs/prestige-home-down-390.jpg), [desktop PDP](refs/prestige-pdp-top-1440.jpg), [mobile PDP](refs/prestige-pdp-top-390.jpg).



### Symmetry

- Official preset page: [symmetry](https://themes.shopify.com/themes/symmetry/presets/symmetry). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://chantilly.myshopify.com/).

- Collection: [exact URL](https://chantilly.myshopify.com/collections/new-in).

- PDP: [exact URL](https://chantilly.myshopify.com/collections/best-sellers/products/sunsets-seaaq-77-forever-tankini).

- Lighthouse input: [home URL](https://chantilly.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) symmetry-home-top-1440.jpg), [mobile home](refs/symmetry-home-top-390.jpg), [desktop two screens down](refs/symmetry-home-down-1440.jpg), [mobile two screens down](refs/symmetry-home-down-390.jpg), [desktop PDP](refs/symmetry-pdp-top-1440.jpg), [mobile PDP](refs/symmetry-pdp-top-390.jpg).



### Be Yours

- Official preset page: [be-yours](https://themes.shopify.com/themes/be-yours/presets/be-yours). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://beyours-theme-beauty.myshopify.com/).

- Collection: [exact URL](https://beyours-theme-beauty.myshopify.com/collections/all).

- PDP: [exact URL](https://beyours-theme-beauty.myshopify.com/collections/dr-elowen-liz/products/glow-drops).

- Lighthouse input: [home URL](https://beyours-theme-beauty.myshopify.com); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) be-yours-home-top-1440.jpg), [mobile home](refs/be-yours-home-top-390.jpg), [desktop two screens down](refs/be-yours-home-down-1440.jpg), [mobile two screens down](refs/be-yours-home-down-390.jpg), [desktop PDP](refs/be-yours-pdp-top-1440.jpg), [mobile PDP](refs/be-yours-pdp-top-390.jpg).



### Concept

- Official preset page: [concept](https://themes.shopify.com/themes/concept/presets/concept). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://concept-theme-tech.myshopify.com/).

- Collection: [exact URL](https://concept-theme-tech.myshopify.com/collections/all).

- PDP: [exact URL](https://concept-theme-tech.myshopify.com/products/echo-elegance).

- Lighthouse input: [home URL](https://concept-theme-tech.myshopify.com); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) concept-home-top-1440.jpg), [mobile home](refs/concept-home-top-390.jpg), [desktop two screens down](refs/concept-home-down-1440.jpg), [mobile two screens down](refs/concept-home-down-390.jpg), [desktop PDP](refs/concept-pdp-top-1440.jpg), [mobile PDP](refs/concept-pdp-top-390.jpg).



### Impact

- Official preset page: [impact](https://themes.shopify.com/themes/impact/presets/impact). Demo link extracted from its `data-demo-store-iframe-url-value` attribute.

- Home: [exact URL](https://impact-theme-sound.myshopify.com/en-fr).

- Collection: [exact URL](https://impact-theme-sound.myshopify.com/en-fr/collections/headphones).

- PDP: [exact URL](https://impact-theme-sound.myshopify.com/en-fr/products/mw08-sport-green-sapphire-glass-black-kevlar-r-case).

- Lighthouse input: [home URL](https://impact-theme-sound.myshopify.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) impact-home-top-1440.jpg), [mobile home](refs/impact-home-top-390.jpg), [desktop two screens down](refs/impact-home-down-1440.jpg), [mobile two screens down](refs/impact-home-down-390.jpg), [desktop PDP](refs/impact-pdp-top-1440.jpg), [mobile PDP](refs/impact-pdp-top-390.jpg).



### Glossier

- Home: [exact URL](https://www.glossier.com/en-sk).

- Collection: [exact URL](https://www.glossier.com/en-sk/collections/skincare).

- PDP: [exact URL](https://www.glossier.com/en-sk/products/glossier-holiday-you-duo?variant=48774147571957).

- Lighthouse input: [home URL](https://glossier.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) glossier-home-top-1440.jpg), [mobile home](refs/glossier-home-top-390.jpg), [desktop two screens down](refs/glossier-home-down-1440.jpg), [mobile two screens down](refs/glossier-home-down-390.jpg), [desktop PDP](refs/glossier-pdp-top-1440.jpg), [mobile PDP](refs/glossier-pdp-top-390.jpg).



### Rothy's

- Home: [exact URL](https://rothys.com/en-at).

- Collection: [exact URL](https://rothys.com/en-at/collections/womens-new-arrivals).

- PDP: [exact URL](https://rothys.com/en-at/products/womens-point-flat-iii-revelvet-russet).

- Lighthouse input: [home URL](https://rothys.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) rothys-home-top-1440.jpg), [mobile home](refs/rothys-home-top-390.jpg), [desktop two screens down](refs/rothys-home-down-1440.jpg), [mobile two screens down](refs/rothys-home-down-390.jpg), [desktop PDP](refs/rothys-pdp-top-1440.jpg), [mobile PDP](refs/rothys-pdp-top-390.jpg).



### Tecovas

- Home: [exact URL](https://www.tecovas.com/).

- Collection: [exact URL](https://www.tecovas.com/shop/c/tecovas-x-mossy-oak).

- PDP: [exact URL](https://www.tecovas.com/products/the-cartwright?color=tobacco-regenerative-bison).

- Lighthouse input: [home URL](https://tecovas.com/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) tecovas-home-top-1440.jpg), [mobile home](refs/tecovas-home-top-390.jpg), [desktop two screens down](refs/tecovas-home-down-1440.jpg), [mobile two screens down](refs/tecovas-home-down-390.jpg), [desktop PDP](refs/tecovas-pdp-top-1440.jpg), [mobile PDP](refs/tecovas-pdp-top-390.jpg).



### Footshop

- Home: [exact URL](https://www.footshop.cz/cs/).

- Collection: [exact URL](https://www.footshop.cz/cs/1551-novinky).

- PDP: [exact URL](https://www.footshop.cz/cs/trenky/555793-adidas-woven-boxer-2-pack-black-white.html).

- Lighthouse input: [home URL](https://footshop.cz/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) footshop-home-top-1440.jpg), [mobile home](refs/footshop-home-top-390.jpg), [desktop two screens down](refs/footshop-home-down-1440.jpg), [mobile two screens down](refs/footshop-home-down-390.jpg), [desktop PDP](refs/footshop-pdp-top-1440.jpg), [mobile PDP](refs/footshop-pdp-top-390.jpg).



### Vuch

- Home: [exact URL](https://www.vuch.cz/).

- Collection: [exact URL](https://www.vuch.cz/damske-novinky/).

- PDP: [exact URL](https://www.vuch.cz/cara/).

- Lighthouse input: [home URL](https://vuch.cz/); final URLs per run recorded in JSON.

- Screenshots: [desktop home](research_notes/Storefront theme inspiration/refs/ (local only, not committed) vuch-home-top-1440.jpg), [mobile home](refs/vuch-home-top-390.jpg), [desktop two screens down](refs/vuch-home-down-1440.jpg), [mobile two screens down](refs/vuch-home-down-390.jpg), [desktop PDP](refs/vuch-pdp-top-1440.jpg), [mobile PDP](refs/vuch-pdp-top-390.jpg).



## Measurement gaps and limits

No target failed wholesale: all homes, sampled PDPs, and sampled collections returned usable pages. `n/m` values are field-specific and explained in their cells.

- **Horizon:** no visible semantic home H1.

- **Horizon Atelier:** no rendered chip/variant-button sample.

- **Horizon Ritual:** no visible semantic home H1.

- **Horizon Fabric:** no visible semantic home H1.

- **Prestige (Couture):** no visible semantic home H1.

- **Symmetry:** no visible semantic home H1; open drawer/sheet not successfully sampled.

- **Be Yours:** no rendered chip/variant-button sample; open drawer/sheet not successfully sampled.

- **Concept:** no visible semantic home H1; open drawer/sheet not successfully sampled.

- **Impact:** no visible semantic home H1.

- **Glossier:** no visible semantic home H1; no rendered chip/variant-button sample; open drawer/sheet not successfully sampled.

- **Rothy's:** no visible semantic home H1; no rendered section-heading sample; open drawer/sheet not successfully sampled.

- **Footshop:** open drawer/sheet not successfully sampled.

- **Vuch:** no visible semantic home H1; open drawer/sheet not successfully sampled.


## Observed patterns

- **Editorial scale varies substantially.** Atelier uses a 120/72 px home H1 and 48/28 px PDP title. Prestige keeps its home hero text at 32/22.15 px and PDP title at 22/18.06 px. Horizon’s hero is an H2 at 56/48 px; Ritual/Fabric foreground brand graphics rather than a measurable content H1.

- **The three DTC demos give product names more weight in the card hierarchy.** Be Yours uses 16/13 px card names and an 18 px card price; Concept uses 18.54/16 px names and 15.27/14 px prices; Impact uses 16/14 px for both. Prestige’s sampled names/prices are 12 px, while Atelier’s are 12 px.

- **Pill CTAs are a choice, not a universal premium rule.** Concept and Impact declare 60 px purchase-button radii. Be Yours, Atelier, Ritual, Prestige, and Symmetry declare 0 px; Horizon uses 14 px and Fabric 2 px. Impact also uses 4 px image swatch tiles; Concept’s sampled variant chip is 5 px.

- **Image surfaces remain mostly square at the corners.** The sampled Shopify card-image wrappers all declare 0 px radius. Concept’s outer card is 15.16 px and Impact’s is 6 px, so wrapper and container values should be considered separately. Resting zero-size shadow declarations are not visible shadows.

- **Mobile density generally converges toward two grid tracks.** Horizon/Atelier/Fabric have five desktop collection tracks; Ritual/Prestige have three; Concept/Impact have four. These sampled grids become two tracks at 390 px. Be Yours’ first sampled collection product strip is a carousel, with one complete mobile card plus overflow, rather than a two-column grid.

- **DTC demos expose a persistent mobile purchase control in this scroll check.** Be Yours, Concept, and Impact show a fixed ATC after two screens of scrolling. Prestige and Symmetry also do; the four Horizon-family sampled PDPs do not in this observation.

- **Home whitespace is not uniformly tighter in the DTC group.** The measured content-edge section-gap medians are 282.4/184.3 px for Be Yours, 313.5/140 px for Concept, and 168/120 px for Impact; Horizon is 48/33.59 px and Prestige 128/65 px. These describe actual section compositions under the stated method, not reusable theme spacing tokens.

- **Appearance and lab speed are separate measurements.** Across the nine Shopify demo homepages, the median of three-run mobile LCP medians is 7.10 s, TBT 39.5 ms, transferred JS 492.59 kB, and fonts 150.99 kB / 4 files. Among real stores, Rothy’s LCP median is 36.40 s in this run environment. Third-party scripts, consent flows, assets, localization, and demo content are included; this is not an isolated theme benchmark.
