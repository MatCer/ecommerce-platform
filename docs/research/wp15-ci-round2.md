# WP15 CI failures, run 36220248489

Evidence: `/tmp/wp15-ci-acceptance.log`, and
`/tmp/wp15-ci-artifacts/acceptance-evidence/` (Compose log, queue samples,
Playwright traces and error contexts). Line numbers below refer to those files.

## Theme test: loading race, not a ten-minute build

The failed CI log, lines 275–281, says:

```text
waiting for getByRole('button', { name: 'Create my own theme' })
element was detached from the DOM, retrying
```

The trace spends 603,591 ms in that click. It never reaches `settled()` or submits
the fork/reset request. `Themes.tsx` treated an unloaded revision list as empty,
rendering the fork button until the response revealed a custom revision and
replaced it with reset. The test's immediate `isVisible()` branch kept looking
for the old label. The parallel AI suite creates custom revisions on the same
demo tenant, and both suites also publish and derive revision numbers from shared
state. A locator change alone would leave those other races intact.

Compose evidence rules out queue starvation as this failure's cause:

- Line 29944: `05:50:39.806621Z`, `job started`, `queue:"themes-ai"`,
  `kind:"themes.ai_edit"`.
- Lines 29949–29951: `themes.build` claimed on `queue:"default"` at
  `05:50:40.053335Z`, completed in `0.009242223` seconds. This queued notification
  is not the duration of the sandbox build owned by the AI run.
- Line 30885: the AI job finishes in `286.530566034` seconds.
- Line 863: builder reports `status:"ready", failures:0` for that AI revision.
- `.acceptance/queue.log`, lines 557–561: AI running, no queued theme build;
  final sample at `06:00:49.124682+00` has no theme jobs and zero outbox lag.
- Compose line 1: sandbox proxy uses `acceptance-theme-builder:local`.

The builder image is present and completed real gates. Its Dockerfile installs
locked dependencies and Chromium in the image; sandbox builds do not require a
host pnpm store. CI cold image construction happens during `make up`, before the
tests. Worker code keeps the configured default concurrency, adds one media loop,
and runs AI loops separately; media did not take away a default slot.

Fix: hide theme mutations until the list succeeds; use a live locator matching
either valid action with a ten-second click deadline. A held-response browser
regression verifies the loading state. Run the two mutating suites in one-worker
`chromium-themes`, after regular Chromium tests, so storefront readers cannot
observe their temporary publications. This uses Playwright's supported
[per-project worker limit](https://playwright.dev/docs/api/class-testproject#test-project-workers),
within the existing global four-worker cap. No build deadline increased.

## Data test: total workflow budget

The trace progresses through the entire workflow, with the timeout firing during
the final accessibility scan. Its test body continues briefly during teardown:
start 88,828 ms, last assertion 151,535 ms (62.7 seconds). Five axe scans consume
30.2 seconds (4.88, 7.59, 7.29, 6.25, 4.14); six import readiness waits consume
14.6 seconds; export readiness consumes 3.68 seconds. No single operation stalls.

Compose lines 28383–28387 show `data.export` starts at `05:48:57.765213Z` and
finishes in `1.723955541` seconds. The request was accepted at `05:48:57.422728Z`
(line 5205), so it waited about 0.34 seconds for a worker, not sixty seconds.

Fix: only this complete workflow gets 120 seconds, approximately twice measured
latency, retaining individual operation deadlines and all five accessibility scans.

## Axe browser integration budget

CI log lines 67–69 report the sticky fixture at `5002ms` (timeout) and the footer
fixture at `1473ms`. The sticky fixture performs three full scans and six centered
target checks, including scroll settling and browser injection on each check.
This is a real browser integration test, not a millisecond unit test.

Local baseline: sticky 3,520 ms, footer 958 ms. Narrowing centered checks to the
only relevant rule (`target-size`) gives sticky 3,389 ms, footer 957 ms. Keep all
rules in the three primary scans and deduplicate identical scroll positions.
The small measured saving does not justify retaining the fragile five-second
deadline: give only the sticky test 15 seconds, documented beside the test.
The existing faulty-sticky and clipped-footer fixtures remain regression checks.

No blanket retries, skipped tests, relaxed accessibility rules at primary scan
positions, or production queue/sandbox policy changes.

## Local verification

- Project `wp15`, existing `.env` and network override, ports 215xx only.
  Rust builds/test threads capped at six; Playwright capped at four.
- Held-response regression fails on the old admin (`Expected: 0; Received: 1`)
  and passes after rebuilding it.
- Focused data + both theme suites: 9 passed in 5.9 minutes. Data: 21.0 seconds;
  AI: 1.7 minutes; fork: 1.6 minutes; token path: 40.9 seconds. Includes hostile
  archives, blocked network access, publish/rollback and JS-budget rejection.
- Rebuilt the full stack from the current source before final acceptance.
- `make lint test`: passed, 682 Rust tests and 359 TypeScript tests. Nine existing
  Rust tests remain ignored by the default target; no test was newly ignored.
  Includes the added non-target-size accessibility regression on a short page.
- Full `make e2e args='--workers=4'` against rebuilt images: 99 passed in 7.1
  minutes, no retries/skips. All 91 regular tests finish before the eight theme
  tests. Data: 21.4 seconds; AI: 1.7 minutes; fork: 1.6 minutes.
- Logs: `/tmp/wp15-round2-checks.log`, `/tmp/wp15-round2-full-e2e.log`,
  `/tmp/wp15-round2-compose.log`.
- `COMPOSE_PROFILES=full docker compose -p wp15 down -v` completed; project-label
  checks show no remaining `wp15` containers or volumes. Nothing pushed.

Remaining risk: this verifies the full local Compose workflow, not a new hosted
GitHub runner. The measured budgets retain headroom for CI contention, and the
existing ten-minute theme deadline remains unchanged.
