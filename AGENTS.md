# Agent notes

Setup, commands and conventions are in `README.md`; this file only adds what an agent must not get
wrong.

- In a linked git worktree, run `pnpm worktree:setup` before anything else (README, "Worktrees").
  It gives the worktree its own database and API port; `make api-native` is its dev server.
- Never run `pnpm verify-merge`: on success it squash-merges the PR, and merging is the
  maintainer's decision. Open the PR and stop.
- Never start, stop or seed the compose stack from a worktree. The Makefile refuses; do not work
  around it by calling `docker compose` directly.
- `make test-rust` may wait for another checkout's run to finish (one sqlx test run per Postgres
  at a time). Waiting is expected; do not kill it or point it at another database.
