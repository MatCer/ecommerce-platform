# Agent notes

Setup, commands and conventions are in `README.md`; this file only adds what an agent must not get
wrong.

- In a linked git worktree, run `pnpm worktree:setup` before anything else (README, "Worktrees").
  It gives the worktree its own database and API port; `make api-native` is its dev server.
- Merge a PR only when the maintainer has delegated merging to you, and only after CI and its
  review are clean on the final head:
  `gh pr merge <PR#> --squash --delete-branch --match-head-commit <sha>`. Never deploy.
  In a linked worktree, run `pnpm worktree:tidy` after the merge to free its `target/`.
- Never start, stop or seed the compose stack from a worktree. The Makefile refuses; do not work
  around it by calling `docker compose` directly.
- Name a new migration `migrations/$(scripts/next-migration-version)_<name>.sql`, not with
  `sqlx migrate add`: main's versions are future days, so a real timestamp sorts before them. CI
  (`scripts/check-migrations.sh`) refuses a version that collides or sorts before the base's latest.
- `make test-rust` may wait for another checkout's run to finish (one sqlx test run per Postgres
  at a time). Waiting is expected; do not kill it or point it at another database.

## Code Review Rules

Flag only issues this PR introduces that a real caller or input reaches in normal use, at one of:

- P0: data loss, a security hole across a real trust boundary, a broken build or deploy.
- P1: an acceptance-criterion break, a regression against main, or normal use doing a paid or
  user-visible side effect twice (model calls, charges, emails).

Do not flag edge cases normal use does not reach, hardening or defense in depth, validation of data
from our own services, refactors, naming, style, or issues main already has. Do not suggest adding
code unless a P0 or P1 needs it. Most PRs should get no comments.

Operating context: pre-launch, one app instance and one worker. Rollback, deploy windows, replicas
and outage handling are not normal use.
