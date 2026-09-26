# WP15 CI acceptance, round 3

## Root cause: ordering before the payment save completes

Evidence is `/tmp/wp15-ci-acceptance.log` and
`/tmp/wp15-ci-artifacts/acceptance-evidence/`. Trace references below are line
numbers inside the failed test's `trace.zip`, extracted to
`/tmp/wp15-round3-trace/`. Times are Playwright monotonic milliseconds.

- CI log lines 123–137: `Test timeout of 60000ms exceeded`,
  `Error: page.waitForURL: Test ended.`, pointing to
  `await p.waitForURL(/\/_p\/fake-pay\//)` in `placeOrder`, **before** the retry checks.
- `4-trace.network:51`: `PUT /_p/checkout/payment`, start `56635.995`,
  `time: 860.583`, response `200`. It completes at about `57496.578`.
- `4-trace.trace:311`: selecting the payment radio finishes at `56648.645`.
  That only waits for the click, not the asynchronous PUT.
- `4-trace.trace:378`: the order button is enabled (`<button type="submit" ...>`;
  no disabled attribute). Line 386: `performing click action` at `57079.016`,
  **443 ms after the payment request starts, 418 ms before it completes**.
- `4-trace.trace:391`: checkout stays on `/` and renders the alert
  `Vyplňte prosím všechny povinné údaje a vyberte dopravu a platbu.`
  (Fill all required details and choose shipping and payment.)
- `4-trace.trace:392–393`: the URL wait starts at `57118.965`, then consumes
  the remainder of the test. There is no contact/address/order submission
  in this shopper's network trace, and no fake-payment navigation to miss.
- Compose lines 2490/2492 corroborate successful shipping/payment saves:
  `06:51:47.118763Z`, `latency:10 ms`, and `06:51:48.167459Z`,
  `latency:652 ms`, both `status:200`.

`CheckoutForm.place` reads `view().payment_method`, populated only when the PUT
returns. The radio updates immediately, but the submit button previously disabled
only for `placing()`. Clicking it during `saving()` rejects an otherwise complete
checkout; nothing automatically re-submits when the save completes. This explains
why a 12-second passing workflow can intermittently stall for 60 seconds.

Fix: disable placement for `placing() || saving()`. The browser regression holds
the payment save response, verifies placement remains disabled, releases it,
then places and pays the order through the real checkout. Reverting just the
submit guard makes this regression fail (`Expected: disabled; Received: enabled`);
restoring it makes the workflow pass. This protects shoppers as
well as every test using this form. No timeout increases or test retries.

Hydration was an initial suspect: the email fill begins while the SSR marker is
present (`4-trace.trace:186`), but hydration has completed in the action snapshot
(line 206), before the filled value appears (line 209). That is not evidence of a
lost email in this failure. The measured pending payment save is the direct cause.
The audit nevertheless found and fixed independently reproducible SSR races.

Queue slots, retry backoff and the flow clock cannot cause this particular wait:
the purchase has not been submitted. Default workers are active in the same
interval: Compose lines 23801–23812 show four `adtracking.deliver` jobs start and
finish in 7–16 ms. The media lane added in round 1 remains separate from default
concurrency; no queue policy was changed in this round.

## Suite audit and fixes

Audited every e2e spec and shared helper for readiness, changing locators, mutable
tenant settings, fixed sleeps, and asynchronous completion assertions.

- **SSR controls:** checkout, sign-in, consent, password, sign-out, withdrawal
  request and payment retry controls were enabled before their handlers attached.
  Reuse `HydratedControls` from round 1. Add the existing `checkoutReady` helper to
  the ad-tracking checkout copy. Held-module tests cover checkout contact entry,
  sign-in, consent, withdrawal request, password, sign-out and payment retry;
  existing tests cover addresses, email-link confirmation and withdrawal form.
  Stripe's embedded payment element already has its own readiness gate.
- **Shared flow clock/settings:** `admin/flows.spec.ts:16–20` resets demo flow
  definitions; lines 60–61 temporarily change review delay to 150 hours. The
  checkout flow and review suites advance the same demo clock. File-local serial
  mode does not isolate these files. Put these three suites in one-worker
  `chromium-shared-state`, after the regular project.
- **Shared newsletter template:** admin newsletter temporarily saves
  `Potvrďte odběr {shop} ...`, while storefront newsletter waits for
  `/^Potvrďte odběr novinek/`. Put both in that same exclusive project. Theme
  mutation suites still run last, after shared-state tests.
- **Finite shared stock (found by the first full repeat):**
  `checkout/orders.spec.ts:254` waited for fake pay after a `409 cart_unavailable`.
  `/tmp/wp15-round3-local-failure/1-trace.network:56` records that response;
  its preceding address response marks the variant `available:false`,
  `stock:out_of_stock`. SQL showed `on_hand=30, reserved=30`, with successive
  reservations reaching 29 at `07:07:52.157061Z` and 30 at `07:07:52.779217Z`.
  Adding to a cart does not reserve inventory, so concurrent specs can choose
  the same last unit. A setup project now provisions at least 100 available
  units on the first variant of the five checkout fixture products, before
  parallel workers. This exceeds combined suite demand and works with existing
  reservations on repeated runs. It uses the real inventory adjustment API,
  preserving movements/outbox/cache invalidation and stock enforcement; the
  separate stock-watch product and sold-out catalogue fixtures are untouched.
- **Job assertions matching old runs:** ad delivery assertions now also check the
  purchase belonging to the current unique email and demo tenant, so an old
  cancelled/dead row cannot satisfy them. Replace the arbitrary three-second
  cancellation sleep with terminal cancellation checks before and after resume.
  Reset the Sklik mock in teardown even if the failure test aborts.
- **Index readiness versus cached HTML (found on the next repeat):** the import
  test polled `/search?q=hrnek` immediately after import application. Trace
  `/tmp/wp15-round3-search-failure/0-trace.network:22,83` shows identical empty
  HTML (`resources/7617d974f133cb92d598c46db080a48f355df4c3.html`) at
  `07:21:09.067Z` and `07:22:08.218Z`. Database evidence: import applied at
  `07:21:08.672837Z`, incremental indexing finished at `07:21:13.34208Z`, and
  rebuild at `07:21:13.505214Z`. The first HTML render preceded index readiness;
  edge TTL is 60 seconds plus stale-while-revalidate. The test exhausted its
  60-second poll on cached empty HTML. Poll the live search page model for the
  imported products first, then assert the same rendered HTML contains them.
  No cache-buster or longer timeout; the actual storefront assertion remains.
  Focused content suite: nine passed (19.4 seconds, including setup).
- **Exports/erasure from earlier work:** data export previously waited for any
  `Ready` row and downloaded the first ready export, potentially from a previous
  run. Capture the newly created export ID, poll that export's API status, reload
  the list and verify the UI requests that ID's download. Repeated privacy
  erasures now await the current successful response and dialog close, so the
  preceding success toast cannot satisfy the next erasure's completion check.
- **Lazy RUM observer:** replace two 500-ms sleeps with observation of the
  reporter's final PerformanceObserver subscription, a buffered LCP delivery,
  and the actual web-vital beacon. Test-only instrumentation; no production hook.
- **Loading/swapped buttons:** round 2's theme mutation readiness/live locator
  remains. Other immediate visibility branches are on already-loaded ad rows or
  the mounted consent banner, and keyboard sheet visibility checks drive tabbing.
  Other sleeps are bounded polling, TOTP-window alignment, or deliberate delayed
  hydration fault injection. They are not blind background-job completion waits.
- Remaining shared demo writes use unique records/products or future scheduled
  collections; catalog/content/AI/webhook suites use separate tenants. Shared staff sign-in links already use atomic per-link claims to prevent
  two workers consuming the same token. Remaining import, mail, delivery and
  theme build checks await observable states.

Project sequencing uses Playwright's [project dependencies](https://playwright.dev/docs/test-projects#dependencies)
and [per-project worker limit](https://playwright.dev/docs/api/class-testproject#test-project-workers).
Context7 was unavailable in this session; official Playwright docs were checked.
The global limit stays four, with zero retries and no new skipped tests.
Delivery polling retains the existing 90-second operation ceiling and the
unchanged 60-second test budget; no deadline is raised.

## Verification

- Existing wp15 `.env`/network override, ports 215xx. No other project touched.
- `CARGO_BUILD_JOBS=6`; browser runs at most four workers. Docker buildx state uses
  `/tmp/wp15-buildx` because the normal activity directory is read-only here.
- Original bundle, held-hydration regressions: four fail with
  `Expected: disabled; Received: enabled`; existing VerifyLink passes.
  Log: `/tmp/wp15-round3-red.log`.
- Fixed bundle: 23 focused ad/storefront/hydration tests pass (16.4 s), then
  15 account/payment/hydration tests pass (12.1 s).
- An exploratory full run caught a new regression-test locator issue: Kobalte's
  password label association is attached during hydration. The pre-hydration
  check now locates the input by its native autocomplete attribute; the real
  password workflow still uses its accessible label.

- Isolated pending-payment regression: reverting only `saving()` fails the
  disabled assertion; setup passes. Log: `/tmp/wp15-round3-save-red.log`.
  Restored guard: `/tmp/wp15-round3-save-green.log`.
- The stock fixture race above aborted the initial three-run attempt (84 passed,
  one failed, 18 dependent tests did not run). Final verification restarts from
  run 1 after the stock setup fix; it does not count that attempt as a pass.

- A subsequent full run passed all 105 tests (7.3 minutes); its second repeat
  caught the search cache race (83 passed, one failed, 21 did not run). That
  attempt is also excluded from the final consecutive-pass record.

The final data readiness assertions were added during an exploratory theme
phase; that run is excluded from the final record as well.

Final consecutive full runs (`make e2e args='--workers=4'`) use the same wp15
stack/database without reseeding or restarting between runs:

| Run | Result | Duration |
| --- | --- | --- |
| 1 | 105 passed | 7.4 min |
| 2 | 105 passed | 7.3 min |
| 3 | 105 passed | 7.4 min |

The count is 104 browser acceptance tests plus one stock-setup test.
`make lint test` passed: rustfmt, clippy, Biome, workspace typechecks,
682 Rust tests and 359 TypeScript tests (43 files). Nine existing Rust tests remain
ignored by the default target; none newly ignored. Rust test threads and all
Vitest worker-limit environment variables were capped at four, builds at six.
Log: `/tmp/wp15-round3-checks.log`.

Full-run logs: `/tmp/wp15-round3-acceptance-2.log`,
`/tmp/wp15-round3-acceptance-3.log`, `/tmp/wp15-round3-acceptance-4.log`.
These are the three runs with the final source; no source changes between them.
Lint/tests ran between the second and third, sequentially with browser work.

`COMPOSE_PROFILES=full docker compose -p wp15 down -v` completed successfully.
Project-label checks returned zero containers, volumes and networks. Compose and
cleanup logs: `/tmp/wp15-round3-compose.log`, `/tmp/wp15-round3-down.log`.
`git diff --check` passed. Changes remain uncommitted on
`chore/wp15-m1-acceptance`; nothing pushed.

## Remaining risk

These are full local Compose/browser runs, not a new hosted CI run. The existing
60-second search HTML cache policy is unchanged; readiness checks now observe
index completion before the first rendered search request. The 100-unit stock
fixture covers the current suite's combined checkout demand and must grow if that
demand ever exceeds it. No production queue/backoff policy or test timeout was
relaxed, and nothing was pushed.
