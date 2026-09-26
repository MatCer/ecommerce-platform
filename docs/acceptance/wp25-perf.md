# WP25 PDP performance investigation (2026-09-26)

## Comparison

Measured `origin/main` (`a9deef0`) and WP25 (`e798382`) serially on the same host,
using only Compose project `wp25`, its existing ignored `.env`/network override,
225xx ports, and `CARGO_BUILD_JOBS=6`. Main was exported to a temporary directory
and its full stack built and started under the same project name. The comparison
retained the database, media and theme artifact, initially populated by `make seed`
plus CI's `scripts/seed-m2-perf-review.sh` fixture. No concurrent builds/tests during gates.

Every row below is the unchanged `make perf`: HTTPS/h2, the existing Lighthouse
mobile preset, three runs per page, median, and the original 1500 ms budget.

| Revision | Home | Category | PDP | Search |
| --- | ---: | ---: | ---: | ---: |
| WP25 before | 1203 | 1276 | 1428 | 1126 |
| Main | 1204 | 1277 | 1427 | 1127 |
| WP25 with AVIF quality 50 | 1204 | 1202 | 1353 | 1127 |

PDP individual LCP samples, ms:

- WP25 before: 1428.3, 1426.8, 1502.2.
- Main: 1426.9, 1426.5, 1426.6.
- WP25 repeat with `--explain`: 1427.6, 1426.8, 1501.4.
- WP25 after: 1427.0, 1352.3, 1352.7.

There is no measurable branch median regression locally (1 ms). Individual
WP25 samples reproduce the approximately 1502 ms timing band seen in CI. Main's
category/search samples also fluctuate by approximately 75 ms. These local
measurements do not establish the exact cause of CI's median changing bands;
they do identify insufficient transfer-budget headroom in the existing PDP.

## Critical path and experiments

Lighthouse identifies the first gallery image as LCP. The browser correctly
selects the 720 px AVIF for a 380 CSS px slot at DPR 1.75. It is already preloaded,
eager, and high priority. Its 57,939-byte body competes with a roughly 23 kB
preloaded font, 10 kB stylesheet and 21 kB document transfer.

The warm pool path does not acquire the cold-admission lock. Timeout eviction
runs only on timeout, error redaction only on errors, and the isolated functional
preview proxy is outside the storefront route. Cached HTML avoids worker
dispatch. The new edge CPU cap recorded zero throttled periods/time over both
the full before gate and the repeated PDP probe. All security changes are kept.

Discarded experiments:

- Low-priority font preload changed Chrome's priority but not median LCP (1428).
- Removing the font preload worsened LCP to 1502; the original preload is kept.
- Slower AVIF encoding at unchanged quality saved less than 1% on the exact
  seeded image, so encoder speed remains 8.

## Fix and limits

`crates/commerce/src/media/encode.rs` changes AVIF quality from 60 to 50 for normal
media processing. No fixtures, responsive dimensions, theme code, measurement,
budgets or security controls change. The same seeded image becomes 36,159 bytes
(-37.6%). A browser-rendered side-by-side comparison at its 380 px displayed width
retains the garment's edges, colour and visible texture. This is still a lossy
quality tradeoff; other detailed photography may show greater differences.

The final stack was rebuilt and reseeded through the normal upload pipeline,
including the published-review fixture. The theme artifact remained
`4117ccd1721fe28849a6d980612093c2`. PDP median headroom grew from 72 to 147 ms;
the slowest final sample was 1427 ms, versus 1502 ms before. TBT and CLS remain
zero, PDP JS remains 28.0 kB gzip (29.3 with RUM), and page-model calls remain
three. Every performance, accessibility, CSP and third-party check passed.

This encoder setting only affects newly processed assets. Existing immutable
variants need regeneration/replacement to benefit; this change does not rewrite
stored media. CI must still rerun on its own hardware. The security review's
existing production launch blockers remain unchanged.

This is an encoder configuration change; a unit test asserting the quality
constant would duplicate the setting. Validation uses the real media pipeline,
visual comparison, existing media tests and the unchanged browser gate.

## Verification

- `make perf`: all four pages passed, as reported above.
- `pnpm exec vitest run --project edge --maxWorkers=2`: 148 tests passed.
- `make lint test`: rustfmt, clippy, Biome and TypeScript checks passed;
  686 Rust tests passed (9 ignored), 362 TypeScript tests passed.
- Final browser check: gallery image loaded at 380 CSS px, body 36,159 bytes,
  original font preload present.
- Only project `wp25` was used; final teardown removes its containers and volumes.
- No push performed.
