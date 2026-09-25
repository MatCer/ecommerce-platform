# WP9 Customers, consent, mail core: implementation plan

> For agentic workers: execute task by task with TDD (failing test, minimal code, green,
> refactor). Commit after every task.

**Goal:** customer accounts on the checkout origin (A1, A4, A5), consent records and
resolution (A20), order/payment/fulfillment/return state machines (A13, A16, A10), and the
mail core with send states, retries and suppression (A14, A29), plus the staff invite email
moved to the outbox → mail pipeline.

**Architecture:** business logic in `commerce::{customers, consent, notifications,
orders::status, privacy}`; SMTP + MJML rendering in `platform::mail`; HTTP in
`api::storefront::{customer, consent}`; cookies and CSRF in the edge (`apps/edge`); UI in
`apps/checkout` (Astro SSR pages + Solid islands built from `@platform/ui`).

## Global constraints

- Every tenant table: `tenant_id`, RLS + `FORCE`, composite FKs, a cross-tenant test.
- Tokens (sessions, magic links) are 256-bit (`commerce::capability`), SHA-256 at rest.
- Only the edge sets cookies. The API returns credentials in response headers
  (`x-session-token`, `x-session-clear`, `x-consent-subject`) that the edge turns into cookies
  and never forwards to browsers.
- `sid` lives only on the checkout origin as `__Host-sid` (host-only, HttpOnly, Secure,
  SameSite=Lax, 30 days). State-changing `/_p/*` calls need a same-origin `Origin` (or
  `Sec-Fetch-Site: same-origin`) and a JSON body.
- Email sends only via `email_messages` + a `mail.send` job enqueued in the business
  transaction. Suppression is checked right before every send.
- No `unwrap()` outside tests, `thiserror` in libraries, sqlx macros with `.sqlx/` data.

## Review focus

- A5: set/change password needs the current password or a magic-link sign-in within the last
  10 minutes; a change revokes every other session; magic links are consumed atomically.
- Anti-enumeration: magic-link requests always answer 202; password login for an unknown
  email still runs an argon2 verification.
- A14 transitions: `sending` found by a later attempt → `uncertain`; transactional mail gets
  exactly one retry from `uncertain`, marketing none.
- A20: the server resolves consent from `consent_records`; client-sent state is never trusted
  for execution.

## Tasks

1. **Migration** `20260929000000_customers_consent_mail.sql`: `customer_groups`, `customers`
   (unique `(tenant_id, email)`, email stored lower-cased), `customer_addresses`,
   `customer_sessions`, `customer_magic_links`, `customer_auth_attempts`, `consent_records`
   (append-only grants), `email_messages`, `email_suppressions`, `platform.ip_salts`;
   `carts.customer_id` FK + status `merged`; purge function for expired auth rows and salts.
2. **State machines** `commerce::orders::status`: pure `transition(state, command) ->
   Result<(state, Vec<Event>), TransitionError>` for order, payment (incl. COD
   delivered→collected→remitted and `LatePayment`), fulfillment, return line; exhaustive tests
   over every (state, command) pair.
3. **`platform::mail`**: `MailConfig` (per-stream SMTP URL + from), `Mailer::send ->
   Delivery::{Accepted, Rejected, NotSent, Uncertain}` classification, `render_mjml`.
   Tests against an in-process fake SMTP server.
4. **`commerce::notifications`**: templates (layout + magic link, password changed, staff
   invite, order skeleton) as MJML + text, cs/sk/en catalogs, tenant branding; `enqueue` and
   `deliver` (A14 state machine, suppression, retry policy); suppression add/check.
5. **`commerce::privacy::ip_hash`** (daily rotating salt) and **`commerce::consent`**
   (`record`, `current`, `state`, `link_anonymous`).
6. **`commerce::customers`**: passwords (argon2id), sessions, magic links (rate limits),
   login, logout, addresses CRUD, set/change password, cart merge, `customer.email_verified`
   outbox event (hook for WP10 guest-order linking), safe redirects.
7. **Storefront API**: `/storefront/v1/customer/*` and `/storefront/v1/consent`; `ConsentConfig`
   gains `text_version`.
8. **Worker**: `mail.send`, `staff.invite_mail` (subscriber of `staff.invited`), cleanup of
   expired customer auth rows; worker gets mail + auth-service config.
9. **Staff invite**: membership + `staff.invited` event commit together; the auth service's
   `/internal/users/invite` can return the link instead of mailing it (`deliver: false`).
10. **Edge**: checkout-origin `/_p/account/*` proxy, `/_p/consent` on both origins, session and
    consent subject in the CHECKOUT binding context, client IP forwarding.
11. **Checkout UI**: sign in (magic link / password), verify, overview with orders placeholder,
    addresses, security, consent preferences, sign out; cs/sk/en; `@platform/ui`.
12. **OpenAPI + clients**, compose/env (SMTP streams for api + worker), `docs/decisions/consent-contract.md`,
    follow-ups.
13. **E2E** `e2e/checkout/account.spec.ts` + stack verification, Astra review, PR.
