---
**Verdict: launch-blocking.** Reviewed commit `db1fae8`, read-only. Five findings below; no confirmed critical finding. Deployment-dependent findings are explicitly marked.

The threat model used these assets and boundaries:

| Boundary | Protected assets / principal threats |
|---|---|
| Storefront → edge → checkout origin | Customer sessions, carts, order capabilities; hostile themes, CSRF, XSS, cache contamination |
| Admin → auth service → API → PostgreSQL | Staff identity, tenant data, integration secrets; account takeover, role bypass, cross-tenant access |
| API → queue → worker | Payment state, inventory, emails; forged jobs, replay, duplicate execution |
| Theme agent → builder → sandbox → preview runtime | Host access, credentials, other tenants; prompt injection, arbitrary code execution, resource exhaustion |
| AI helpers → proposed changes → confirmation | Catalog integrity and AI budget; malicious source content, unsafe model output |
| Webhooks, payment providers, SES/SNS, ad forwarders | Authentic events and authorized outbound traffic; forgery, replay, SSRF, amount manipulation |
| Object storage, imports, exports, GDPR | Private documents and PII; unauthorized downloads, archive traversal, decompression abuse, incomplete deletion |

1. **HIGH — Order and withdrawal capabilities leak into request logs.**
   Location: crates/api/src/lib.rs:307.

   Tracing records the raw URI path. Routes include `/storefront/v1/orders/{token}` and `/storefront/v1/withdrawals/{token}`; excluding query strings does not protect these credentials. Someone with log access can replay order tokens to read customer email, addresses and order details, or use an unconsumed withdrawal token to submit a declaration. Order tokens remain valid for 90 days.

   **Fix:** log `MatchedPath` route templates, with a safe fallback for unmatched routes. Redact capability paths in edge errors too. Remove exposed tokens from retained logs and revoke affected capabilities. Add log-capture regression tests.

2. **HIGH — Untrusted functional checks can read authentication emails through the sandbox network.**
   Locations: apps/theme-builder/src/pipeline.ts:411, docker/caddy/Caddyfile:27.

   **Applies to the supplied Compose stack.** Merchant/model-authored `checks/*.spec.ts` execute arbitrary Node code on `theme-check`. That network reaches shared Caddy, whose `mail.localhost` route exposes Mailpit. A malicious check can request Caddy with that Host header, read other tenants' magic-link emails, and return credentials through a deliberately failing check's captured output. No container escape is needed.

   **Fix:** provide a dedicated preview-only proxy with an exact host/path allowlist. Exclude Mailpit, auth, admin, storage administration and mocks. Network isolation must enforce this independently of model instructions or browser CSP. Verify with an adversarial functional check.

3. **HIGH — Theme render deadlines do not terminate hostile computation.**
   Locations: apps/edge/src/gateway.ts:369, apps/edge/src/runtime.ts:88.

   **Applies to the implemented Miniflare edge and previews.** The ten-second timeout races a promise and aborts body reading; it neither cancels worker execution nor destroys the instance. Runtime workers lack per-instance OS resource limits. A theme can pass sampled gates, then enter a CPU loop or allocate memory on another URL, continuing after the gateway returns 504 and degrading the shared host.

   **Fix:** run untrusted runtime instances under enforceable CPU/memory/process limits and terminate them on deadline, with bounded admission. Alternatively, deploy the intended managed Workers runtime and verify its limits. Build-container limits do not cover this edge execution. Upstream also explicitly requires additional sandboxing for hostile code.

4. **MEDIUM — SES/SNS authenticity and freshness verification is unfinished.**
   Location: crates/api/src/webhooks.rs:180.

   After Basic authentication, the endpoint accepts the envelope without verifying its SNS signature, authorized topic or timestamp. Possession of the shared endpoint secret permits fabricated bounce/complaint events for known message IDs; old authenticated notifications can also be replayed after suppression removal. This is **not an unauthenticated bypass**, but it leaves the documented production verification requirement unimplemented.

   **Fix:** verify SNS signatures using constrained certificate fetching, enforce configured topic ARNs and freshness, and deduplicate message IDs. Refuse production SES ingestion until these checks exist. Stop logging full subscription-confirmation URLs.

5. **MEDIUM — Withdrawal email quota is bypassable by concurrent requests.**
   Location: crates/commerce/src/withdrawals.rs:201.

   The three-links-per-hour check performs an unlocked count followed by insertion. Concurrent requests for a known order number/email can all observe remaining allowance and enqueue separate emails. The general storefront limiter does not enforce this recipient quota atomically.

   **Fix:** serialize issuance with a tenant/order advisory lock or locked quota row before counting, inserting and enqueueing. Add a concurrency test proving that only three messages are created.

Controls verified in source:

- **Tenant isolation:** all 115 statically created tenant tables have ENABLE/FORCE RLS declarations; generated event partitions receive both. `tenant_tx` uses transaction-local `set_config(..., true)`. Runtime roles are non-owner/non-BYPASSRLS. Inspected pool-level exceptions use platform tables or explicit privileged functions.
- **Authentication/authorization:** EdDSA JWT issuer/audience/expiry checks, membership rechecks, sensitive-operation freshness checks, disabled staff self-signup, and restrictions on one-factor login for 2FA-enabled staff.
- **Browser boundaries:** checkout-only `__Host-sid`, same-origin mutation checks, injected identity headers, restricted theme operations, denied outbound theme fetches, edge-owned CSP and cache exclusions.
- **Tokens/payments:** hashed random capabilities, atomic single-use consumption, cart-scoped placement idempotency, Stripe signature verification and event deduplication, account/livemode/object/amount/currency matching.
- **Untrusted content:** HTML sanitization, escaped email templates, bounded archive parsing with traversal/link rejection, constrained AI file tools, validated helper plans and explicit acceptance/publishing.
- **SSRF/storage/builds:** DNS-filtered safe HTTP client, redirect checks, private authorized downloads, export credential exclusions, and build containers with non-root execution, dropped capabilities, resource limits and capped tmpfs output.
- **Production switches:** fake payments/simulator rejection, disabled fake AI, dev-only test-clock/E2E identities and outbound-host overrides.

Validation limits: this was static review, not deployed-system penetration testing. Existing integration tests were inspected, not executed. Production Cloudflare routing, TLS/HSTS, storage IAM and live provider configuration remain unverified. `cargo-audit` is unavailable; `pnpm audit --offline --json` failed with `fetch failed`, so dependency advisory status is **unknown**.

Fix finding 1 before any pilot; resolve findings 2–3 wherever untrusted themes/previews run, and finding 4 before enabling SES. The supplied repository does not support a launch clearance yet.

---

## WP25 disposition

| Finding | Status | Evidence / owner |
|---|---|---|
| 1. Capability logging | Fixed in code; historical-log cleanup deferred | API traces use `MatchedPath` with `<unmatched>` fallback; edge errors redact capability paths and exception text. Log-capture regressions cover both. Operations owner must purge any older retained raw-path entries and revoke exposed capabilities at each deployment; this worktree has no access to external log stores. |
| 2. Functional-check network | Fixed for supplied Compose stack | Merchant checks use `theme-functional`, whose only peer is a preview-only Caddy proxy. A real uploaded check spoofs `mail.localhost` and requires HTTP 403. Other deployments must carry the same isolation. |
| 3. Render resource limits | Deferred: platform infrastructure owner; launch-blocking | Deadline now evicts the offending instance, admission is bounded and the local edge container has a host-level CPU/memory/pid backstop. These are defense in depth, not enforceable per-instance limits. The Miniflare entrypoint refuses `APP_ENV=prod`; deploy managed Workers with verified limits or per-instance gVisor/Firecracker processes before launch. |
| 4. SNS verification | Deferred: mail integration owner; production fail-closed | Production refuses `MAIL_EVENTS_SECRET`, so unsigned ingestion cannot run there. Dev fixtures keep the local endpoint. Implement constrained certificate fetch, signature, topic, freshness and replay checks before enabling SES. Subscription URLs are no longer logged. |
| 5. Withdrawal quota | Fixed | Order-row lock serializes count, token insertion and email enqueue; 12 concurrent requests must create exactly three links. |
