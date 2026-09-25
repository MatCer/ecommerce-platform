# Decision record: consent contract (WP9, spec A20)

What themes (the default theme's banner, WP8) and the checkout app may rely on when they ask
for, show or change consent. The server is the only authority: anything that depends on
consent (forwarding events, marketing mail, personalization) resolves it from
`consent_records` at execution time (`commerce::consent::current`). Purposes that a client puts
into a beacon or any other payload are never trusted.

## Purposes

`analytics`, `ads`, `personalization`, `email_marketing`, `review_invites` (A20). The banner
asks for the ones in `GET /shop` → `consent.purposes` (M1: `analytics`, `ads`,
`personalization`); the email purposes belong to checkout and the account (signed-in
customers change them on the preferences page).

## Recording a choice: `POST /_p/consent`

On the shop origin (`<shop>/_p/consent`) and the checkout origin (`checkout.<shop>/_p/consent`).
The platform banner (`@platform/storefront-sdk/consent-banner`, `saveConsent`) sends it as a
beacon; themes should use that component rather than post themselves.

```http
POST /_p/consent
Content-Type: application/json
Origin: https://<same origin>

{"purposes": {"analytics": true, "ads": false, "personalization": true},
 "text_version": "2026-09-25", "source": "banner"}
```

- `purposes`: any subset of the five; the banner sends every purpose it offered, granted or
  refused. Omitted purposes are left as they are. Unknown keys are refused (`422`). At least one.
- `text_version`: the one from `GET /shop` → `consent.text_version` (the wording the person
  saw), `[A-Za-z0-9_-]{1,32}`.
- `source` (optional): `banner` (default) or `preferences`.
- Same-origin JSON only (`Origin` equal to the page's origin, else `Sec-Fetch-Site:
  same-origin`), 16 kB max. Anything else is `403`/`415`/`413` from the edge.
- Response `200` with the anonymous subject's current state (`ConsentState`):
  `{"purposes": {"analytics": true, "ads": false, "personalization": true,
  "email_marketing": null, "review_invites": null}, "text_version": "2026-09-25"}`.
  `null` = never asked.

Each purpose becomes one append-only `consent_records` row (`subject`, `purpose`, `granted`,
`text_version`, `source`, IP hashed with the daily rotating salt, time). On the checkout origin a
signed-in customer's choice is recorded for the customer as well.

## Cookies (only after a choice)

Before the first choice nothing is stored on the device (A20).

| Cookie | Value | Attributes | Readable by scripts | Written by |
|---|---|---|---|---|
| `__Secure-consent_id` | anonymous subject id, 32 hex | `Domain=<shop host>; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=13 months` | no | edge, on the first `POST` |
| `consent` | granted purposes, comma-separated, URL-encoded (`analytics%2Cpersonalization`; empty = decided, nothing granted) | `Domain=<shop host>; Path=/; Secure; SameSite=Lax; Max-Age=180 days` | yes | the SDK at once on the shop origin, and the edge after every recorded `POST` (same name, domain and path, so it is one cookie) |

`Domain=<shop host>` makes both visible on `checkout.<shop host>` (the preferences page and
sign-in linking run there), and lets the preferences page update the banner's cookie.

A theme uses the `consent` cookie (`readConsent`, `hasConsent`) only to decide what to render
(banner shown while it is missing; RUM only with `analytics`; recently viewed only with
`personalization`). It is a UI hint, not an authorization.

## Reading the state: `GET /_p/consent`

Same routes, no body. Answers a `ConsentState`: the cookie purposes (`analytics`, `ads`,
`personalization`) of this browser's anonymous subject and, when signed in on the checkout
origin, the email purposes of the customer; `text_version: null` when there is no choice yet.
Sets no cookies. Themes normally read the `consent` cookie instead (no request).

## Preferences page

`checkout.<shop>/consent` (`GET /shop` → `consent.preferences_url`), linked from the banner
and the footer. It posts the same contract with `source: "preferences"`. Signed-in customers
also see `email_marketing` and `review_invites`.

## Sign-in linking

When a customer signs in on the checkout origin, the anonymous subject's latest choices that
are newer than the customer's own are copied to the customer (`source: linked`), so the most
recent decision of the person wins. Later banner choices on the shop origin update the
anonymous subject and are linked again at the next sign-in.
