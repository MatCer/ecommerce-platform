# Handoff: WP25 working state (2026-09-26)

WP25 runs in worktree `chore/wp25-m3-acceptance`, based on `db1fae8` (WP15 M1 acceptance).
The Git administrative path is read-only here: fetch, rebase, staging and commits fail. No push
or PR was requested. This handoff describes worktree state; it does not claim a merge.

## Implemented in this worktree

- The supplied security review is preserved in `docs/security/2026-09-final-review.md`, followed
  by a finding-by-finding disposition. API logs use route templates; edge error logs do not echo
  capabilities or exception text. Operators must purge/revoke historical exposures in existing
  deployments.
- Merchant functional checks run on `theme-functional`, with only a preview-only proxy attached.
  Build and browser gates retain their existing network. A browser acceptance check attempts to
  spoof `mail.localhost` from inside a sandbox and requires HTTP 403.
- Withdrawal link issuance locks its order row for count, insert and enqueue. A concurrent
  database regression requests 12 links and requires exactly three emails.
- The local Miniflare runtime evicts a timed-out tenant instance, caps admitted instances and has
  a Compose-level resource backstop. It still lacks enforceable per-instance OS limits;
  `APP_ENV=prod` refuses this entrypoint. Deploy managed Workers or isolated gVisor/Firecracker
  instances before production traffic. Production also refuses SES ingestion until SNS signature,
  topic, freshness and replay verification is implemented; subscription URLs are no longer logged.
- `docs/acceptance/m3.md` maps M3 criteria to real browser flows and integration limits; the
  existing CI `acceptance` job runs the full browser and perf suites.

## Verification and environment

Local Compose project: `wp25`; ports 22500–22508; default subnet `10.213.25.0/24`.
`/tmp/wp25-docker-config` is used because the host's Docker buildx activity directory is
read-only. Only `wp25` was used; `wp21` and other projects were not touched.

Verification: `make lint test` passed (686 Rust tests, 360 TypeScript tests; nine existing Rust
ignores). Three consecutive full Playwright runs at four workers passed 106/106 each in 9.1,
9.1 and 9.2 minutes, with no skips or retries. `make perf` passed four pages over local HTTPS;
`scripts/smoke-images.sh wp25-rust:local wp25-mocks:local wp25-theme-builder:local
wp25-auth:local` passed. The docs/runbook pre-launch checklist retains real-provider,
production sandbox, legal/accounting and cloud verification as launch gates.

## Remaining launch blocks

1. Host this edge on a runtime with enforceable per-instance CPU/memory/process and deadline
   termination, or prove managed Workers limits. The local Miniflare process is not launch-ready.
2. Implement SNS certificate-pinned signature verification, TopicArn allowlist, freshness and
   replay deduplication before setting `MAIL_EVENTS_SECRET` in production.
3. Perform a real Anthropic provider smoke run and record pass rate, turns, repairs, tokens and
   cost on a disposable tenant. Local acceptance uses the fake provider by design.
4. For deployments that ran the pre-WP25 API, remove raw capability paths from retained logs and
   revoke exposed order and withdrawal capabilities.
