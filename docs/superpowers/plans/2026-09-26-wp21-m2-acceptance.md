# WP21 M2 marketing acceptance and performance plan

## Global Constraints

- Use only the `wp21` Compose project, ports 22100–22108 and subnet `10.213.21.0/24`; four Playwright workers maximum, six Cargo jobs, serial Lighthouse.
- Preserve production consent, tenant isolation and rate limits. Tests use unique subjects and deterministic job signals; shared flow settings and clock run in the serial project.
- No visual changes, independent Astra review, push or PR. Commit each task with the requested Codex trailer if the worktree permits it.

## Review Focus

- Every §16 M2 journey crosses the real shop, checkout, admin, mail and mock-provider interfaces as applicable; document the exact test for every §11 criterion.
- Check negative consent and suppression paths, repeat runs against retained state, and M2 customer-data access/erasure.
- Re-measure home/category/PDP with recommendations, reviews and watch form against §9.6 and A26.

## Tasks

### 1. Establish isolated baseline and traceability

Files: ignored `.env`, `docker-compose.override.yml`; `docs/acceptance/m2.md`; existing `e2e/**`.

1. Configure and start `wp21`; seed the demo, then inspect the existing browser specs and identify criterion gaps.
2. Map each M2 criterion to a browser assertion, marking missing coverage until it passes.

### 2. Fill M2 acceptance gaps

Files: `e2e/checkout/flows.spec.ts`, `e2e/storefront/newsletter.spec.ts`, `e2e/admin/data.spec.ts`, related product code only for reproduced bugs.

1. Add focused failing browser checks for the missing coupon, price-drop, complaint and M2 privacy paths. Use unique email/order subjects and the dev clock; await a current message/job rather than a sleep.
2. Implement the smallest product fixes the new tests expose, with regression coverage.
3. Keep mutations of the demo flow clock/definitions serial across specs.

### 3. CI and verification

Files: `.github/workflows/ci.yml`, `docs/acceptance/m2.md`, `docs/follow-ups.md`.

1. Confirm CI's full e2e target includes new specs; update job naming/coverage if needed.
2. Run `make lint test`, full e2e three consecutive times at four workers with no skips/retries, and `make perf`; record actual counts and page metrics.
3. If config/images change, run `scripts/smoke-images.sh`. Shut down only `wp21` with volumes.
