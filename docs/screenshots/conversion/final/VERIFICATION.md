# Conversion refresh: verification (SHOP-89, 2026-10-08)

Dev stack rebuilt from `main` after PR #43 (`make up`, `make theme-build`). Before = SHOP-75
baseline in `docs/screenshots/conversion/*.jpg`, after = this folder.

## Gates

| Check | Result |
|---|---|
| `make perf` (Lighthouse mobile, median of 3) | `/` 1579 ms (warn: over the 1.5 s target, under the 2 s limit), `/c/trika` 1351, `/p/tricko-basic` 1426, `/search` 1351; TBT 0, CLS 0 |
| JS gzip (incl. RUM-sampled visit) | at most 28.3 kB (29.6 with RUM) of 30; home 23.6 of 35 |
| axe WCAG 2.2 AA, page scan at three scroll positions | 0 violations on all four pages |
| axe with open states (cart 390/1440, nav sheet 390/1440, filter sheet, search sheet) | 0 violations |
| CSP violations, third-party origins | 0, 0 |
| Theme smoke (`packages/theme-kit/src/smoke.ts`) | ok, storefront calls 4 / 2 / 3 per render |
| Full e2e (`make e2e`) | 114 passed |

## Audits

- **Omnibus:** a crawler checked every product card on home, categories and search, and every PDP linked from them. CZ: 111 cards (39 reduced) and 61 PDPs (16 reduced). SK (`demo-sk`): 82 cards (54 reduced) and 40 PDPs (16 reduced). Every struck price has its "lowest price in 30 days" line next to it. The phone buy bar never shows a struck price.
- **Fake urgency:** the theme has no timers, countdowns, viewer counts or invented stock numbers. Stock words ("Poslední kusy", "Na objednávku") come from the variant's real `stock` state.
- **Checkout tokens:** checkout reads `identity`, `identity-ink`, `identity-wash`, `muted-foreground`, `sale`, `foreground`, `background`, `font-sans`, plus classes on `buy`, `buy-hover`, `card`, `muted`, `border` and `input`. All are present.
  - Text contrast: worst pair is sale on sale-wash at 5.00:1.
  - Card on buy is 7.38:1.
  - Input border on card is 3.42:1, above the 3:1 needed for non-text contrast.
  - Checkout renders with the new tokens (`checkout-mobile.png`), and the checkout e2e specs pass.
- **Keyboard:** the e2e keyboard-only specs pass: skip link, header, variant radios, cart, Escape returns focus, and product → cart → COD checkout.
- **Screen reader (accessibility tree):**
  - One h1 per page. Named landmarks. No unnamed controls, except "Hlídat", which only reads empty because its `<details>` is closed.
  - The cart dialog read as a second banner and contentinfo landmark (`<header>`/`<footer>` inside the modal). Fixed: plain divs.
  - Category pages put the "Kategorie" h2 before the h1 because the sidebar comes first in DOM order. This is best practice, not a WCAG failure, and was left as is.
- **Not done:** a real screen reader on a real device (VoiceOver or TalkBack). That is part of STOP 4, the owner's phone walk-through.
