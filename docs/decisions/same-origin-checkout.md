# Decision record: checkout on the storefront origin, accounts on their own (SHOP-73)

Question: can the platform pages now served from `checkout.<shop>` (checkout, order pages,
account, withdrawal, review, newsletter/watch/flow links, consent, restore-cart) move to paths on
`<shop>` (SHOP-54) while theme JS, which is merchant- or AI-authored and untrusted, keeps running
unrestricted on that same origin?

**Status: draft, not approved.** The second security review (2026-10-15) rated it unsound; the
third rated the fixes for its two blockers sound with required changes, which are folded in
(A2, C4, C6). Four further issues remain open (§ Open issues, O3-O6). SHOP-54 does not start
until they are closed.

**Proposed decision: split them, like Shopify.**

- **Checkout, order pages and one-off email links move to `<shop>`** with the controls below.
  Every control is mandatory for SHOP-54; the move does not ship with any of them missing.
- **Customer accounts move to their own origin** (SHOP-95): `<shop>.account.<platform>` by
  default, optionally `account.<merchant domain>`. Sign-in, sign-up, password reset and every
  `/account` page live only there. The shop origin holds no customer session and never renders a
  password field.

Shopify does the same (checked live on allbirds.com, 2026-10): checkout is
`www.allbirds.com/checkouts/cn/<token>` on the shop origin, accounts are on
`accounts.allbirds.com`. Unlike Shopify's new customer accounts, ours keep passwords; the
separate origin is what protects them.

An independent review (Astra, 2026-10-10) rejected the first draft, which kept accounts on
`<shop>`: a shared anonymous page token let a theme fix a customer into the attacker's session,
Safari cookie fixation was accepted, and autofilled passwords were readable by theme JS. Its ten
findings and their resolution are listed at the end.

## Threat model

Adversary: theme code running in the customer's browser on `<shop>`, from a third-party theme
or a prompt-injected AI edit. It can run any script `script-src 'self'` allows, which is
everything in the theme artifact. The merchant is not the adversary (the admin already shows
them their orders). The theme server worker is not in scope here: it keeps the §4 trust
boundary of `runtime-contract.md` (no cookies, no egress, restricted binding).

Assets: account credentials and sessions, the checkout cart capability, customer PII (name,
email, phone, addresses, order contents), bearer links from email (order page, withdrawal,
review, newsletter preferences, restore-cart), payment-return secrets, and the integrity of
legally required flows (placing an order, withdrawal, consent).

| Threat (same-origin theme JS) | Control |
|---|---|
| Stealing or fixing an account session, reading autofilled passwords | Accounts on their own origin (A1) |
| `fetch`, `<link rel=prefetch>`, XHR reading a PII page | Platform pages served only to top-level navigations (C1) |
| `<iframe src=/checkout>` and reading its DOM | C1, `frame-ancestors 'none'`; theme CSP already has `frame-src 'none'` |
| HTTP cache or bfcache replay of a PII page | `Cache-Control: no-store` (C1), bfcache reload (C7) |
| `w = window.open('/checkout')`, then `w.document` | COOP mismatch (C2) |
| Service worker on `/` intercepting platform navigations | No workers on the origin, ever (C3) |
| Calling `/_p/checkout/*`, `/_p/orders/*` with the customer's cookies | Page token bound to a random capability (C4) |
| Reading bearer URLs or payment-return secrets from history or referrer | Exchanged for a cookie + 303 to a clean URL (C5) |
| Planting or overwriting the checkout capability | `__Host-Http-` cookie, rotation on every PII write (C6), HTML-only tokens (C4); residual R1 |
| Attaching the attacker's account to the customer's checkout | `state`-bound identify and placement re-confirmation (A2) |
| Forged consent | Theme consent limited to anonymous cookie categories (C9) |
| Storage, `BroadcastChannel` | No platform data in origin storage (C7) |
| Shadowing a platform path or asset, now or from cache | Platform namespace reserved before launch (C8) |

## Accounts

**A1. Account origin.** Account pages, sign-in, sign-up, password reset and email verification
are served only from the account origin, which runs no theme code; theme tokens style it as
schema-validated values. Old `/account*` paths on `<shop>` and `checkout.<shop>` answer `308` to
the account origin through an explicit route map. Cookies there are `__Host-` (not settable with
`Domain`, so a theme on `<merchant domain>` cannot toss them onto `account.<merchant domain>`).
The account origin accepts state-changing requests only when `Sec-Fetch-Site: same-origin`:
`account.<merchant domain>` is same-site with the shop, so `SameSite` alone would let theme JS
send credentialed POSTs.

Per-shop origins keep password managers from mixing up saved logins between shops. Two browser
behaviours to know, neither of which hands theme JS a password without the customer picking it:
Chrome offers logins saved for one subdomain on sibling subdomains of the same registrable
domain after the user selects them. On the default origin that only affects other shops' account
pages (all platform code); with `account.<merchant domain>` a theme form on the shop could have
the customer pick their saved login. The merchant opts into that by choosing the custom
subdomain; the admin says so.

**A2. Sign-in from checkout.** The shop origin never receives an account session. A checkout
learns who the customer is only from the account origin's own session cookie, which a theme
cannot plant: it is `__Host-` on another host, and Safari enforces `__Host-`.

1. "Sign in" in checkout is a navigation POST to `/checkout/identify/start` (page token). The
   server creates an identify transaction: a random `state`, bound to the checkout's current
   capability, single use, valid 5 minutes, and sets it as a fresh `__Host-Http-idt` cookie as
   well. It answers `303` to the account origin with `state`.
2. The account origin signs the customer in if needed, then redirects to the one fixed
   callback `<shop>/checkout/identify?state=…&code=…`. The code is single use, valid 60 s and
   bound to the `state`, the shop and the account.
3. The shop edge strips both under C5. The server accepts the callback only if the `state`
   exists, is unused, matches the `__Host-Http-idt` cookie, the code was issued for it, and the
   request's capability is the one the `state` was bound to; it consumes `state` and code in
   one transaction. An unsolicited callback is rejected (RFC 9700 §4.5: authorization code
   injection). On success the checkout is linked to the account and the capability rotates
   (C6).

Steps 1-3 do not bind to the browser where the prefix is not enforced: a theme can plant both
the capability and the transaction cookie the attacker started with. So **the identify link is
only a prefill hint.** It lets checkout show that account's saved addresses; it never makes
the checkout or anything entered in it visible in that account, and never updates the account.
Only a confirmed placement (4) does.

4. **Placement confirmation.** Placing a checkout linked to an account is a separate
   transaction with its own purpose: the place-order POST creates a placement `state` bound to
   the checkout id, its current capability generation and the expected account, and redirects
   to the account origin. The account origin reports, from its own unplantable session, which
   account is signed in in this browser, if any. The callback consumes that `state` and places
   the order in the same transaction:
   - same account: the order is attributed to it;
   - no account or a different one: the order is placed as a guest order, and the checkout
     shows that before the redirect back completes the placement;
   - missing, expired, mismatched or reused `state`, or any capability rotation since the
     place-order POST (another tab changed the checkout): placement aborts and the customer is
     back on the review step. Abort never falls back to a guest order.

The callback target is the exact canonical shop origin and path, never a prefix match.

## Controls on `<shop>`

**C1. Navigation-only platform pages.** For every platform page path (§ Routing) the edge
answers only when `Sec-Fetch-Mode: navigate` and `Sec-Fetch-Dest: document` and no
`Sec-Purpose` header (prefetch/prerender would consume one-shot tokens). Anything else gets `403`
with no body. A request without `Sec-Fetch-Mode` (older than Safari 16.4 / Chrome 76 / Firefox
90) gets a static "browser not supported" page: fail closed, because a script fetch from such a
browser looks the same. Scripts cannot set `Sec-Fetch-*` and `fetch()` cannot use mode
`navigate`. Responses carry `Cache-Control: no-store`, `frame-ancestors 'none'` +
`X-Frame-Options: DENY`, `Referrer-Policy: strict-origin`, and the checkout CSP (Stripe,
Packeta) with `worker-src 'none'`. Not `no-referrer`: it turns `Origin` on same-origin form and
`fetch` POSTs into `null`, which `gateway.ts` `sameOrigin` rejects.

**C2. Opener isolation by COOP mismatch.** Same-origin documents with equal COOP share a
browsing context group. Theme responses switch to `same-origin-allow-popups`; platform pages
keep `same-origin`. The values differ, so opening, or navigating a held popup to, a platform
page puts it in a new browsing context group and the opener's handle reports `closed` (HTML
COOP matching; all current engines), and the same in reverse. Switch to
`noopener-allow-popups` once Safari and Firefox ship it.

**C3. No service workers on the origin.** The edge refuses every request carrying
`Service-Worker: script` with `403`, and theme and platform CSPs add `worker-src 'none'`. A 403
stops new registrations but does not reliably remove an active one, so this must hold for
every shop origin from its first public response: SHOP-54 ships the C3 edge rule and CSP to
theme responses before any shop goes live, and its verification checks that no production
response has ever answered `Service-Worker: script` with a 2xx (the theme runtime serves no
worker scripts today). A shop origin that ever served one cannot host platform pages until a
recovery path exists; there is none in scope.

**C4. Page token on platform API calls.** `/_p/checkout/*` (except start, C10),
`/_p/orders/*`, `/_p/withdraw*`, `/_p/reviews`, `/_p/newsletter/*`, `/_p/watch/*`, `/_p/flows/*`
and `/_p/fake-pay/*`, GET included, require `x-page-token` (JSON) or a `_pt` form field. The
token is `base64url(HMAC-SHA256(EDGE_PAGE_TOKEN_SECRET, "pt1|" + host + "|" + cap))`, where
`cap` is the checkout capability (`__Host-Http-cart`) or, on bearer-link pages, the C5
capability, compared in constant time. Rules:

- A request without that capability cookie gets `403`. There is no token for empty
  credentials, so no token is shared between visitors.
- Tokens exist only in platform page HTML (`<meta name="page-token">`), which C1 keeps from
  theme JS. No API response ever carries a token, so theme JS that makes a call with a planted
  capability and its known token learns nothing that outlives the next rotation. A request
  that fails validation never receives a token.
- Calls that rotate the capability (C6) are navigation form POSTs answered with `303` to the
  next checkout page; the new token arrives in that page's HTML. A tab still holding an old
  token gets `403` and reloads itself by navigation; it never replays the mutation.
- Every protected read and mutation validates the capability **inside the same database
  transaction** that reads or writes the checkout, against the checkout's current capability
  generation. A request authorised with a capability that a concurrent write has since
  revoked fails instead of returning or changing data from after the rotation. This covers
  non-PII mutations (shipping method, cart lines, discount codes) and their responses too.
- JSON endpoints keep the same-origin `Origin`/`Sec-Fetch-Site` and JSON-only checks. The
  navigation POSTs C4, C6, C10 and A2 rely on are a separate, enumerated set of
  form-encoded endpoints that require `_pt` (except C10), same-origin `Origin`,
  `Sec-Fetch-Site: same-origin` and `Sec-Fetch-Mode: navigate`.

`/_p/consent` (C9) and the shop's `/_p/cart*` stay token-free: themes are meant to call them.

**C5. No secrets in history or referrers.** `navigation.entries()` exposes earlier same-origin
URLs in the tab unless the navigation that loaded them used `no-referrer`/`origin`, which is
decided by the page linking in (an email client, Stripe), not by our response. So the edge never
renders a URL that holds a secret. A navigation carrying one stores it in an `HttpOnly` cookie
and answers `303` to a clean URL; rendering reads it back from the cookie. Redirects create no
history entries. The inventory, which the edge enforces by route and parameter name:

| Source | Parameters |
|---|---|
| Order and email links | `/o/<token>`, `?token=`, `?t=` |
| Checkout identify (A2) | `?code=` |
| Stripe return (`return_url`) | `payment_intent`, `payment_intent_client_secret`, `setup_intent`, `setup_intent_client_secret`, `redirect_status`, `source`, `client_secret` |

Packeta's widget returns by `postMessage`, not by URL. The C5 cookies (`__Host-Http-cap`) are
`Path=/`, live 30 minutes, are deleted when the page they authorise completes its action
(withdrawal sent, review submitted) and hold one capability: a second link opened in the same
browser replaces the first. Stripe values are consumed by the checkout-return handler and not
kept. A clean URL opened in another browser shows "open the link from your email again".

**C6. The checkout capability.** The checkout cart cookie becomes `__Host-Http-cart`
(`HttpOnly; Secure; Path=/; SameSite=Lax`, 256-bit random). Chrome 140+ and Firefox 143+ refuse
`document.cookie`/Cookie Store writes of `__Http-`/`__Host-Http-` names; Safari does not (R1).
Where the prefix is not enforced, theme JS can plant a capability the attacker holds, so C6
does not rely on the prefix. Instead the capability rotates on **every** write that adds or
changes PII (contact, address, note), on checkout start (C10) and on identify (A2). The write,
the new random capability and the revocation of the old one happen in one transaction, and the
new value is only ever sent as an `HttpOnly` `Set-Cookie`, which theme JS cannot read in any
engine. PII therefore only ever sits under a capability the attacker has never seen: a planted
capability is dead from the victim's first write, and a write the theme makes itself yields a
cookie it cannot read and no token (C4). Planting another capability later moves the victim to
a different checkout; their data stays where it was, out of the attacker's reach. Server-side,
each capability has an expiry (24 h idle), is scoped to one shop and one checkout, and is
checked against the checkout it is presented for.

The final step shows "Confirmation goes to <email>" next to the place-order button, so a
pre-filled email the customer did not type is in view at the moment of decision (R1).

The shop `cart` cookie (`Path=/_p`) and consent cookies are unchanged.

**C7. No platform state in origin storage or bfcache.** Platform pages put nothing into
`localStorage`, `sessionStorage`, IndexedDB, Cache API or `BroadcastChannel`. The placement
replay in `apps/checkout/src/lib/placement.ts` goes away: after a reload or lost response the
checkout page asks `GET /_p/checkout/order` (page token) whether the checkout already became an
order, instead of replaying a stored body. Platform pages reload on `pageshow` with
`event.persisted`, because `no-store` does not keep every browser out of bfcache.

**C8. Platform namespace reserved before launch.** The edge routes platform paths, `/_p/*` and
the checkout artifact's assets before the theme worker and theme public files. Checkout assets
move to their own prefix, `/_checkout/<build id>/`, and the edge refuses theme files under
`/_checkout/`, `/checkout`, `/o/` and the other platform paths now, before any shop is live, so
no browser can hold a theme-served response at a URL the checkout will later import. The build
id makes every chunk and stylesheet URL new per release. A merchant redirect or CMS page cannot
claim a platform path.

**C9. Consent.** The theme-callable `/_p/consent` records only anonymous cookie categories
(analytics, marketing cookies) for the current browser. It never sets `email_marketing`,
`review_invites` or other customer preferences, ignores any `source` in the body, and is not
linked to a customer. Customer preferences change only on platform pages with a page token:
checkout (marketing opt-in at placement) and the newsletter preferences page.
`consent-contract.md` is updated to match.

**C10. Checkout start.** The theme's checkout button is a form POST to `/checkout/start`: a
top-level navigation (C1), exempt from the page token because a theme page cannot have one. It
converts the shop cart, mints a new checkout capability, revokes the browser's previous one, and
answers `303 /checkout` with no token in a readable place. A hostile theme can trigger it, which
does what the customer's own click does: restart checkout from the shop cart, which the theme
already controls.

**C11. Unsubscribe.** RFC 8058 one-click unsubscribe is `POST /_p/unsubscribe/<token>`: bearer
token only, ignores cookies, no `Origin` check (mailbox providers send none), never issues a
page token.

Theme-side changes that come with it: theme CSP `form-action 'self'`, `worker-src 'none'`, COOP
`same-origin-allow-popups`; the speculation rules exclude the platform paths; `checkout_url`
in the page model points at `<shop>/checkout/start`.

## Routing (input to SHOP-54 and SHOP-95)

On `<shop>`: `/checkout`, `/checkout/start`, `/checkout/identify/start`, `/checkout/identify`,
`/o/<token>` → `/o/<id>`,
`/withdraw`, `/review`, `/restore-cart`, `/consent`, `/newsletter`, `/newsletter/*`,
`/watch/*`, `/flows/*`, plus `/_p/fake-pay/*` locally. The `/_p/*` operations keep their paths,
except `/_p/account/*`, which moves to the account origin.

`checkout.<shop>` answers only legacy routes, through an explicit map: each known path answers
`308` to `https://<shop>` + the mapped path (query kept), `/account*` to the account origin,
anything else `404`. The target is built from the canonical shop origin and a path from the
map, never from the raw request path, so `//attacker.example` cannot become an external
redirect. These redirects are `Cache-Control: no-store`, since some carry tokens until C5
strips them on arrival.

The handoff (`/start?h=`, `__Secure-hf-*`, `/storefront/v1/checkout/handoff`) goes away.

## Residual risks

- **R1. Pre-filled checkout.** On browsers that do not enforce `__Host-Http-` (Safari, Chrome
  < 140, Firefox < 143) theme JS can plant a checkout the attacker filled in. C6 keeps
  everything the customer enters out of the attacker's reach, and A2 keeps the attacker's
  account off the order. What remains: fields the customer leaves untouched keep the
  attacker's values. If the customer places the order with the attacker's email still shown
  next to the button, the confirmation and order link go to the attacker. A theme can also
  move the customer to another checkout, which loses their input (nuisance, no data). This
  needs the customer to confirm a foreign email on screen, which is no stronger than R2.
  Accepted. Revisit when WebKit ships the prefix (WebKit bug 296267).
- **R2. Look-alike UI.** A theme can `pushState` to `/checkout` and draw a fake form that sends
  typed data away; CSP does not cover navigation. A theme could already draw any form on the
  shop. Browser address and card autofill is not origin-bound, so a theme form gets the same
  autofill on any site; saved passwords are origin-bound and are on the account origin. Stripe
  card fields stay in Stripe's cross-origin iframe.
- **R3. Same-process side channels.** Separate browsing context groups on one site may share a
  renderer process. Accepted, as for any same-site app.
- **R4. Old browsers** without Fetch Metadata cannot use checkout (C1). Measure the share from
  RUM before launch; the fallback page names the browsers that work.
- **R5. Stripe's first-party cookies** (`__stripe_mid`, `__stripe_sid`) are readable and
  writable by theme JS. They carry fraud signals, not card data; tampering degrades Radar
  scoring only.

## Rejected alternatives

- **Everything on `<shop>`, accounts included** (first draft): rejected by the review. Account
  sessions are fixable and passwords autofill into theme forms on the same origin.
- **Sandboxed or allowlisted theme islands**: removes the problem but breaks the theme model
  (Astro + Solid islands that merchants and the AI author freely), costs the A26 JS budget an
  extra document per island, and needs a postMessage API for everything islands do today.
- **Keep `checkout.<shop>`**: the per-domain DNS record and certificate SHOP-54 exists to drop.
- **One platform checkout domain** (`pay.<platform>/<shop>`): cross-site to the shop, so the
  cart handoff stays, consent splits again, and customers see a foreign domain at payment.

## Round-1 findings and response

Whether each response holds is in § Open issues.

| # | Finding (Astra, 2026-10-10) | Response |
|---|---|---|
| 1 | Blocker: anonymous visitors share one page token; theme fixes a session via `/_p/account/login` | No login or session on `<shop>` (A1); no token without a capability, none issued on failure (C4) |
| 2 | Blocker: R1 accepted data theft on Safari | Accounts off-origin; capability rotates on every PII write, HTML-only tokens (C4, C6); `state`-bound identify (A2); remainder is a pre-filled email the customer confirms (R1) |
| 3 | Token-free consent can forge customer preferences | Theme consent limited to anonymous cookie categories, `source` ignored (C9) |
| 4 | C3 does not remove an incumbent worker | C3 live before any shop goes public, verified (C3) |
| 5 | Cached theme assets at future checkout URLs | Namespace reserved before launch, build-id asset paths (C8) |
| 6 | Autofill credential theft | Passwords only on the account origin (A1); address autofill is not origin-bound (R2) |
| 7 | Stripe return secrets in history | C5 inventory includes Stripe parameters; capability expiry and revocation defined |
| 8 | `Origin: null` forms, one-click unsubscribe | `strict-origin` (C1), bearer unsubscribe endpoint (C11) |
| 9 | Checkout start and token rotation | Start as navigation POST (C10), rotation and stale tabs (C4) |
| 10 | Open redirect, placement replay hash, bfcache | Route map redirects (Routing), server-side placement recovery and bfcache reload (C7) |

## Open issues (review round 2, 2026-10-15)

Round-1 findings 1, 3, 8 and 10 are resolved by the design above; 2 is not; 4, 5, 6, 7 and 9
are partial and continue below.

- **O1. Blocker: identify-code injection (A2).** An attacker signs in to their own account,
  keeps the unused identify code, and has the theme navigate the victim to
  `/checkout/identify?code=…`. The victim's checkout is attached to the attacker's account in
  every browser. Fix direction: checkout starts the sign-in with a server-side transaction bound
  to the checkout capability and exact callback, consumed atomically with the code; unsolicited
  callbacks are rejected (RFC 9700 §4.5, authorization-code injection).
  **Resolved** (round 3) by the `state`-bound transaction, prefill-only identify link and the
  separate placement confirmation in A2.
- **O2. Blocker: planted guest checkout (C6, R1).** An attacker creates a guest checkout,
  enters dummy PII so the first-PII rotation has already happened, and plants the rotated
  capability in a browser without `__Host-Http-` support (Safari, Chrome < 140, Firefox < 143).
  The victim's later edits land in a checkout the attacker can read. Theme JS holding a planted
  capability and its token can also make the first PII call itself and read the replacement
  token. **Addressed** by rotation on every PII write and HTML-only tokens (C4, C6); the
  remaining pre-filled-email case is accepted as R1. **Resolved** (round 3) once capability
  checks run in the same transaction as the data access (C4).

Round 3 (2026-10-15) rated O1/O2 "sound with required changes": a read already authorised
could outlive a rotation (fixed in C4), the identify link was not browser-bound (now
prefill-only, A2), placement confirmation lacked its own binding (A2.4), and navigation POSTs
clashed with JSON-only checks (C4). All four are folded in above.
- **O3. Password managers (A1, R2).** Some managers match by registrable domain and can
  autofill on load (Bitwarden defaults), so a theme form on `<merchant domain>` can receive a
  login saved for `account.<merchant domain>` without the customer choosing it. "Passwords are
  origin-bound" is too strong. Either the custom account subdomain is an accepted risk, or
  password accounts stay on the platform domain.
- **O4. Origin history (C3, C8).** A merchant domain used elsewhere before may already have a
  service worker or cached assets in customers' browsers. Platform logs cannot prove otherwise.
  Fail closed: same-origin checkout only on demonstrably fresh origins until a verified
  recovery exists; the same provenance rule for asset URLs.
- **O5. Checkout start path (C10).** `/checkout/start` does not receive the shop `cart` cookie
  (`Path=/_p`). Keep the navigation POST at `/_p/checkout/start` or migrate the cookie path.
- **O6. Capability lifecycle (C4, C5).** Cookie expiry and deletion are not revocation. The
  server must enforce expiry, action/resource/shop scope, revocation and single use; a valid
  HMAC proves possession, not permission.

Implementation notes from the same review: `consent-contract.md` and `consentProxy` in
`apps/edge/src/gateway.ts` must change for C9; `apps/checkout/src/lib/placement.ts` must be
replaced for C7; the `no-referrer` remark in C1 applies to no-cors/same-origin requests, not
CORS-mode ones.

## Verification required in SHOP-54

A browser e2e with a hostile theme on the shop origin proves: `fetch`/`iframe`/prefetch of a
platform page get nothing; `window.open('/checkout')` returns a closed handle; service worker
registration fails and `Service-Worker: script` gets 403; `/_p/orders/*` without
`x-page-token`, with one minted for another capability, or with no capability cookie gets 403;
after `/o/<token>` and after a Stripe return → logo, `navigation.entries()` and
`document.referrer` hold no secret; a capability the attacker holds, planted in the victim's
browser, reads nothing the victim enters afterwards, including when the checkout already holds
attacker PII; a PII write made by theme JS returns no token; an identify callback without a
matching `state` or `__Host-Http-idt` is rejected; a checkout identified with the
attacker's account shows nothing in that account; a read authorised with capability K and
paused while the victim's write rotates K fails (deterministic race test); a placement whose
checkout rotated after the place-order POST aborts; placement of a
checkout linked to an account the browser is not signed in to produces a guest order;
`/_p/consent` cannot set `email_marketing`; on Chromium, `document.cookie` cannot
set `__Host-Http-cart`; `checkout.<shop>//attacker.example` does not redirect off-site. Unit
tests in `apps/edge` cover C1, C3, C4, C5, C8, C10 and C11 per route. SHOP-95 adds the account
origin's own checks (return-path allowlist, identify code single use, `Sec-Fetch-Site`).
