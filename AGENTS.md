# Agent notes

Setup, commands and conventions are in `README.md`; this file only adds what an agent must not get
wrong.

- In a linked git worktree, run `pnpm worktree:setup` before anything else (README, "Worktrees").
  It gives the worktree its own database and API port; `make api-native` is its dev server.
- Merge a PR only when the maintainer has delegated merging to you, and only after
  `pnpm verify <PR#>` passed on its final head and its review is clean. Use the
  `gh pr merge ... --match-head-commit <sha>` line verify prints. Never deploy.
- Never start, stop or seed the compose stack from a worktree. The Makefile refuses; do not work
  around it by calling `docker compose` directly.
- `make test-rust` may wait for another checkout's run to finish (one sqlx test run per Postgres
  at a time). Waiting is expected; do not kill it or point it at another database.
