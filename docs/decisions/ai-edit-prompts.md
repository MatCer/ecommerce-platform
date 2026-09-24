# AI theme-edit feasibility (WP2, spec §12.3)

No Anthropic API key was available, so the "AI" was the implementing agent itself (Claude Opus
5.5) acting under the theme contract. Each prompt started from a fresh copy of `themes/default`,
and the agent could only touch `src/**`, `public/**` and `theme.tokens.json`, add no dependencies,
and get data only through `@platform/storefront-sdk`. Every change then went through the gates in
`scripts/theme-gates.sh`:

1. `theme-kit lint` (contract: no foreign fetch, no URL imports, no inline scripts or handlers, no
   cookie access, `set:html` only for sanitized HTML or JSON-LD, required routes, locked deps)
2. `astro check` (TypeScript)
3. `astro build` + `theme-kit pack` (artifact), then publish via channel pointer + edge purge
4. `theme-kit measure`: Lighthouse mobile (median LCP/TBT/CLS), A26 JS (network idle + full scroll),
   axe WCAG 2.1 AA, CSP violations, third-party origins, storefront calls per render
5. `theme-kit smoke`: Playwright browse → category → product → add to cart → checkout handoff

Gates ran against the local edge (Node + Miniflare) behind Caddy over TLS + HTTP/2, one Lighthouse
run per page. Patches: `docs/decisions/ai-edit/pNN.patch`.

## Prompts and results

| # | Merchant prompt (as a CZ/SK merchant would write it) | Result | Repairs | PDP LCP / JS gz | Changed lines |
|---|---|---|---|---|---|
| 1 | „Udělej produktovou stránku prémiovější: větší galerie a na desktopu přilepený box s cenou a tlačítkem.“ (premium PDP, bigger gallery, sticky buy box) | pass | 0 | 1202 ms / 24.4 kB | 12 |
| 2 | „Přidej k výběru velikosti tabulku velikostí ve vysouvacím panelu.“ (size-guide drawer) | pass | 0 | 1202 ms / 25.5 kB | 74 |
| 3 | „Ukaž odpočet, do kdy objednat, aby zboží odešlo ještě dnes.“ (dispatch countdown) | pass | 0 | 1277 ms / 25.0 kB | 51 |
| 4 | „Prelož texty obchodu do slovenčiny pre SK trh.“ (translate UI strings for SK) | pass | 0 | 1351 ms / 25.4 kB | 194 |
| 5 | „Na úvodní stránku dej lištu s dopravou zdarma od 1 500 Kč a výhody obchodu.“ (free-shipping bar + USPs) | pass | 0 | 1352 ms / 24.4 kB | 21 |
| 6 | „Změň barvy na tmavě zelenou a nadpisy dej patkovým písmem.“ (dark green brand, serif headings) | pass | 0 | 1201 ms / 24.4 kB | 31 |
| 7 | „Na kartách produktů ukaž při najetí myší druhou fotku.“ (second photo on hover) | **blocked** | 1 (no valid repair) | 1202 ms / 22.2 kB, **26 calls** on category | 28 |
| 8 | „Pridaj na produkt sekciu Naposledy prezerané.“ (recently viewed) | pass after repair | 1 | 1202 ms / 25.2 kB | 55 |
| 9 | „Do patičky přidej loga platebních metod a dopravců.“ (payment/carrier logos) | pass | 0 | 1202 ms / 24.4 kB | 20 |
| 10 | „Pod produkt přidej Často kupováno společně.“ (frequently bought together) | pass after SDK addition | 1 (contract) | 1351 ms / 24.4 kB, 3 calls | 17 |

LCP values are single Lighthouse runs (the simulated LCP moves in ~75 ms steps between runs);
every run stayed under 1.5 s, TBT was 0 ms, CLS ≤ 0.019, axe found 0 violations, no CSP
violations, no third-party origins.

**Pass rate:** 7/10 on the first attempt, 8/10 within the 3-cycle repair loop without platform
changes, 9/10 after one SDK addition (`recommendations()`, now in the SDK). 1/10 is not possible
within the contract (the data is not in the page model).

## What happened, per prompt

1. Layout and Tailwind only. The trap: the gallery's `sizes` and the LCP preload's `imagesizes`
   must change together with the grid; if they diverge the browser downloads two variants. The agent
   caught it by reading the layout, no gate checks it.
2. Solid island with a native `<dialog>`. The size chart is hard-coded in theme code: merchant
   content in code.
3. Client-only island (HTML is edge-cached, a server-rendered countdown would be stale). Cutoff hour,
   weekends and holidays are hard-coded. The countdown appears after hydration; on the mobile
   viewport it is below the fold, so Lighthouse CLS does not see the shift, and on desktop it would.
4. Cross-cutting: dictionary file plus a `locale` prop threaded into every island and card, across
   9 files. Both locales ship in every island chunk (+1.1 kB gz).
5. Straightforward. Copy hard-coded in Czech again (same i18n gap).
6. Token change in `theme.tokens.json` flows to the checkout origin automatically (`/_p/tokens.css`
   verified). Switching the display font meant removing the Archivo preload and `@font-face` by
   hand, because otherwise an unused font is still downloaded. A system serif stack was used because
   the agent cannot add web fonts (no network, and no platform font library).
7. The card model has one image. The only way the agent found within the contract was to fetch every
   product's page model during SSR: 24 extra binding calls per category render. The original
   §12.3 gates passed it. WP2 added a **storefront-calls-per-render budget** (edge
   `x-edge-subrequests`, gate max 10, hard cap 50), which then failed it. No valid repair exists
   without an API change. The hidden hover images also download (not budgeted).
8. First attempt passed **all gates but did not work**: a `client:visible` island that renders
   nothing on the server has no box to observe, so it never hydrates. Found only by a functional
   Playwright check; repaired with `client:idle`. Storage is gated on `personalization` consent
   (A20). Prices are snapshots from view time and go stale.
9. Neutral text badges (official logos are trademarks with usage rules).
10. The binding already exposed `/recommendations`, but the SDK had no method and the contract says
    "data only via the SDK", so the agent was blocked. WP2 added `recommendations(context)` and the
    change then passed.

## Feasibility verdict

AI editing within the contract is **feasible for presentation-layer requests** (layout, styling,
tokens, composition of existing page-model data, small islands). That covers most of the prompts
merchants actually write. It is **not feasible** where the request needs data the page models do
not carry, or platform concerns (i18n, fonts, logos). The gates are fast enough for the loop
(measured: lint 0.05 s, astro check 2.9 s, build 1.7 s, pack 0.07 s, measure 42 s for 3 pages
with one Lighthouse run each, smoke 2.6 s; about 50 s per cycle, so 3 repair cycles fit easily).

The gates catch performance and security regressions well. They do not catch **functional
regressions** (prompt 8), so the agent loop must also write and run a feature assertion (a small
Playwright check per request) before calling the change done.

## Required contract, SDK and gate additions

For WP6 (page models and SDK):
- `ProductCard.images` (at least 2) or `hover_image`; batch card lookup by ids (recently viewed, fresh prices).
- `ProductPage.size_guide` (or a CMS block reference); `delivery.dispatch_cutoff` + `next_dispatch_date`
  computed server-side (holidays, stock).
- SDK coverage for every binding operation (done for `recommendations` in WP2); a consent-gated
  storage helper (`consentStorage("personalization")`); a single `lcpImage()` helper that emits both
  the preload and the `<img>` attributes from one `sizes` value.
- Theme i18n: platform-provided message catalogs per market locale, with only the active
  locale sent to the client.

For WP8 (default theme):
- Build the default theme i18n-ready from the start. Every theme string goes through the catalog.
- A licensed payment/carrier icon set and a small self-hosted font library (latin-ext subsets) the
  theme can pick from via tokens.

For WP23 (builder and gates):
- Keep the new storefront-calls budget; add an image-bytes budget (Lighthouse `total-byte-weight`
  or image transfer until idle) and a desktop CLS run.
- Lint rules: `client:visible` on an island whose server render can be empty; preloads of assets
  the page no longer references.
- The agent must add a feature-level Playwright assertion per request (the smoke covers only the
  purchase path).
- Record per-revision: gates, repairs, tool turns, diff size (this table is the template).

Caveat: the agent had full repository knowledge and no tool-turn cap. A real 25-turn loop with
only the contract as context will do worse on cross-cutting prompts (4) and on performance traps (1, 7).
