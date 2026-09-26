# WP25: M3 acceptance and security follow-through

## Global Constraints

- Work on `chore/wp25-m3-acceptance`; no push, PR, or independent Astra review.
- Preserve the supplied security review verbatim, then append a disposition table.
- Keep local Compose isolated under `wp25`; use four or fewer Playwright workers and six Rust build jobs.
- Security fixes need negative/abuse regression tests. Acceptance must exercise public interfaces with the fake AI provider and real theme gates.

## Review Focus

Capability secrecy, untrusted theme execution/network limits, SNS authenticity, atomic withdrawal quota, and evidence that M3 workflows gate acceptance before publication.

## Tasks

1. **Security record and capability logging.** Save the review; replace raw URI logging with route templates and redact edge errors. Capture logs in tests, including unmatched capability paths.
2. **Theme and webhook boundaries.** Isolate functional checks behind a preview-only proxy; enforce runtime termination/resource controls; verify SNS provenance/freshness/replay or fail closed. Add adversarial tests.
3. **Withdrawal quota.** Lock the order before count/insert/enqueue and prove concurrent requests create at most three links.
4. **M3 acceptance.** Extend real-interface e2e for helper review/quota, builder upload/fork/gates/preview/publish/rollback/hostile archives, and AI agent limits/injection; document criterion-to-test mapping and CI inclusion.
5. **Operations and verification.** Update README, runbook, handoff, follow-ups; run lint/test, three clean full e2e passes, perf, image smoke, tear down only `wp25`, and commit by task.
