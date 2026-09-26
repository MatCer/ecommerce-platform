# Handoff: final state (2026-09-26)

`main` = WP26 merge. All work packages WP0–WP26 are merged (PRs #1–#32). WP26 = fixes from manual
local testing (Firefox checkout handoff, seeded order detail, password-manager autofill; `scripts/autofill-check.mjs`
checks autofill with the real Bitwarden extension). No open PRs, no running
agents, no leftover worktrees. M1, M2 and M3 acceptance suites run locally, not in CI:
`pnpm verify-merge <PR#> --full` (full Playwright e2e at 4 workers + axe + `make perf`) before a
release; ordinary PRs run the specs for their areas (`docs/runbook.md`, "Verify and merge").

| Milestone | Acceptance | Traceability |
|---|---|---|
| M1 local pilot | WP15 | README, `docs/runbook.md`, `docs/research/wp15-ci-round*.md` |
| M2 marketing | WP21 | `docs/acceptance/m2.md` |
| M3 AI | WP25 | `docs/acceptance/m3.md`, `docs/security/2026-09-final-review.md` |

Open gaps with owners: `docs/follow-ups.md`. Pre-launch checklist: `docs/runbook.md`.
Last measured PDP LCP on CI: ~1430 ms against the 1500 ms budget; PDP JS 29.3 kB of 30 kB.
Keep an eye on both when touching the product page.

## Remaining launch blocks

1. Host this edge on a runtime with enforceable per-instance CPU/memory/process and deadline
   termination, or prove managed Workers limits. The local Miniflare process is not launch-ready.
2. Implement SNS certificate-pinned signature verification, TopicArn allowlist, freshness and
   replay deduplication before setting `MAIL_EVENTS_SECRET` in production.
3. Perform a real Anthropic provider smoke run and record pass rate, turns, repairs, tokens and
   cost on a disposable tenant. Local acceptance uses the fake provider by design.
4. For deployments that ran the pre-WP25 API, remove raw capability paths from retained logs and
   revoke exposed order and withdrawal capabilities.
