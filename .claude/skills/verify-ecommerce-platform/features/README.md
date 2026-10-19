# Ecommerce platform verification map

The maintained source for verifying user-facing behavior of the platform. Read the index, then use
the matching feature file as the recipe. `SKILL.md` owns launch, doctor, evidence and cleanup.

## Baseline preconditions

- Storefront, checkout and edge recipes need a held stack (`stack.sh up <slot>`, SKILL.md B):
  seeded demo shop `demo` (CZ market, `demo.localhost`) and `demo-sk` (SK market), owner
  `owner@lnen.example`, the review fixture and the same-origin origins `demo-fresh(-sk).localhost`.
- API recipes run on the native API (SKILL.md A, empty database) or on a held stack (seeded).
- `stack.sh doctor <slot>` names your worktree as owner and shows the commit you mean to prove.
- Ports below are slot 3 (`38080` HTTP, `38443` HTTPS, Mailpit `38025`); slot 4 uses `4xxxx`.
- Never drive the dev stack on `8080` for a proof.

## Driving conventions

- Prefer the repo's Playwright specs (`stack.sh e2e 3 ...`); write an ad-hoc script only for a path
  no spec covers.
- Locate by ARIA role and Czech accessible name on the CZ shop, `data-testid` on checkout and order
  pages, never by CSS position.
- Every browser context sets the consent cookie (`consent=""`) for each shop origin it visits,
  unless the consent banner is what you are proving.
- New Firefox tabs open `<origin>/favicon.svg` before a shop or checkout page.

## Proof and skip reporting

- Capture the action and the resulting state; UI proof is a screenshot plus an ARIA snapshot.
- Mutations are confirmed through a second view (order page, Mailpit, SQL, admin list).
- Record the feature ID, entry point, slot and commit with every artifact.
- Report an unreachable path with the command tried and the unmet precondition; never report it as
  verified through another path.

## Feature entry contract

Each feature file starts with an H1 and one paragraph, then four H2s in order: `Sub-features`,
`How to get to it (user POV)`, `Driving it with <harness>` (starting with `Preconditions:`), and
`Gotchas`.

## Features

- [Storefront browsing](./storefront.md): home, category, search, product page, cart drawer.
- [Checkout and payment](./checkout.md): cart handoff, guest order, fake gateway, order page, email.
- [Edge gateway](./edge.md): tenancy by host, cache and security headers, `/_p/*` routes.
- [API and staff access](./api.md): health, OpenAPI, Admin API auth, staff sign-in.
