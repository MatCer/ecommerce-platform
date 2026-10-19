# Edge gateway

The edge (`apps/edge`) resolves the shop from the Host header, serves the theme artifact with
cache and security headers, forwards only allowed storefront operations under `/_p/*` to the API,
and keeps the Storefront and Internal APIs off the public API host.

## Sub-features

- `edge-tenancy` maps `demo.localhost`, `demo-sk.localhost` and `checkout.<shop>` to the right shop.
- `edge-cache` caches theme HTML at the edge and makes browsers revalidate.
- `edge-headers` sends COOP and the security headers on shop and checkout pages.
- `edge-proxy` serves `/_p/public/*` page models and cart operations.
- `edge-isolation` keeps `/storefront/*` and `/internal/*` unreachable on `api.localhost`.

## How to get to it (user POV)

- Any shop URL on the held stack, e.g. `http://demo.localhost:38080/`.
- The page-model endpoints the theme calls, e.g. `/_p/public/pages/product/tricko-basic`.

## Driving it with curl

Preconditions:

- Held stack up; `stack.sh doctor 3` is green.

- **Cache.** Run `curl -sS -o /dev/null -D - http://demo.localhost:38080/` twice. Headers show
  `Cache-Control: public, max-age=0, must-revalidate` and `X-Edge-Cache` (`HIT` on the repeat).
- **Security headers.** The same response and `http://checkout.demo.localhost:38080/` carry
  `Cross-Origin-Opener-Policy: same-origin`; checkout is `Cache-Control: no-store`.
- **Compression.** With `--compressed`, `Content-Encoding: gzip` (Caddy `encode gzip`).
- **Page model.** `curl -sS http://demo.localhost:38080/_p/public/pages/product/tricko-basic | jq '.product.name'`
  prints `"Tričko Basic"`; the response is `no-store` JSON.
- **Isolation.** `http://api.localhost:38080/storefront/x` and `/internal/x` return `404` with
  body `not found`.
- **TLS.** `curl -sSk -D - -o /dev/null https://demo.localhost:38443/` answers `HTTP/2 200`
  without an `alt-svc: h3` header.

## Gotchas

- Without `--compressed` Caddy does not gzip; timing or size numbers must use compression.
- An `alt-svc: h3` header means the stack was built from a checkout older than the h1/h2
  Caddyfile; rebuild it.
