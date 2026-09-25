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

```http
POST /_p/consent
Content-Type: application/json
Origin: https://<same origin>

{"purposes": {"analytics": true, "ads": false, "personalization": true},
 "text_version": "2026-09-25"}
```

- `purposes`: any subset of the five; omitted purposes are left as they are. Unknown keys are
  refused (`422`). At least one purpose.
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

## Cookies (set by the edge, only after a choice)

Before the first choice nothing is stored on the device (A20). The first `POST` sets:

| Cookie | Value | Attributes | Readable by scripts |
|---|---|---|---|
| `__Secure-consent_id` | anonymous subject id, 32 hex | `Domain=<shop host>; Path=/; Secure; SameSite=Lax; HttpOnly; Max-Age=13 months` | no |
| `consent` | `<text_version>.<mask>` | same, without `HttpOnly` | yes |

`Domain=<shop host>` makes both visible on `checkout.<shop host>` (the preferences page and
sign-in linking run there).

`mask` has one character per purpose in the order `analytics, ads, personalization,
email_marketing, review_invites`: `1` granted, `0` refused, `-` not asked. Example:
`consent=2026-09-25.101--`.

A theme uses the `consent` cookie only to decide what to render (show the banner when it is
missing or its version differs from `consent.text_version`; start the RUM beacon only with
`analytics` granted). It is a UI hint, not an authorization.

## Reading the state: `GET /_p/consent`

Same routes, no body. Answers the `ConsentState` of the signed-in customer (checkout origin)
or of the anonymous subject; `text_version: null` when there is no choice yet. Sets no
cookies. Themes normally read the `consent` cookie instead (no request).

## Preferences page

`checkout.<shop>/consent` (`GET /shop` → `consent.preferences_url`), linked from the banner
and the footer. It posts the same contract with `source: "preferences"`. Signed-in customers
also see `email_marketing` and `review_invites`.

## Sign-in linking

When a customer signs in on the checkout origin, the anonymous subject's latest choices that
are newer than the customer's own are copied to the customer (`source: linked`), so the most
recent decision of the person wins. Later banner choices on the shop origin update the
anonymous subject and are linked again at the next sign-in.
