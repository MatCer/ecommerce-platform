# WP5 Admin shell + catalog UI: implementation plan

> **For agentic workers:** execute task by task with TDD where logic is testable; commit after
> every task.

**Goal:** a SolidJS admin SPA at `admin.localhost` (static build served by Caddy) with staff
login (password, magic link, TOTP challenge + enrollment), in-memory staff JWT with silent
refresh and re-authentication for sensitive operations, tenant switcher, role-aware navigation,
and the catalog screens on top of the WP3 Admin API; plus the missing staff-management Admin
API and a trigram index for the product search.

**Spec:** D10, §3.2, §5.3, §8.3, §14 (WCAG 2.2 AA, CSRF/CSP), §17 WP5, A9.

## Global Constraints

- Stack: SolidJS + Vite + Solid Router + TanStack Solid Query + Kobalte + Tailwind 4,
  `@solid-primitives/i18n` (cs/sk/en). Typed API through `@platform/admin-client`. No `any`.
- Design tokens live in `packages/config/tailwind/theme.css`; reusable accessible components in
  `packages/ui` (Kobalte-based) so the checkout app can share them later.
- Staff JWT only in memory (A9). The Better Auth session cookie is HttpOnly and never read by JS.
- Staff management: owner/admin only, `auth_time` within 15 min (`401 reauth_required`), audit
  log in the same transaction, a tenant always keeps at least one owner, only owners grant,
  change or remove the owner role.
- Every screen: loading, empty, error, permission-denied, disabled and success states;
  keyboard operable; visible focus; usable from 768 px up.

## Design decisions

- **Auth on the admin origin.** `*.localhost` hosts are distinct *sites* (each `x.localhost`
  is its own registrable domain), so a `SameSite=Lax` Better Auth cookie on `auth.localhost`
  is never sent on `fetch` from `admin.localhost`. Caddy therefore routes
  `admin.localhost/api/auth/*` to the auth service and `BETTER_AUTH_URL` is the admin origin:
  the session cookie is first-party, no `SameSite=None` needed, Better Auth's origin check
  still applies. The JWT issuer stays `http://auth.localhost` (A9) and `auth.localhost` keeps
  serving JWKS. Recorded as a deviation from §5.3 ("cookie on auth.localhost").
- **Token lifecycle.** `GET /api/auth/token` after sign-in; refreshed 60 s before `exp` and
  once on `401 invalid_token`. `401 reauth_required` opens a re-login dialog (password +
  TOTP); after success the original request is retried once.
- **Invitations.** API: find-or-create the Better Auth user through the auth service's
  internal endpoint, insert the membership + audit entry, send the magic-link invite, then
  commit (a failed send rolls the membership back; the user creation is idempotent). The auth
  client is shared with the superadmin CLI.
- **Owner guard.** Role changes/removals lock the tenant's owner rows (`FOR UPDATE`) before
  counting, so two concurrent demotions cannot leave zero owners.
- **Rich text.** Minimal `contenteditable` editor (bold, italic, headings, lists, link) with
  DOMPurify on load and paste; the server (`ammonia`) remains the trust boundary. Rejected
  Tiptap/ProseMirror (~150 kB) as disproportionate for product descriptions.
- **Media.** `POST /assets/uploads` → XHR `PUT` (progress events) → `complete` → poll the
  asset until `ready`/`failed`; ordering with move up/down buttons (keyboard accessible).
- **Search index.** `pg_trgm` GIN indexes on `product_translations.name` and `variants.sku`
  for the `ILIKE '%q%'` product search.

## Review Focus

- A9: JWT never persisted; reauth flow; staff endpoints require fresh auth.
- Owner invariant under concurrency; cross-tenant denial on every staff endpoint.
- Admin CSP and the `/api/auth/*` routing on the admin origin.
- Accessibility of the custom pieces (rich text toolbar, category reorder, variants matrix).

## Tasks

1. **Staff management API** (`crates/commerce/src/staff.rs`, `crates/api/src/admin_staff.rs`,
   `crates/api/src/auth_service.rs`): list/invite/update role/remove, owner guard, audit,
   integration tests (roles, fresh auth, last owner, cross-tenant, auth-service mock).
2. **Trigram index** migration + `EXPLAIN` sanity test; `make sqlx-prepare`; `make openapi`.
3. **Design tokens + `packages/ui`** (Kobalte): Button, fields, Select, Checkbox, Dialog,
   Toast, Tabs, Badge, states (Spinner/Empty/Error/Forbidden).
4. **Admin shell** (`apps/admin`): Vite config, i18n, auth client + token store, API client
   middleware, router, layout with tenant switcher + role-aware nav, login/magic link/TOTP,
   security settings (TOTP enrollment, set password).
5. **Catalog screens:** products list, product editor (all sections), categories tree,
   parameters, markets, staff, audit log.
6. **Docker + Caddy:** admin image (static build), `/api/auth/*` route, CSP headers.
7. **Playwright e2e** (`e2e/admin/`) + axe; screenshots in `docs/screenshots/wp5/`.
8. Verification, Astra review, PR.
