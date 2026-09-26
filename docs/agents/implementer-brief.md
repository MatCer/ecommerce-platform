# Brief for implementing agents (read fully before starting a work package)

You implement one work package (WP) of the platform described in
`docs/superpowers/specs/2026-09-24-platform-design.md` (the spec, §17 lists the WPs).
Also read `docs/scope.md`. The binding amendments in spec §20 override earlier spec sections. The spec is authoritative; if a spec detail is wrong or
infeasible, choose the best-practice alternative, record it in the PR under
"Deviations from spec", and continue. Never stop to ask questions: decide, document, proceed.

## Workflow

1. You are in a git worktree on your own branch. First `git fetch origin && git rebase origin/main`
   so you start from the latest main.
2. Read the spec sections relevant to your WP and the existing code you build on
   (use `agentgrep`/`rg` + narrow reads; do not dump whole directories).
3. Write the WP implementation plan to `docs/superpowers/plans/<date>-wp<N>-<slug>.md`
   using the superpowers:writing-plans format (header, Global Constraints, Review Focus,
   tasks with files/interfaces/steps). Keep it concrete but proportionate: code blocks for
   key interfaces and tests, not every line. Commit it.
4. Execute the plan task by task with TDD (superpowers:test-driven-development): failing test,
   minimal code, green, refactor. Commit after every task (small, conventional commits:
   `feat(catalog): ...`, `fix(api): ...`, `test(...)`, `chore(...)`, `docs(...)`).
   End every commit message with the trailer line:
   `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`
5. Verify through the real interface: bring the relevant part of the Docker stack up
   (`make up` / `make dev-infra` once they exist), exercise the flow (curl, Playwright, psql),
   and run the full check suite (`make lint test` + e2e/perf where the WP touches them).
   If you touched config, Dockerfiles or binaries, also run `scripts/smoke-images.sh` against freshly
   built images (`pnpm verify-merge --full` runs it with no dependencies available: binaries must boot and report degraded
   readiness rather than crash when optional config/deps are missing).
   Bring your compose stack down when finished (`docker compose -p <project> down`).
6. Independent review before the PR: run an Astra review yourself via Codex, read-only:
   `codex exec -m gpt-6-astra -s read-only --skip-git-repo-check -C <worktree> -o /tmp/<wp>-review.md "<prompt>"`
   (prompt: review `git diff origin/main...HEAD` against the spec incl. §20 amendments; ranked findings
   with file:line and fixes; verdict). Before launching, check no other `codex exec` started in the
   last 60 s (`ps -eo etimes,args | grep -E 'codex(\.js)? exec' | grep -v grep`); if one did, wait
   (concurrent Codex startups corrupt the login). Run it in the background and poll; it takes 5-30 min.
   Fix every blocker/high/medium finding (with tests), reasonable lows too; list what you fixed and
   anything you consciously declined (with reason) in the PR body under "Review".
7. Push the branch and open a PR against `main` with `gh pr create`. PR body sections:
   Summary, What was verified (commands + outcomes, briefly), Deviations from spec,
   Follow-ups / known gaps. End the body with:
   `🤖 Generated with [Claude Code](https://claude.com/claude-code)`
8. Final message to the orchestrator: PR URL, 10-20 line summary, verification evidence,
   deviations, known gaps. Do not paste large logs.

## Engineering rules (mandatory)

- Follow `/home/matcer/.agents/ENGINEERING-STANDARDS.md` (security baseline, TDD, KISS/YAGNI,
  no custom crypto/auth protocols, validate at trust boundaries, fail closed).
- Rust: stable toolchain, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, no `unwrap()`
  outside tests, `thiserror` for library errors, `anyhow` only in binaries. Business logic only in
  `crates/commerce`. SQL via `sqlx` (prefer `query!`/`query_as!` with offline data committed in
  `.sqlx/` so builds work without a DB).
- TypeScript: strict mode, never `any` (use `unknown` + narrowing), Biome clean, pnpm only.
- Every tenant-owned table: `tenant_id`, RLS policy, `FORCE ROW LEVEL SECURITY`, and an
  integration test proving cross-tenant access fails.
- Use Context7 (`resolve-library-id` + `query-docs`) for current library/framework docs before
  using an API you are not certain about. Pin versions in manifests; commit lockfiles.
- Frontend UI work: use the `frontend-design` skill for visual direction and keep to the shared
  tokens in `packages/config`; accessibility per spec §14.
- LLM/Anthropic API code: consult the `claude-api` skill first.
- Do not add dependencies that duplicate existing ones. Justify each new non-trivial dependency
  in the PR.

## Machine limits (shared 24-core desktop, other agents may be running)

- `CARGO_BUILD_JOBS=6` (export it), never raise vitest/turbo worker env vars, Playwright
  `workers: 4` max, Lighthouse serial. Do not start a second heavy build/test run while one runs.
- Docker: use a compose project name unique to your WP (`COMPOSE_PROJECT_NAME=wp<N>`) and the
  port offsets given in your task prompt, so parallel WPs do not collide. Never touch other
  projects' containers.
- Do not run long-lived dev servers outside Docker except briefly for a verification step;
  stop them afterwards.
