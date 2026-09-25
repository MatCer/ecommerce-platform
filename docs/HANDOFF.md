# Handoff: orchestration state (2026-09-25)

Read this first when continuing in a new session.

## Status

`main` = `7554a51`. Every PR from #1 to #24 is merged. No open PRs, no running agents, no leftover worktrees.

| Milestone | Done (merged) | Remaining |
|---|---|---|
| M1 local pilot | WP0–WP12, WP13a, WP14 | **WP13b**, **WP15** |
| M2 marketing | WP17 recommendations, WP18 email marketing, WP20 ad forwarders | **WP16** reviews, **WP19** flows, **WP21** acceptance |
| M3 AI | WP22 AI helpers, WP23 theme builder | **WP24** AI theme editing, **WP25** acceptance |

## Remaining work packages (spec §17 + §20 amendments)

| WP | Scope | Prereqs (all merged) |
|---|---|---|
| WP13b | CSV import of customers + historical orders (archived, no side effects, A28), tenant data export (JSONL + assets manifest zip, private bucket), GDPR access/erasure (anonymize orders, keep invoices per tax law, A29), newsletter subscriber import with consent evidence | WP12, WP18 |
| WP16 | Reviews: storage, review tokens, moderation, verified flag, `AggregateRating` JSON-LD, Omnibus review-verification disclosure (legal template exists from WP13a), theme integration | WP12 |
| WP19 | Flow engine + abandoned cart (email_marketing consent at each step, restore link, optional single-use coupon), watchdog (back-in-stock / price-drop, double opt-in, `inventory.changed`/`price.changed`), review invites (`review_invites` consent, after delivery), dev test clock | WP16, WP18 |
| WP24 | AI theme editing: agent loop **outside** the sandbox (Claude tools: list/read/write/delete within `src/**`, `public/**`, `theme.tokens.json`; run_checks), writes a per-change functional check (hook exists in WP23), max 25 turns / 3 repair cycles, prompt UX + diff + check report in admin. Use `docs/decisions/ai-edit-prompts.md` + `theme-builder-sandbox.md`. Fake provider for tests (no API key locally) | WP22, WP23 |
| WP15 | M1 acceptance: full e2e reliable with 4 workers (per-spec users, auth/storefront rate limits vs shared local IP, checkout-handoff timeouts), perf/a11y gates, manual keyboard checks, seed polish, README/runbook, all WP15 items in `docs/follow-ups.md` | all M1 |
| WP21 | M2 acceptance suite + perf re-check | WP16, WP19 |
| WP25 | M3 acceptance + final security review (Astra) + docs | WP24 |

Suggested order: **WP16 ∥ WP13b ∥ WP24** → **WP19** → **WP15** → **WP21** → **WP25**.

## How the orchestration works (keep doing this)

- Every WP is implemented by an **Opus 5.5** agent (`Agent`, `model: opus`, `isolation: worktree`, background). The user asked for Opus for all implementation, backend included.
- The agent reads `docs/agents/implementer-brief.md` (mandatory). It covers the plan doc, TDD, commits with the trailer, running its own **Astra review via `codex exec -m gpt-6-astra`**, local `scripts/smoke-images.sh`, and opening the PR.
- Each prompt gives a unique `COMPOSE_PROJECT_NAME=wp<N>`, a port set and a subnet `10.213.<N>.0/24` (git-ignored `docker-compose.override.yml`; the machine has no free Docker address pools). Avoid Chromium-blocked ports (e.g. 10080). Every prompt also carries a migration timestamp floor (the current latest migration is `20261015000000`).
- Orchestrator loop:
  1. Link the PR (`mcp__t3-code__link_pull_request`).
  2. `gh pr checks <n> --watch`, then `gh pr merge --squash --delete-branch`.
  3. `git worktree remove` + delete the local `worktree-agent-*` branch.
  4. Tell still-running agents that main moved (they merge main and regenerate `.sqlx`/OpenAPI/clients).
- Max 4 agents at once. Disk fills fast (Docker build cache + Rust `target/`): `docker builder prune -f --keep-storage 10GB` and remove finished `wp*` images/volumes.
- API session limits can stop agents mid-task. Resume them with `SendMessage` (context is kept).

## Key documents

- Spec: `docs/superpowers/specs/2026-09-24-platform-design.md` (§20 amendments A1–A30 override earlier text)
- Scope: `docs/scope.md`
- Decisions: `docs/decisions/*.md`
- Per-WP plans: `docs/superpowers/plans/`
- Open gaps with owners: `docs/follow-ups.md`
- Operations + real-provider pre-launch checklist: `docs/runbook.md`

## Known risks

- Full e2e is flaky with 4 workers: rate limits, because all local browsers share one IP, and checkout handoff timeouts. Specs pass alone. Owner: WP15.
- Real providers are untested; everything ran against mocks: Stripe, Packeta/PPL, SES/SNS signatures, bank-app scans of the QR codes. See the runbook checklist.
- Legal templates and invoice/VAT setup need lawyer/accountant review before real use.
- Theme sandboxes use the plain Docker runtime; prod needs gVisor/Firecracker on separate build hosts.
