# Research: A/B testing for storefront themes (SHOP-35)

Status: research spike, 2026-10-10, revised after an independent review (see "Review notes").
Open decision 7 was answered on 2026-10-22 (SHOP-99: self-build). This doc records no other
decision; see "Open decisions".

Evidence tags:
- **[code]** verified in this repo (file:line);
- **[doc]** recorded in a repo decision record, not re-measured;
- **[src]** external source (URL, date);
- **[inf]** inference or estimate.

## Decision summary

1. **Default architecture:** the edge picks the arm and the arm renders server-side (SSR). A
   variant can be delivered three ways:
   - (a) a data override in the same artifact;
   - (b) one artifact containing every arm's components, with SSR rendering only the selected
     branch;
   - (c) one artifact per arm, kept for whole-theme tests.
   Only (c) means "1 variant = 1 build". Option (b) needs one build, gated once per arm path.
2. **The owner's build-cost premise is partly right.** Build count depends on how a variant is
   represented, not on where it renders. Client-side delivery of code arms still needs those arms
   built and shipped.
3. **The owner is right that SEO does not decide this.** Control for crawlers must come from the
   enrolment rule, not from bot detection (Google calls that cloaking).
4. **Client-side is not ruled out in principle.** A first-party, platform-owned script with CSS
   variants applied before paint could fit. But headroom is thin (PDP JS 29.3 of 30 KiB with RUM;
   LCP median 1,353 ms vs a 1,500 ms target and 2,000 ms fail), and nothing has been measured.
   Third-party testing scripts are out (CSP, `thirdPartyOrigins: 0`).
5. **"Zero-build variants ready now" holds only for a few surfaces:** messages, home hero fields
   and CMS blocks.
   - Token variants need a storefront route plus theme integration.
   - Section and PDP settings do not exist. Building them is its own theme-contract project, not
     a schema tweak.
6. **Enrol analytics-consented visitors only.** Record assignment idempotently at the edge,
   outside render and cache. Run an A/A test before trusting any readout.
7. **Low traffic:** 2 arms, big swings, and a calibrated stopping rule. 20 arms is out of reach
   for SMB shops. Bandits are deferred.
8. **Self-build stays the lean option.** GrowthBook (self-hosted, server-side SDK) is a real
   alternative for the stats and admin layer and should be priced before slice 4.

## 1. How a theme revision is built and served today

- **What a revision is:** a source archive, built by `astro build`, then `theme-kit pack` into a
  content-addressed artifact. The artifact holds server modules, client assets, tokens, CSP
  hashes and runtime metadata.
  [code] `packages/theme-kit/src/artifact.ts:100`, [doc] `docs/decisions/runtime-contract.md:35-62`.
- **Pipeline:** `static` (lint, `astro check`), then `build` (`astro build` + pack), `check`
  (Lighthouse, axe, smoke, screenshots), then `functional`.
  [doc] `docs/decisions/theme-builder-sandbox.md:71-78`.
  - **Recorded local observation, not a benchmark:** about 95 s in total. Static took 5.6 s,
    build + pack 2.8 s, check 86 s. The token-only path took about 35 s; it still runs
    `astro build` but skips `astro check`, Lighthouse and smoke.
    [doc] `theme-builder-sandbox.md:84-86`.
  - Read this as "the quality gates dominate", not as a universal ratio.
- **Limits:**
  - Static, build and check get 2 CPUs; 2, 3 and 3 GiB; 240, 300 and 600 s.
    [code] `apps/theme-builder/src/pipeline.ts:220-229, 270-276, 318-325`
  - One build at a time per builder process. [code] `apps/theme-builder/src/server.ts:66`
  - At most 3 outstanding revisions per tenant, running ones included (`409 builds_in_progress`).
    [code] `crates/commerce/src/themes/revisions.rs:44, 380-389`
- **Storage:** local artifacts are about 1.1-1.2 MB each (about 220 kB of client files).
  [code] `du .artifacts/*`
- **Serving:**
  - The edge renders `site.theme_artifact`. [code] `apps/edge/src/gateway.ts:449`
  - The HTML cache key includes the artifact. [code] `gateway.ts:463-470`
  - Previews swap the artifact through the same `shop()` path. [code] `gateway.ts:1720-1765`
  - So per-request artifact selection is mechanically easy. Keeping it safe is the hard part
    (§7).
- **Edge pool:**
  - One workerd instance per artifact and tenant, with about 0.25 s cold start
    ([doc] `runtime-contract.md:83-96`).
  - Idle instances are evicted after 10 min ([code] `apps/edge/src/server.ts:58`).
  - A hard cap of **28 theme instances across all tenants** ([code] `apps/edge/src/runtime.ts:52`).
  - Artifact-per-arm multiplies pool pressure and cold starts.
- **Tokens are build-time on the storefront.** `theme.tokens.json` is compiled into the theme CSS.
  [code] `packages/theme-kit/src/vite.ts:5-24`, `themes/conversion/src/layouts/Base.astro:5`
  - `/_p/tokens.css` exists only on the checkout origin. The shop origin 404s every other
    `/_p/*`. [code] `gateway.ts:1626-1634` (checkout handler), `gateway.ts:1494`
  - The checkout endpoint reads the **active** artifact.
- **Existing data surfaces:**
  - `ShopModel.messages` [code] `crates/commerce/src/storefront/pages.rs:131`;
  - home hero fields [code] `pages.rs:320-324`;
  - CMS/blog `ContentBlock[]` [code] `themes/conversion/src/components/Blocks.astro`.
  - There is no section, layout or PDP settings system.
  - AI editing writes source files and produces gated revisions
    [doc] `docs/decisions/ai-theme-editing.md:41`. So AI variants are code variants today.

**Is "1 variant = 1 full build" true?** Only for artifact-per-arm (c). Twenty such arms would be
twenty gated builds, serialised and capped at 3 outstanding per tenant, plus pool pressure.
- Option (b) puts all arms in one artifact and renders the selected component server-side.
  Unselected server code never reaches the browser. Island assets for the selected arm need
  measuring.
- Incremental compilation and shared chunks are possible in principle; content addressing does
  not forbid them. They would save little: the repeated quality-check setup costs more. [inf]

## 2. What a variant can change, and the cheapest delivery

| Change | Example | Cheapest mechanism | Builds for 20 variants | Ready now? |
|---|---|---|---|---|
| Platform copy | `messages` CTA labels | (a) page-model override | 0 | needs override plumbing only |
| Home hero / CMS blocks | headline, landing-page blocks | (a) page-model override | 0 | same |
| Design tokens | palette, radius | (a) arm-aware storefront tokens route + one-time theme `<link>` | 0 after a theme change | no: theme + edge work |
| Section order / settings | hero layout, trust strip, PDP gallery | (a) once a section-settings system exists | 0 after that project | no: separate theme-contract project (schemas, persistence, API/SDK, renderers, defaults, editor/AI tooling, validation) |
| Business settings | free-shipping threshold, offers | (a) API-side, persisted through cart/checkout | 0 | no: money guardrails |
| Component code | new buy panel | (b) one artifact, SSR-selected branch; or (c) | 1 (b) or 20 (c) | (c) mechanically yes |
| Whole-theme / layout | new PDP template | (c) artifact per arm | 20 | yes, with §7 risks |

Schema-valid data can still break contrast, layout or LCP. Data arms need representative
visual, a11y and perf checks before they start, for example through the token fast path's
checks. [inf]

## 3. Client-side vs server-side vs hybrid

| | Client-side (first-party) | Artifact per arm | Same artifact, SSR-selected (a/b) |
|---|---|---|---|
| Flicker / CLS | Avoidable for CSS variants applied before paint. DOM swaps above the fold risk CLS (budget 0.05) or need hiding. Shopify warns that anti-flicker snippets add >2 s FCP [src: https://shopify.dev/docs/storefronts/themes/best-practices/performance/disable-ab-testing-when-inactive, read 2026-10-10] | none | none |
| Perf budget | Tight, unmeasured. PDP JS 29.3/30 KiB with RUM; LCP median 1,353 ms (target 1,500, fail 2,000) [code] `packages/theme-kit/src/budget.ts:3-12`, [doc] `docs/acceptance/wp25-perf.md:19, 60-67` | each arm gated | base gated once; arms need visual/perf checks |
| Caching | HTML shared; assignment in JS | key already includes artifact | key needs experiment, config version and arm (§7) |
| SEO | acceptable to Google if not crawler-specific | same | same |
| Security | must be platform-owned; themes cannot touch cookies [code] `packages/theme-kit/src/lint.ts:68-70`; CSP `script-src 'self' <hashes>` [doc] `runtime-contract.md:155-163` | unchanged | data is schema-validated |
| Checkout | not covered | not covered | not covered. Checkout is platform-rendered; offer arms need cart persistence |

**SEO.** Google (updated 2025-12-10,
https://developers.google.com/search/docs/crawling-indexing/website-testing) [src]:
- do not show crawlers different content by "server logic … or any other method";
- use `rel="canonical"` and 302 only when a test uses alternate URLs;
- end tests promptly.

Same-URL tests where unconsented visitors (crawlers included) get control fit this guidance.
[inf]

## 4. Assignment and consent (EU)

- **EDPB Guidelines 2/2023 v2.0** (adopted 2024-10-07) [src]
  https://edpb.europa.eu/system/files/2024-10/edpb_guidelines_202302_technical_scope_art_53_eprivacydirective_v2_en_0.pdf
  - Art. 5(3) covers storing or accessing **information**, not only identifiers.
  - Locally produced information that is sent back is in scope (§53), and so is fingerprinting
    (§43).
  - IP-based processing is in scope only under conditions (§§54-56).
  - The guidelines define scope; they create no exemptions.
- **CNIL Sheet 16** (2020-06-11) lists A/B testing among audience-measurement purposes that can
  be consent-exempt. [src] https://www.cnil.fr/en/sheet-ndeg16-use-analytics-your-websites-and-applications
  - Conditions: strictly the publisher's own purpose, no cross-referencing, limited lifetime,
    opt-out. Segregated processors are allowed.
  - It is not a blanket exemption for assignment cookies plus purchase linking at user level.
- **ICO (UK):** current guidance lists "A/B testing" as likely to meet the statistical-purposes
  exception, when the output is aggregate and non-identifying and the sole purpose is improving
  the service. [src, read 2026-10-10]
  https://ico.org.uk/for-organisations/direct-marketing-and-privacy-and-electronic-communications/guidance-on-the-use-of-storage-and-access-technologies/what-are-the-exceptions/
- **Germany (DSK, §25 TDDDG):** assessment is purpose-specific, and "strictly necessary" is read
  narrowly. [src] https://datenschutzkonferenz-online.de/media/oh/OH_Digitale_Dienste.pdf
- **Digital Omnibus** (2025/0360(COD)): still pending in committee, so no application date.
  [src] https://oeil.europarl.europa.eu/oeil/en/procedure-file?reference=2025%2F0360%28COD%29
- **Our facts:**
  - Events and `anon_id` exist only with a stored `analytics` grant, and purchases link only
    then. [code] `crates/commerce/src/analytics.rs:281, 384-395`
  - The subject cookie is minted on the first choice, refusal included, and shared with checkout.
    [code] `crates/api/src/storefront/consent.rs:125`, `apps/edge/src/gateway.ts:170, 814-818`
  - The consent text describes "Anonymous visit statistics".
    [code] `crates/commerce/src/storefront/messages/en.json:138`

| Option | Legal posture | Measurable sample | Bias |
|---|---|---|---|
| A. Consented only; arm = keyed hash(experiment, subject) | lowest, **if the analytics purpose text covers experiments** (update `consent.analytics_hint` and the cookie policy; old consents need a policy decision) | all measurable visitors | consenters only; first pre-consent page is always control |
| B. Assignment for everyone, aggregate-only measurement under CNIL/ICO-style exemptions | varies by member state; per-market legal review | adds only aggregate exposure counts, no conversions | mixed |
| C. Cookieless (IP/UA) | high (EDPB §43, §§54-56) | same | unstable buckets |

**Recommend A.** Things to define before launch:
- consent withdrawal (stop assigning, keep past data per the existing retention rules);
- consent arriving after first paint (assign from the next page);
- cookie expiry and cross-device (assignment is per browser, not per person);
- changing weights mid-test (not allowed; start a new experiment version).

The theme worker never sees cookies [code] `runtime-contract.md:146-150`, so the edge passes the
arm in the request context.

## 5. Measurement

- **Assignment:** record it once per `(experiment, version, anon_id)`, idempotently, at the edge
  or through an API call outside render.
  - Never as a side effect of a page-model fetch. That fetch misses HTML cache hits and fires on
    SWR revalidations and speculation. [code] `gateway.ts:486-522`
  - Keep **assignment** (eligible and bucketed) apart from **served exposure** (the first HTML
    that carried the arm) and, where it matters, **visible exposure** (a beacon after render, for
    below-the-fold changes).
- **Analysis unit:** a unique consented subject. The primary metric is purchasers per assigned
  subject (intent-to-treat).
  - A bounded attribution window, e.g. 14 days after assignment.
  - Cohorts are read only once mature.
  - Currencies stay separate.
  - Cancellations and refunds are excluded via `orders` at readout time.
- **Outcome data:** the `purchase` event carries `anon_id`, session and order total.
  [code] `analytics.rs:384-410`
- **SRM:** χ² test on **unique assignments** against the configured split.
  [src] https://www.microsoft.com/en-us/research/articles/diagnosing-sample-ratio-mismatch-in-a-b-testing/
  - Edge request counters count requests, repeats and failures, so they are not usable for SRM.
    [code] `apps/edge/src/counters.ts:61`
- **Schema:**
  - `experiments`: id, tenant, version, kind, arms with frozen config and weights, metric,
    window, status;
  - `experiment_assignments`;
  - an arm in the request context and the page model.
  - `events` is unchanged.

## 6. Statistics for low-traffic shops

Assumptions: equal two-arm split, α = 0.05 two-sided, 80% power, normal approximation.
Unique-visitor conversion, not sessions. "Purchases" means baseline-equivalent (control)
purchases. [src] Evan Miller formula; reproduced by the reviewer.

| baseline CR | +10%: visitors/arm (control/treatment purchases) | +20% |
|---|---|---|
| 1% | 163k (1,631 / 1,794) | 42.7k (427 / 512) |
| 2% | 80.7k (1,614 / 1,775) | 21.1k (422 / 507) |
| 3% | 53.2k (1,596 / 1,756) | 13.9k (417 / 501) |

- **20 arms at 2% CR, +20%, Bonferroni over 19 comparisons:**
  - about 39.9k visitors per arm, about 797k in total;
  - about 15.9k baseline-equivalent purchases (about 19k if every treatment lifts +20%).
- No real tenant traffic distribution was checked. "SMB-infeasible" is an inference; check it
  against production order counts before setting eligibility thresholds.
- **Bayesian readout:** posterior plus expected loss is a good presentation. Optional stopping
  still inflates errors.
  [src] https://www.r-bloggers.com/2015/08/is-bayesian-ab-testing-immune-to-peeking-not-exactly/,
  https://arxiv.org/abs/1807.09077
  - Fix the priors, the minimum duration (≥ 2 full weeks, for weekly seasonality) and the
    loss threshold.
  - Then simulate the rule, including A/A and null runs, to calibrate the false-positive rate
    before shipping.
- **Bandits:** usable without cross-shop priors, but adaptive allocation complicates inference.
  [src] https://arxiv.org/abs/2111.00137 They are deferred until the A/B readout is calibrated.
- **Validity risks:**
  - novelty effects;
  - seasonality and promotions;
  - inventory changes;
  - concurrent experiments (allow one per tenant at first);
  - RUM sampling (10%) making perf guardrails noisy.
- **Guardrails:**
  - SRM on assignments;
  - error rate;
  - RUM LCP;
  - revenue per assigned visitor for offer arms;
  - a maximum duration.

## 7. Operational risks the design must cover

- **Artifact lifecycle:** GC releases `ready` revisions after 7 days
  ([code] `crates/commerce/src/themes/revisions.rs:1064`). Active arms must be pinned.
  - Retained lookup covers hashed `/_astro/*` only. Unversioned public files come from the
    active artifact. [code] `gateway.ts:1523`
- **Pool cap:** 28 theme instances shared by all tenants ([code] `runtime.ts:52`). Each artifact
  arm takes slots, so cap artifact arms per tenant and platform-wide.
- **Cache:**
  - The key covers experiment id, config version and arm.
  - Config is frozen while running.
  - Stop and promote purge the tenant.
  - SWR refreshes must render the same arm.
  - Arm-specific CSS needs versioned URLs.
  - Any outer CDN must not cache arm HTML without the same key.
  - The hit-rate impact is not simply ÷ arms; measure it.
- **Checkout:** platform-rendered, and its tokens come from the active artifact.
  - Offer or shipping arms must be persisted on the cart, and checkout must recompute them
    authoritatively.
  - Otherwise checkout shows something different from what the visitor saw.
- **Promotion:** goes through the existing publish path (fresh auth, audit). Stopping a test
  falls back to control.

## 8. Build vs buy

| | Fit | EU / data | Verdict |
|---|---|---|---|
| GrowthBook | MIT core; self-host [src] https://docs.growthbook.io/self-host; server SDK evaluation [src] https://docs.growthbook.io/lib/node; warehouse-native over our Postgres; Bayesian, SRM, CUPED [src] https://docs.growthbook.io/statistics/overview | self-hosted in EU; no browser origin needed | **real option** for the stats and admin layer; edge assignment and data arms are ours either way |
| PostHog | flags + its own event pipeline | EU cloud Frankfurt [src] https://posthog.com/blog/posthog-cloud-eu | duplicates our analytics |
| Statsig | acquired by OpenAI (announced 2025-09-02) [src] https://www.verdict.co.uk/openai-acquire-statsig/ | EU only via warehouse-native (2024 FAQ) | no |
| Optimizely | quote-only [src] https://support.optimizely.com/hc/en-us/articles/4410289753485-Pricing | enterprise | no |
| Cloudflare / Vercel | Workers cookie-split recipe [src] https://developers.cloudflare.com/workers/examples/ab-testing/; Flags SDK precompute (Next/SvelteKit) [src] https://vercel.com/changelog/flags-sdk-3-2 | n/a | confirms the edge pattern |

**What has to be built either way:**
- assignment, exposure, cache rules and lifecycle;
- the data-override plumbing.

**What can be bought:** the readout, calibration, guardrails and experiment admin. GrowthBook
self-hosted covers this at the cost of operating one more service.

The cost is not small either way. Decide after slices 1-2. [inf]

## 9. Readout and stopping rule (SHOP-99)

The readout compares control with one variant on a daily UTC snapshot. It reports `collecting`,
`winner` or `no_difference`, and the guardrails can stop the experiment. A seeded simulation of
daily peeking picked the rule's three constants. [code]
`crates/commerce/src/experiments/stats.rs`, `crates/commerce/src/experiments/stats/simulation.rs`

**The rule.** Each arm's posterior is Beta(1 + converters, 1 + subjects - converters). The rule
judges every daily snapshot afresh and remembers nothing from earlier days. The first gate that
holds decides. [code] `stats.rs:169`

1. If the experiment stopped for SRM, the readout is `collecting (evidence)`. A broken split
   never names a winner.
2. If fewer than 14 days of assignments have matured, it is `collecting (time)`.
3. If either arm has fewer than 100 converters, it is `collecting (converters)`.
4. If choosing either arm loses less than 2% of the control's conversion rate in expectation, it
   is `no_difference (equivalent)`.
5. If the arm more likely to be best has P(best) of at least 0.996, that arm is the `winner`. A
   tie goes to control.
6. If the experiment stopped and every attribution window has closed, it is
   `no_difference (ended)`.
7. Otherwise it is `collecting (evidence)`.

The constants are `floor` 0.996, `epsilon` 0.02 and `min_converters` 100. [code] `stats.rs:16`
The floor decides winners, epsilon decides equivalence, and the converter minimum gates both.
Epsilon is relative to the control's posterior mean, so one value fits a 0.5% shop and a 5% shop.

The guardrails run only while the experiment runs, in this order. [code] `stats.rs:217`

1. SRM fires when Pearson's χ² of all unique assignments against the configured weights exceeds
   10.8276, the p < 0.001 cutoff for 1 df. It needs at least 100 assignments.
2. Harm fires when P(control best) exceeds 0.99 after 14 mature days. It has no converter
   minimum.
3. The duration limit fires after 56 days of real time.

**Numerics.** For integer shapes, P(X > Y) is a hypergeometric tail. With Nx = a + b - 1,
Ny = c + d - 1 and K = a + c - 1, it equals P(H <= a - 1) for H ~ Hypergeometric(Nx + Ny, Nx, K),
by an order-statistics argument. The code gives the mode weight 1, walks out both ways with the
ratio recurrences and sums the two tails apart with Neumaier's compensated sum, so a tiny
probability is summed directly, never computed as 1 minus a number near 1. A direction stops
early only when a geometric bound certifies that the remaining mass is below 1e-15 of the mass so
far. The walk uses no factorials and no log-gamma, and it visits at most the two arms' converters
plus 2 terms. Expected loss uses the shifted identity L_X = mean_Y P(Y+ > X) - mean_X P(Y > X+),
where X+ is Beta(a + 1, b). The leading arm's loss comes from its small tails, and the trailing
arm's loss adds the difference of the means. The unit tests check exact rationals such as
P(X > Y) = 8/11 and 165/442 and losses of 41/1001 and 809/6188, exact big-integer sums at 100 to
60,000 subjects, a 40-digit sum at 100,000 converters on 5,000,000 subjects that agrees within
1e-15, and the truncated walk against the full support. [code] `stats.rs:299`, `stats.rs:313`

**Model.** One run is one experiment, and day 0 is its start. Each day assigns N new subjects,
split Binomial(N, 1/2) between the arms, and each arm's converters in that cohort are
Binomial(n, p). A cohort counts only once all of it is 7 days old, so purchase timing inside the
window does not matter. The peek at day D sees:

- assignments through day min(D, stop);
- mature cohorts through day min(D - 7, stop), which is also `mature_days`;
- `Phase::Running { days: D }` while the experiment runs;
- the final readout at D = stop + 7;
- an untrusted split after an SRM stop.

Each peek reads the readout first and runs the guardrails second, as the hourly job does once a
day's snapshot exists. A stop ends assignment, and the peeks go on until stop + 7 days. The 56-day
limit stops every run. [code] `simulation.rs:206`

A false winner is an A/A run that shows `winner` for either arm at any peek, the 7 peeks after the
stop included. Power is the share of lifted runs that show `winner (variant)` at any peek.

**PRNG and seeds.** The generator is SplitMix64, and a test checks that seed 0 gives
`0xe220a8397b1dcdaf` first. Uniforms are (z >> 11) * 2^-53. The daily split counts random bits,
and conversions use inversion from k = 0 with the pmf recurrence. The sampler computes (1 - p)^n
by squaring, because Rust leaves the precision of `powi` and `powf` to the platform. The
simulation therefore uses only integer and basic IEEE 754 arithmetic, so the tallies should not
depend on the machine [inf]. The debug test reproduces the release grid's tallies exactly. Each
scenario seeds one stream for all its runs with `daily << 32 | baseline_bp << 16 | lift_percent`,
for example `0x258_00c8_0014` for +20% at 2% and 600/day. [code] `simulation.rs:8`,
`simulation.rs:102`

**Grid and selection.** The grid crosses `floor` in {0.99, 0.993, 0.995, 0.996, 0.997, 0.998,
0.999}, `epsilon` in {0.005, 0.01, 0.02, 0.05} and `min_converters` in {0, 25, 50, 100}. All 112
rules read the same runs. The A/A cells run 2% at 100, 600 and 1,500 assigned subjects a day, plus
600/day at 0.5% and at 5%, with 4,000 runs each. The lifted scenarios run +20% at 2% for 300, 600
and 1,200/day and +10% at 1,500/day, with 2,000 runs each. The constants are picked in order, and
each criterion must hold at every value of the constants not yet picked, so a later pick cannot
break an earlier criterion. [code] `simulation.rs:480`

1. `floor` is the smallest value whose A/A false-winner rate has a Wilson 95% upper bound of at
   most 5% in every cell.
2. `min_converters` is the largest value that costs at most 1 percentage point of +20% power at
   600/day.
3. `epsilon` is the largest value whose +10% runs at 1,500/day end `no_difference (equivalent)`
   in at most 5% of runs.

| `floor` | worst A/A upper bound over every epsilon, minimum and cell |
|---|---|
| 0.99 | 10.34% |
| 0.993 | 7.62% |
| 0.995 | 5.77% |
| **0.996** | **4.68%** |
| 0.997 | 3.74% |
| 0.998 | 2.59% |
| 0.999 | 1.50% |

| `min_converters` at floor 0.996 | worst cost in +20% power at 600/day |
|---|---|
| 0, 25 or 50 | 0 points |
| **100** | **0.6 points**, 53.4% to 52.8% |

| `epsilon` at floor 0.996 and 100 converters | +10% runs at 1,500/day that end `equivalent` | that show it at any peek |
|---|---|---|
| 0.005 or 0.01 | 0% | 0% |
| **0.02** | **0.55%** | 1.50% |
| 0.05 | 13.95% | 45.25% |

Counting the runs that show `equivalent` at any peek, not only at the end, also picks 0.02.
Epsilon changes no A/A or power cell in the grid. A peek that clears the 0.996 floor has its
trailing arm about 2.65 standard errors behind, and that arm then loses far more than 2% of the
rate at these counts. [inf]

**Results for the chosen rule.** The committed test pins the A/A rows and the 600/day row with
`assert_eq!`, and the grid prints the rest. [code] `simulation.rs:326`

| A/A cell, 4,000 runs | false winners | Wilson 95% upper bound | SRM stops | harm stops |
|---|---|---|---|---|
| 2%, 100/day | 0 | 0.10% | 45, 1.125% | 180, 4.5% |
| 2%, 600/day | 131, 3.275% | 3.87% | 63, 1.575% | 180, 4.5% |
| 2%, 1,500/day | 148, 3.7% | 4.33% | 62, 1.55% | 187, 4.675% |
| 0.5%, 600/day | 0 | 0.10% | 41, 1.025% | 162, 4.05% |
| 5%, 600/day | 153, 3.825% | 4.47% | 67, 1.675% | 149, 3.725% |

| +20% at 2%, 2,000 runs | power | median first-win day | first win after the stop |
|---|---|---|---|
| 300/day | 500, 25% | 48 | 98, 4.9% of runs and 19.6% of winners |
| 600/day | 1,056, 52.8% | 37 | 123, 6.15% of runs and 11.6% of winners |
| 1,200/day | 1,723, 86.15% | 30 | 91, 4.55% of runs and 5.3% of winners |

- No lifted run named control the winner. A +10% lift at 1,500/day wins in 802 runs, 40.1%, and
  ends `equivalent` in 11, 0.55%.
- At 2% and 100/day an arm expects 56 converters after 56 mature days, and at 0.5% and 600/day it
  expects 84. The rule almost never decides in those cells, and their experiments end
  `collecting (converters)`. [inf]
- At 300/day the converter minimum costs 4.2 points of power, 29.2% to 25.0%. The criterion
  weighs only 600/day.
- A/A runs stop for harm in 3.7% to 4.7% of runs. Each look is a 1% test, and the daily looks
  from day 21 add up. The spec fixes the 0.99 threshold.
- About 12% of the winners at 600/day first show after the 56-day stop, while the last cohorts
  mature. Promotion therefore has to work on a stopped experiment.
- The committed test reruns the grid's runs for the chosen rule. It pins the calibration and is
  not an independent sample. Its worst A/A upper bound is 4.47%, below the 5% target.
- An independent sample was measured once on 2026-10-22 and not committed: the same test with
  every scenario seed XORed with `0x5348_4f50_3939_0002`. It gave A/A false winners of 0, 2.73%,
  3.95%, 0 and 4.10% in the cells above, in order. The worst Wilson upper bound was 4.76%, at 5%
  and 600/day. Power for +20% at 600/day was 56.25%.
- The root's spec lane measured a second independent sample on 2026-10-22, with every seed XORed
  with `0x5350_4543_5f52_3101`. In the 5% and 600/day cell, 193 of 4,000 runs named a false
  winner, 4.83%, with a Wilson upper bound of 5.53%. Extended to 40,000 runs, 1,617 did, 4.04%,
  with a Wilson 95% interval of 3.85% to 4.24%. A 4,000-run sample can therefore put its upper
  bound above 5% by chance while the true rate stays near 4%.

**SMB traffic assumption [inf].** The SMB level is 600 assigned subjects a day. Assignment needs
analytics consent, and a subject is one browser, so that is 600 × 30 = 18,000 consented unique
visitors a month. At a 60% analytics opt-in that is 18,000 / 0.6 = 30,000 monthly visitors. At 2%
it is 18,000 × 0.02 = 360 attributed purchases a month. No tenant traffic has been measured yet,
because the platform has not launched.

**Daily UTC snapshot.** The readout and the guardrails read the same snapshot. [code]
`stats.rs:125`

- `as_of` is the last UTC midnight. Once the experiment stops, it is at most stop + 7 days.
- A subject is mature once assigned at least 7 days before `as_of`. Subjects assigned after the
  stop never count.
- `mature_days` runs from the start to min(`as_of` - 7 days, stop).
- `Phase::Running` carries `days`, real time since the start, which drives the 56-day limit.
- `final` holds from the first midnight at or after stop + 7 days.
- `decides_from` is the first midnight at or after start + 21 days, which is 14 mature days plus
  the 7-day window. It is null when the experiment stopped less than 14 days after its start,
  because `mature_days` can then never reach 14. A later stop still decides once the subjects
  assigned before it mature.

The cohort cutoff is daily, but the readout reads order status live. A cancellation or a full
refund can still change a count within the day and after `final`. "Final" means every attribution
window has closed, not that the numbers are frozen.

**Reproduce.**

- The committed test, `the_stopping_rule_keeps_a_a_false_winners_under_five_percent`, takes 4.0 s
  in a debug build on the development machine, and the command also runs the unit tests:
  `cargo test -p commerce --lib --locked experiments::stats -- --nocapture`
- The grid takes under 1 s in a release build once built. It prints all 112 rules, the evidence
  for each criterion and the chosen rule, and it fails unless `RULE` is that rule:
  `cargo test -p commerce --lib --locked --release experiments::stats::simulation::calibration_grid -- --ignored --nocapture`

## Open decisions for the product owner

1. **Consent:** enrol only consented visitors, and extend the analytics consent text to cover
   experiments? What happens to existing consents?
2. **First surface:** copy and hero (cheapest), or tokens (needs the storefront route and a theme
   change)?
3. **Section settings:** fund them as a separate theme-contract project? This is the real unlock
   for AI data variants.
4. **Code arms:** SSR-selected components in one artifact (b), or artifact per arm (c) with a cap
   (≤ 2 arms per tenant, pinned against GC)?
5. **Arms and eligibility:** 2 arms by default, plus a minimum-traffic gate based on real
   order counts?
6. **Offer and shipping arms:** in scope? They need cart persistence and pricing-law review.
7. **Stats and admin:** self-build the readout, or self-host GrowthBook?
   **Answered 2026-10-22 (SHOP-99): self-build.** The readout, calibration, guardrails and admin
   live in the existing API, worker and admin app.
   - Self-hosted GrowthBook is one more container (Next.js front end, Express API, Python stats
     engine) plus a MongoDB or MongoDB-compatible database [src]
     https://docs.growthbook.io/self-host (read 2026-10-22). The platform runs one app instance
     and one worker.
   - Separate organizations in one instance need multi-tenant mode, which is part of GrowthBook
     Enterprise [src] https://docs.growthbook.io/using/security (read 2026-10-22). Without it,
     merchants could not use GrowthBook's UI, so the merchant admin is ours either way and
     GrowthBook would only be a stats engine behind it.
   - Its warehouse queries would need a cross-tenant read role over `experiment_assignments`,
     `events` and `orders`, outside the forced-RLS model that every public table follows.
   - Edge assignment, exposure, cache rules and arm plumbing are already ours (SHOP-96,
     SHOP-98, SHOP-100). The missing part was a two-arm Beta-Binomial readout, which is small.
   - Revisit if bandits, CUPED or many concurrent experiments per tenant become requirements.

## Proposed implementation slices

1. **Assignment and exposure contract plus cache rules.**
   - `experiments` (versioned, frozen config) and idempotent `experiment_assignments`.
   - Consent-gated keyed hash at the edge; arm in the request context.
   - Cache key with experiment, version and arm.
   - Tests: stickiness, no consent gives control, cache isolation, SWR arm consistency,
     withdrawal.
2. **A/A validation.** Two identical arms on a real shop. Check SRM on assignments and that the
   readout shows no false signal.
3. **One narrow data experiment.** A copy or hero override through the page model, with a visual
   and perf check of the arm.
4. **Calibrated readout plus minimal admin.**
   - Priors, stopping rule and simulation.
   - Attribution window, refunds and currency rules.
   - Start, stop and manual promote via publish.
   - Guardrail auto-stop.
   - This is where to decide build vs GrowthBook.
5. **Artifact arms with lifecycle protection.**
   - GC pinning.
   - Pool caps.
   - Public-asset handling.
   - Option (b) SSR-selected components in the theme contract.

Separate project: section and PDP settings system. Deferred: token arms, offer arms, bandits and
cross-shop priors.

## Review notes

Independent review (gpt-6.1-sol, "sound with fixes") was applied after verifying each cited line
or source. Partial disagreements:
- **ICO:** accepted, but it is UK guidance and does not govern EU merchants; it informs the
  option B discussion only.
- **"Client-side not inherently flicker"**: accepted for CSS variants applied before paint.
  Above-the-fold DOM swaps still trade CLS against hiding, so the server default stands.
