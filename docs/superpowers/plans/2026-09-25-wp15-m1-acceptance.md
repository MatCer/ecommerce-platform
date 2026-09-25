# WP15 M1 local-pilot acceptance plan

## Global Constraints

- Work only in `chore/wp15-m1-acceptance`; use `wp15` Compose, ports 21500–21508, subnet `10.213.15.0/24`. Keep Playwright at four workers and Lighthouse serial.
- Keep production rate limits and tenant isolation intact. Any e2e identity mechanism must be trusted at the edge, scoped to dev/test, and fail closed in production.
- No visual admin/storefront/theme changes and no WP19 flow edits. Only trivial semantic UI fixes; report larger issues for the UI agent.
- TDD for behavior changes; commit each task with the requested Codex trailer. No push, PR, or independent Astra review.

## Review Focus

- Verify shared-IP e2e isolation without accepting forged client headers from browsers.
- Diagnose checkout handoff latency through the real edge/API/checkout path; preserve one-time token and origin split.
- Enforce §9.6 and A26 performance and accessibility budgets with serial Lighthouse; keep all measurements reproducible.
- Verify a fresh seed, keyboard-only checkout and admin editing, and all WP15 ledger dispositions.

## Tasks

### 1. Establish isolated stack and baseline

Files: `.env` and `docker-compose.override.yml` (ignored), e2e and perf reports.

1. Check branch relation to `origin/main`; record any fetch/rebase sandbox limit.
2. Create WP15-only Compose settings. Run `make up && make seed` and inspect health, demo data and first full e2e/perf failures.
3. Record failure signatures before changing application behavior.

### 2. Stabilize authentication, storefront limits and handoff

Files: `apps/auth/**`, `apps/edge/**`, `crates/api/src/rate_limit.rs`, `e2e/**` as evidence dictates.

1. Add focused failing tests for any shared-IP keying or handoff race found in baseline.
2. Give specs independent staff identities and a dev-only trusted source for per-test rate-limit keys; reject or ignore client-forged values in production.
3. Fix checkout handoff at the observed bottleneck. Keep single-use semantics; no blanket retries, longer timeouts, or skips.
4. Exercise negative production-mode tests and affected e2e specs.

### 3. Complete M1 acceptance coverage and seed

Files: `e2e/**`, `crates/api/src/seed.rs`, `scripts/**` as needed.

1. Move eligible manual smoke paths into Playwright, including a keyboard-only browse → cart → checkout journey and admin product edit.
2. Make `make up && make seed` produce a plausible, navigable CZ/SK demo; keep seed idempotent.
3. Resolve each WP15 follow-up in `docs/follow-ups.md`: implement in scope or assign a justified later/pre-launch owner. Avoid visual and WP19 flow changes.

### 4. Perf/a11y gates and documentation

Files: `packages/theme-kit/src/measure.ts`, gate config, `README.md`, `docs/runbook.md`, relevant tests.

1. Run existing `make perf`; correct measurable gate failures without visual design changes. Keep Lighthouse serial and axe at zero serious/critical.
2. Document quickstart, architecture, operations, test layers, and known limitations concisely.
3. Run `make lint test`, full e2e three consecutive times at four workers, `make perf`, and `scripts/smoke-images.sh` if images/config changed. Record pass counts and failures exactly.
4. Shut down only `wp15` with `docker compose -p wp15 down -v`.
