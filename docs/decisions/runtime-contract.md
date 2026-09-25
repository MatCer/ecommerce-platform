# Decision record: storefront runtime contract (WP2)

Status: accepted for M1 (WP6/WP8 build on it; WP23 revisits the builder side).
Spec: §3, §9, §9.6, §12.3, amendments A1, A2, A4, A6, A7, A22, A26, A30.
Code: `apps/edge`, `packages/theme-kit`, `packages/storefront-sdk`, `themes/default`,
`apps/checkout`, stub API in `apps/mocks/src/storefront`.

## 1. Version matrix (pinned in the pnpm catalog)

| Component | Version | Note |
|---|---|---|
| Node | 24 (image `node:24.21.0-bookworm-slim`) | Debian: workerd needs glibc (Alpine does not work) |
| astro | 7.3.5 | `output: "server"` |
| @astrojs/cloudflare | 14.3.3 | pulls `@cloudflare/vite-plugin` 1.58.0 |
| @astrojs/solid-js / solid-js | 7.0.2 / 1.9.15 | |
| tailwindcss / @tailwindcss/vite | 4.3.3 / 4.3.3 | CSS-first `@theme reference` + token CSS |
| vite | 8.3.0 | (Rolldown) |
| wrangler | 4.137.0 | same miniflare/workerd as the vite plugin |
| miniflare | 5.20260921.0-alpha | the edge's runtime API |
| workerd | 1.20260921.1 | one copy in the lockfile |
| @astrojs/check | 0.9.10 | |
| playwright / lighthouse / @axe-core/playwright | 1.63.0 (Chromium 153) / 13.5.0 / 4.13.0 | theme-kit gates |
| web-vitals | 6.2.2 | SDK, lazy-loaded RUM |
| Caddy | 2.11.4 | HTTP + `tls internal` HTTP/2 listener |

Why these exact versions: pnpm's supply-chain release-age policy resolves `@cloudflare/vite-plugin`
to 1.58.0, which depends on wrangler 4.137.0 / miniflare 5.20260921.0-alpha / workerd
1.20260921.1. Pinning the catalog to the same set keeps a single workerd in the tree, so
`astro dev`/wrangler and the edge run the identical runtime. Miniflare 5 is published only with an
`-alpha` suffix (it is npm `latest` and what wrangler itself uses). Its README still documents the
v3 options; the real v5 API is `workers[].config` (wrangler-shaped: `manifest`, `env` bindings of
`type: "fetcher"`) plus `workers[].dev.outboundService`. **Upgrade rule:** bump all four together,
then re-run `pnpm test` (the edge suite starts real workerd) and `make perf`.

## 2. Artifact contract (A22)

`astro build` output is packed by `theme-kit pack` into an immutable, content-addressed directory:

```text
<ARTIFACT_ROOT>/<id>/manifest.json
                    /server/*.mjs      worker modules (entry: entry.mjs)
                    /client/**         static assets (/_astro/*, public/*)
<ARTIFACT_ROOT>/channels/<name>        pointer file → id (local publish, see §6)
```

`manifest.json` (`ArtifactManifest` in `packages/theme-kit/src/artifact.ts`):

| Field | Meaning |
|---|---|
| `schema` | 1 |
| `id` | 32 hex chars: sha256 over every `(path, sha256)` + tokens + CSP hashes + runtime + kind |
| `kind` | `theme` or `checkout` |
| `runtime.compatibility_date` / `compatibility_flags` | from the **platform** (`RUNTIME` in theme-kit): `2026-09-21`, no flags. The theme's generated `wrangler.json` is ignored |
| `runtime.main`, `runtime.modules[]` | explicit ES module list; the edge loads exactly these (no auto-discovery) |
| `assets` | URL path → `{sha256, size}` for `client/**`, minus `_headers`, `_redirects`, `_routes.json`, `.assetsignore` |
| `tokens` | validated `theme.tokens.json` (A6) or null |
| `csp.script_hashes` / `style_hashes` | hashes of the only allowed inline code (§5) |

Findings that shaped it:
- Astro 7's server bundle imports only `cloudflare:workers` (no `node:*`), so **no `nodejs_compat`**.
- The adapter enables an `IMAGES` binding and `SESSION` KV by default; the theme config disables both
  (`imageService: "passthrough"`, `session: false`).
- The adapter entry calls `env.ASSETS.fetch()` for unmatched routes and prerendered error pages, so a
  theme cannot run with `STOREFRONT` alone (see A7 deviation in §4).
- Pack is atomic (temp directory + rename), idempotent (same sources → same id, no rewrite), rejects
  symlinks and special files, and caps the artifact at 50 MB (A6).
- M1 serves the artifact from local disk (baked into the edge image by `scripts/build-artifacts.sh`).
  `bundle.tar.zst` in the private bucket (§9.3.3) is WP23's transport. The unpacked layout above is
  the contract between builder and edge.

**Static assets.** The edge serves them itself, before any worker runs:
- `/_astro/*`: content-hashed file names, `Cache-Control: public, max-age=31536000, immutable`.
  Looked up in the active artifact, then in `site.retained_artifacts`, so HTML rendered before a
  publish (edge cache, prerendered speculation, open tabs) keeps loading its old chunks.
  Retention is per site. Another tenant's artifacts are never searched (tested).
- Other `public/` files (favicon, …): active artifact only, `max-age=300`.
- Retention/GC policy for WP6: keep an artifact while it is active or listed in any site's
  `retained_artifacts`; list the previous 3 revisions or 7 days, whichever is longer.

## 3. Edge runtime behaviour (tested in `apps/edge/test/gateway.test.ts`)

- **One Miniflare (workerd) instance per distinct artifact and tenant**, created on first use.
  A30 says "per distinct artifact". WP2's review showed that a shared isolate lets theme code keep
  one request's context id in module state and replay it while serving another tenant. So
  untrusted theme artifacts get one isolate per tenant (still one instance per artifact when a
  single tenant uses it), and the binding refuses contexts of other tenants. The platform-owned
  checkout artifact stays shared across tenants. Within one tenant, a worker can still borrow a
  concurrent request's context; that exposes only the same tenant's public data and its own
  cache hints, which is accepted. Cold start
  is about 0.25 s (first render after restart, measured through Caddy); warm uncached render is
  about 9 ms, cache hit about 1.4 ms. The edge container uses 133 MiB with the theme and checkout
  instances running.
- **Restart:** state is rebuilt from disk + resolver; nothing is lost except in-flight handoff
  tokens (§7) and the HTML cache.
- **Eviction:** `pool.evict(id)` / `evictIdle(10 min)` (timer in `server.ts`). The next request
  recreates the instance.
- **Publish:** the site's `theme_artifact` pointer changes, then `POST /_edge/purge {tenant_id}` (the
  resolver cache and the HTML cache are purged). Running instances are never mutated.
- **Rollback:** the same, pointing back to an older artifact. The old artifact's assets are still on disk.
- Worker responses are fully buffered (needed for the cache and for closing the request context).
  Streaming SSR is a later optimisation. One deadline (10 s, 504) covers headers **and** body, and
  a body over 5 MB is cancelled (502). A never-ending stream cannot keep a context open.

## 4. Trust boundary (A7)

A theme worker gets exactly:
- `STOREFRONT`: a Node-side fetcher binding (`restrictedBinding` in `bindings.ts`). Allowed: `GET`
  `/shop`, `/pages/{home,search,blog}`, `/pages/{category,product,cms,blog}/<slug>`,
  `/search/suggest`, `/recommendations`, `/redirects/resolve`. Anything else returns 404. It rejects
  any credential or tenant header (`authorization`, `cookie`, `x-tenant`, `x-market`,
  `x-storefront-token`, `x-cart-token`, …) with 400, and rejects encoded separators (`%2f`, `%2e`, `\`, `//`).
- `ASSETS`: **deviation from A7**, which says one binding. It is read-only and serves only files
  listed in the same artifact's manifest. Required by the Astro Cloudflare adapter (see §2).

How the tenant is injected: the edge resolves the site from the Host header, opens a **request
context** (tenant, market, locale, storefront token, artifact id) and passes only an opaque 144-bit
id to the worker (`x-platform-ctx`). The binding looks the context up server-side. Unknown or
closed ids return 403, and an id issued to another artifact also returns 403, so a theme cannot
borrow a context. The context closes when the render finishes. Page-model `cache` hints (public,
max_age, tags) are collected there for the cache decision, so they are trustworthy.

Outbound network: `dev.outboundService` points at a tiny platform `egress-deny` worker that answers
403 and reports the target host. **Finding:** with a Node *fetcher* as `outboundService`, Miniflare
5 still passes `connect()` TCP sockets through to the internet. The hostile-theme test read a real
`HTTP/1.1 200` from example.com. A *worker* service without a `connect` handler closes that path.

Fan-out guard: at most **50 binding calls per render** (Workers' free-plan subrequest limit), over
that 429. The count is returned as `x-edge-subrequests` and budgeted by the gate (≤ 10).

Proved by `apps/edge/test/fixtures.ts` (hostile theme `/probe`): external fetch, direct
`api`/admin/internal URLs and TCP connect are all denied; admin/internal paths, `..` and encoded
`..` through the binding are refused; forged `x-tenant`, forged storefront token, `authorization`
and `cookie` headers get 400; forged or missing context gets 403; cart and POST through the binding
get 404; `env` contains only `ASSETS` and `STOREFRONT`; the only upstream call carries the
edge-injected tenant.

Production mapping (WP6 to confirm): Workers for Platforms user workers with (a) an outbound worker
that receives the tenant as dispatch parameters, or (b) a service binding to a platform wrapper
worker with the context passed as a signed/opaque token. Both give the same guarantee: the tenant
never comes from the worker.

The checkout app gets `CHECKOUT` (`GET /shop`, `GET /cart` for now) + `ASSETS`. The checkout-scoped
cart capability is injected from the `__Host-cart` cookie by the edge, never exposed to app code.

## 5. Headers, CSP, speculation rules

- Before resolving, the edge strips client `X-Tenant`, `X-Market`, `X-Locale`, `X-Storefront-*`,
  `X-Forwarded-*`, `Forwarded`, `X-Real-IP`, `X-Platform-*`, `X-Cart-Token`, `CF-Connecting-IP`.
  The worker receives a fixed `Accept: text/html` + the context id and **nothing from the
  client**: no cookies, no Authorization, no Accept/Accept-Language/User-Agent. HTML is cached
  per tenant/market/locale/path, so any client header the output could vary on would poison the
  cache; the locale comes from the market. Worker responses lose
  `set-cookie`, any worker CSP, hop-by-hop and `mf-*` headers. The worker URL is rebuilt from the
  configured scheme + validated Host.
- **Theme CSP:** `default-src 'self'; script-src 'self' <hashes>; style-src 'self' <hash>;
  style-src-attr 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self';
  frame-src 'none'; form-action 'self' <checkout origin>; frame-ancestors 'none'; base-uri 'self';
  object-src 'none'`. The hashes cover Astro's per-directive bootstrap scripts, the `astro-island`
  element, Solid's hydration/event-replay script and Astro's island `<style>`. They are computed at
  pack time from the **pinned** Astro/Solid (A26). Themes cannot add client directives because
  `astro.config.mjs` is platform-owned. Stylesheets are external (`inlineStylesheets: "never"`), so
  no build-specific style hashes are needed. `style-src-attr 'unsafe-inline'` is a deliberate
  trade-off: attribute styles cannot load resources beyond `img-src`.
- **Checkout CSP:** same base plus `js.stripe.com`, `frame-src` Stripe + `widget.packeta.com`,
  `connect-src 'self' https://api.stripe.com`, `form-action 'self'`.
- `Referrer-Policy: strict-origin-when-cross-origin`, `X-Content-Type-Options: nosniff`,
  `Cross-Origin-Opener-Policy: same-origin`, `Permissions-Policy` (payment only on checkout).
- Static assets carry `Content-Security-Policy: default-src 'none'; …; sandbox`. A theme could
  otherwise ship `public/x.html` (or an SVG) with inline script on the shop origin, outside the page
  CSP. Opened directly, such a document now runs in an opaque origin without script. Subresource
  use (JS, CSS, `<img>`) is unaffected.
- `Speculation-Rules: "/_p/speculation-rules.json"` on theme responses: prerender `/*` except `/_p/*`
  and `[rel~=nofollow], [data-no-prerender]`, eagerness `moderate` (A26, no inline script).
  Cross-document view transitions via CSS `@view-transition`.

## 6. Routing and cache policy (A1, A2)

| Host / path | Handler |
|---|---|
| `<shop>/_p/cart[/lines[/id]]`, `/_p/cart/coupons` | cart proxy (capability cookie → `x-cart-token`) |
| `<shop>/_p/checkout/start` (POST) | handoff (§7) |
| `<shop>/_p/public/<storefront op>` (GET) | public page-model reads for islands |
| `<shop>/_p/e`, `/_p/newsletter` (POST) | events beacon, newsletter |
| `<shop>/_p/speculation-rules.json` | rules |
| other `/_p/*`, `/_edge/*` | 404 |
| `<shop>/media/*` | media origin (image variants) |
| `<shop>/robots.txt`, `/llms.txt`, `/sitemap*.xml`, `/feeds/*` | API passthrough (`/storefront/v1/files/*`) |
| `<shop>/_astro/*`, public files | artifact assets (§2) |
| `<shop>` anything else (GET/HEAD only, else 405) | theme worker |
| `checkout.<shop>/start?h=` | handoff exchange |
| `checkout.<shop>/_p/tokens.css` | tenant tokens as CSS (A6) |
| `checkout.<shop>` assets / everything else | checkout artifact / checkout worker (always `no-store`) |
| `preview-*` | 404 until WP23 |
| internal port 8788: `/_edge/purge`, `/_edge/healthz` | bearer `EDGE_PURGE_TOKEN` (constant-time compare, ≥ 16 chars); not routed by Caddy |

State-changing `/_p/*` requests must be same-origin (`Origin` equal to the shop origin, else
`Sec-Fetch-Site: same-origin`). JSON bodies are limited to 16 kB and events to 64 kB. Bodies are
read as a stream and cancelled at the limit; they are never fully buffered first.

**Cache (A2):** only GET/HEAD on `/`, `/c/*`, `/p/<slug>`, `/pages/<slug>`, `/blog[/<slug>]`,
`/search` (optional `/xx` locale prefix). Never cached (one test each, `cache.test.ts` +
`gateway.test.ts`): the checkout origin, `/_p/*`, previews, `token`/`h`/`sig` query parameters,
requests with `Authorization`, responses with `Set-Cookie`, `Cache-Control: private|no-store|no-cache`,
a private page model, non-200 responses. TTL = min(60 s, theme `s-maxage`/`max-age`, page-model
`max_age`), so themes can only lower it, plus 300 s stale-while-revalidate. The key covers
(tenant, market, locale, artifact, host, path + sorted query without `utm_*`/`gclid`/`fbclid`/…).
Browsers get `max-age=0, must-revalidate`, so a purge is visible immediately. Purge works by
tenant, by tags (scoped to a tenant when both are given), or everything. Every purge bumps a
generation, and a render (or SWR refresh) that started before a purge cannot write its now-stale
result. A revalidation that finds the page no longer cacheable deletes the old entry. It is an
in-memory LRU of 64 MB with a 20k entry cap; key, headers and tags count against the byte budget.

## 7. Origin split and checkout handoff (A1, A4)

- Shop origin cookie: `cart=<API-minted 256-bit token>; Path=/_p; HttpOnly; Secure; SameSite=Lax`.
  **Deviation:** A1 says `Path=/_p/cart`, but then the cookie never reaches
  `POST /_p/checkout/start`. `/_p` still keeps it away from every theme request.
- `POST /_p/checkout/start` asks the API for a **checkout-scoped** cart token
  (`POST /storefront/v1/cart/checkout-token`; the API **rotates**: the shop capability is revoked,
  the new token is checkout-scoped and read-only until WP10, and the edge clears the shop cookie in
  the same response), mints a 43-char handoff token (single use, 60 s,
  stored as SHA-256, bound to `checkout.<shop>`) and 303s to `checkout.<shop>/start?h=…`
  (`Referrer-Policy: no-referrer`). Only a same-site navigation can redeem it
  (`Sec-Fetch-Site: same-site|same-origin`, i.e. the shop's own 303). A link planted by another
  site or opened from mail is refused without burning the token, so an attacker cannot push their
  cart into a victim's checkout. The exchange consumes it atomically, sets
  `__Host-cart=…; Path=/; HttpOnly; Secure; SameSite=Lax` and 303s to `/`. Replaying the token, an
  expired token, or the same token on another checkout host all return 400.
  `__Host-` cookies cannot be tossed in from the shop origin via `Domain=`.
- ponytail: handoff records live in edge memory (one process). WP6 moves them to the API
  (`POST /internal/v1/handoffs`) when the edge scales out.

## 8. Performance (§9.6, A26)

Measured with `make perf` (`theme-kit measure`): Lighthouse 13 default mobile preset (Moto G Power,
slow 4G, 4× CPU), median of 3 runs. JS = every script transferred until network idle, then after
scrolling the full page, counted as gzip -9 of each body plus inline bootstrap scripts. Stack:
compose `edge` + stub API behind Caddy **over TLS + HTTP/2**, fixture photos ~35–60 kB at 720 w AVIF.

| Page | LCP | TBT | CLS | JS gz (A26) | JS gz + RUM sampled | 3rd-party | axe serious/critical | storefront calls |
|---|---|---|---|---|---|---|---|---|
| `/` | 1128 ms | 0 ms | 0.009 | 21.1 kB | 26.0 kB | 0 | 0 | 2 |
| `/c/trika` | 1202 ms | 0 ms | 0.019 | 22.2 kB | 27.0 kB | 0 | 0 | 2 |
| `/p/tricko-basic` | 1202 ms | 0 ms | 0.000 | 24.4 kB | 29.2 kB | 0 | 0 | 2 |

Final run on the compose stack. Repeated 3-run medians of the same build gave PDP LCP between
1.20 and 1.35 s; home (1.13 s) and category (1.20 s) were stable.

Budget: LCP ≤ 1.5 s, TBT ≤ 150 ms, CLS ≤ 0.05, JS ≤ 30 kB (home 35). **All pass.** Every planned
island is wired: variant picker + add to cart (`BuyBox`), mini cart drawer with free-shipping bar
and the handoff form, search typeahead (ARIA combobox), facet filters as a plain GET form enhanced
on hydration, gallery, consent banner, newsletter, RUM (consented + 10 % sampled, `web-vitals`
loaded lazily). Solid runtime ≈ 9 kB gz, shared automatically by Vite. No router, one root per
island, cart state shared through a module signal.

What it took (each change was measured):
1. **Measure over HTTP/2.** Over plain HTTP/1.1 through Caddy the same build scored LCP 1.50–1.65 s.
   Lantern models 6 connections per origin and a TCP handshake each, which punishes pages that
   load about 12 small island modules. Removing all islands brought it to 1.28 s. Production is
   h2/h3, so the gate measures over h2 (`tls internal` listener, `HTTPS_PORT`). This is a
   measurement amendment, not a budget change.
2. **`sizes` must describe the real slot.** `50vw` on a 412 px viewport at DPR 1.75 needs 360.5 px,
   which is 1 px more than the 360 w variant, so browsers took the 720 w (4× heavier) file. Now
   `calc(50vw - 1.5rem)` + a 480 w variant.
3. **Gallery:** only the LCP image is in the server HTML. Lazy images inside a horizontal
   scroll-snap strip are within Chrome's lazy-load margin and were downloaded at once (≈ 370 kB
   in the first measurement, before the `sizes` fix).
4. **One web font** (Archivo, display + prices, 23 kB, preloaded); body uses the system stack. Two
   preloaded fonts competed with the LCP image.
5. Home: the measured LCP element was a lazily loaded featured card, not the category tiles. It is
   now eager + `fetchpriority=high` + preloaded.

Headroom: the PDP has 5.6 kB of JS left, but only 0.8 kB when the RUM sample loads `web-vitals`
(attribution build, 4.9 kB). Recommendation for WP8/WP14: count RUM outside the lab budget (it
loads after consent, for 10 % of visits) or replace `web-vitals/attribution` with a ~1 kB
`PerformanceObserver` reporter.

Local-dev scheme note: the edge's canonical scheme is `http` locally (Caddy :80). The TLS listener
exists for measurement. The handoff redirect follows the canonical scheme, so the smoke test runs
over http.

## 9. What later work packages must implement

**WP6 (storefront runtime):**
- `GET /internal/v1/resolve?host=` returning the `Site` shape (`apps/edge/src/sites.ts`: tenant,
  market, locale, shop_host, storefront_token, theme_artifact, retained_artifacts), replacing
  `StaticResolver` + `ChannelResolver`. Service token for edge → API.
- The real Storefront API behind the same paths the stub serves (`apps/mocks/src/storefront`): page
  models with `seo` + `cache {public, max_age, tags}`, cart with API-minted capability tokens
  (hashed at rest, scoped shop/checkout), `POST /cart/checkout-token` (rotates: revokes the shop
  capability), `/events`, `/newsletter/subscribe`,
  `/files/{robots.txt,llms.txt,sitemap*.xml,feeds/*}`.
- Generated SDK types replacing `packages/storefront-sdk/src/types.ts` (same shapes), plus the
  additions from `ai-edit-prompts.md` (card images, batch cards, size guide, dispatch cutoff,
  consent-gated storage, `lcpImage()`, i18n catalogs).
- Handoff tokens in the API. The purge call from API to edge on publish/content changes. Artifact
  GC per §2. Edge-side counters before consent (A20). Streaming SSR if TTFB needs it.
- Confirm the WfP mapping of §4 and the `ASSETS` exception.

**WP8 (default theme):** the full design on top of this probe (tokens, components, a11y states),
i18n from the start, keeping `make perf` green. Do not regress the §8 fixes, and keep `sizes` and
preloads in one helper.

**WP23 (builder):** sandboxed build → `theme-kit lint`, `astro check`, `astro build`,
`theme-kit pack` → `bundle.tar.zst` in the private bucket → unpack on the edge. Gates =
`scripts/theme-gates.sh` (+ image-bytes budget, desktop CLS, feature assertions). Preview hosts:
separate cache namespace, never cached (already bypassed by the policy).

## 10. Open risks

1. **Miniflare 5 is alpha** and its docs lag. The edge uses a small API surface (`workers[].config`,
   fetcher bindings, `dev.outboundService`), all covered by tests. Pin, test on upgrade.
2. **Egress** relies on an undocumented Miniflare detail (worker vs fetcher outbound for `connect()`).
   The test fails loudly if that changes. Prod WfP has its own egress controls to verify in WP6.
3. **`ASSETS` binding** widens A7 slightly. The risk is low (read-only, same artifact), but the
   amendment text should say so.
4. **In-memory state** (handoffs, HTML cache, resolver cache) is per process. Fine for M1 local;
   WP6 must externalise handoffs before running more than one edge process.
5. **Lab numbers use synthetic fixtures and a fast stub API.** Real TTFB (Postgres page models) adds
   directly to LCP. Re-measure in WP8/WP15 against the seeded API. LCP headroom is about 0.25–0.35 s.
6. **Lantern quantisation:** single runs move in ~75 ms steps. Use 3 runs (median) for decisions.
7. **Theme CSS contract:** `style-src-attr 'unsafe-inline'` and external-only stylesheets are
   deliberate. Revisit if critical-CSS inlining is ever needed; that needs build-time style hashes.

## 11. WP6: from stubs to the platform (supersedes the local-only parts above)

- **Sites** come from `GET /internal/v1/resolve?host=` (service token `INTERNAL_API_TOKEN`,
  `ApiResolver`), cached 60 s and purged by `/_edge/purge`. The response adds
  `storefront_token`, `theme_artifact` (`null` until published → 503), `retained_artifacts` and
  `checkout_artifact`. `sites.local.json`, `StaticResolver.fromFile` and channel pointers are gone
  from the runtime (the static resolver stays for tests).
- **Artifacts (A22, A30):** `make theme-build` packs the theme and checkout and runs
  `api admin publish-artifacts`: every file goes to the private bucket under
  `artifacts/<id>/…`, the id is registered in `platform.theme_artifacts`, the `default-theme` /
  `checkout` channels move, and every tenant that follows the default gets a new published
  `theme_revisions` row + `theme_active` (one shared artifact, revision per tenant). The edge
  downloads a missing artifact through `GET /internal/v1/artifacts/{id}/{path}`, recomputes the
  content address from the bytes (`theme-kit artifactId`) and only then renames it into place.
  **Deviation:** per-file objects instead of `bundle.tar.zst`: no archive format to parse, and
  integrity comes from the content address. Retention: previous 3 revisions or 7 days.
- **Handoff (A1, A4):** state moved to the API. `POST /storefront/v1/cart/handoff` (shop
  capability) revokes the shop capability and returns a single-use 60 s token (SHA-256 in
  `checkout_handoffs`, bound to tenant + market + cart); `POST /storefront/v1/checkout/handoff`
  consumes it atomically and mints the checkout capability. The edge keeps the Sec-Fetch-Site
  check and the cookies; it holds no state, so it can scale out.
- **Redirects:** a theme 404 triggers `GET /storefront/v1/redirects/resolve?path=`; only
  same-shop paths are followed (checked by the API and again by the edge), `no-store`.
- **Media:** `/media/<tenant>/…` only (the public bucket is shared; other prefixes 404).
- **WfP mapping (confirmed for M1):** option (b): the `STOREFRONT` wrapper binding with the
  opaque per-render context id; the tenant, market and storefront token never come from theme
  code. The `ASSETS` exception stays (read-only, same artifact).
- **Storefront API authorization:** token → tenant (401), market loaded under that tenant's RLS
  (403 `market_mismatch`), a disagreeing `X-Tenant` is 403. Caddy no longer proxies
  `api.localhost/storefront/*`.
- **Publishing safety:** `make theme-build` runs `theme-kit verify` (content address recomputed
  from the files, exact file set) before `publish-artifacts`; registration is serialized per id
  (advisory lock) and a registered id is immutable (different bytes → `409 artifact_mismatch`).
  The edge additionally refuses a manifest whose runtime section (entry point, compatibility
  date/flags) differs from the platform's, and an artifact of the wrong kind for theme/checkout.
- **Measured on the seeded demo shop** (`make perf`, 3-run medians, compose stack over h2):

  | Page | LCP | TBT | CLS | JS gz | calls |
  |---|---|---|---|---|---|
  | `/` | 1053 ms | 0 | 0.017 | 21.3 kB | 2 |
  | `/c/trika` | 1053 ms | 0 | 0.007 | 22.4 kB | 2 |
  | `/p/tricko-basic` | 1352 ms | 0 | 0.000 | 24.6 kB | 2 |

  The PDP needed a 720 px image variant: with only 640/960 the 412 px @1.75 viewport loaded the
  960 px file and LCP was 1.8 s. The media pipeline now also emits 480 and 720.
- **Search (WP7) integration:** category and search page models list through
  `commerce::search` (variant-correct facets, results rehydrated from Postgres) and fall back to
  the Postgres `storefront::listing` when search is degraded (A27: Meilisearch is not part of
  core readiness; verified by stopping it: pages 200, typeahead 503). Listing URLs use the
  engine's `f.<facet key>` parameters in both paths. `/storefront/v1/search` and
  `/search/suggest` use the storefront-token model like every storefront call; islands reach
  them as `/_p/public/search*`. `make seed` queues a full index rebuild.

## 12. WP8: the default theme on the platform

- **Locale prefixes (§9.1):** a market's non-default locales live under `/<locale>/…`
  (`demo-sk.localhost/cs/…`). The edge (`splitLocale`) strips the prefix for theme renders and
  `/_p/public/*` and renders with that locale; the locale is part of the HTML cache key, and
  the default locale or a locale of another market is never stripped (404). Cart routes stay
  unprefixed (cookie `Path=/_p`). The API builds every page-model href with
  `Context::path()` and canonicals with `Context::page_url()`; hreflang alternates cover every
  (market, locale). `ShopModel.base_path` is what themes put in front of the links they build
  themselves; `ShopModel.checkout_url` is the checkout origin (account, `/withdraw`).
- **New `/_p/*` routes:** `POST /_p/consent` (same-origin JSON, forwarded to
  `/storefront/v1/consent`; answered 202 `{recorded:false}` while the API has no such route,
  WP9). `POST /_p/newsletter` also accepts a plain urlencoded form and answers 303 back to the
  same-origin `Referer` (else `/`) with `?newsletter=ok|invalid#newsletter`.
- **Consent banner** is a platform component in the SDK (`@platform/storefront-sdk/consent-banner`,
  Solid + plain CSS on token variables). Nothing is stored before a choice; the choice is the
  `consent` cookie plus a JSON beacon to `/_p/consent`; withdrawing `personalization` clears the
  SDK's device storage (`consentStorage`).
- **SSR hazard found:** a top-level Solid `onCleanup` that touches `document` runs when the
  server render is disposed and hangs Astro's async Solid renderer ("renderToString timed out",
  the edge answers 504 after 10 s). Browser-only setup and teardown belong inside `onMount`.
- **Measured** (`make perf`, seeded demo shop, 3-run medians over h2, all islands wired):

  | Page | LCP | TBT | CLS | JS gz (A26) | + RUM sampled | calls |
  |---|---|---|---|---|---|---|
  | `/` | 1277 ms | 0 | 0.000 | 22.5 kB | 24.5 kB | 2 |
  | `/c/trika` | 1277 ms | 0 | 0.000 | 23.0 kB | 25.0 kB | 2 |
  | `/p/tricko-basic` | 1427 ms | 0 | 0.000 | 27.5 kB | 29.5 kB | 3 |
  | `/search?q=mikina` | 1126 ms | 0 | 0.000 | 23.0 kB | 25.0 kB | 2 |

  The gate now also judges the RUM-sampled visit (A26 counts every script a visit downloads).
  axe (WCAG 2.2 AA tags): 0 serious/critical, no CSP violations, no third-party origins. What
  it took: the cart drawer is a dynamic import on first open (not counted, never downloaded by
  most visitors; Solid's `lazy()` was avoided because it adds ~1 kB of Suspense runtime to every
  page), likewise the consent panel (only without a choice) and the recently-viewed list (only
  with `personalization`), the newsletter is a plain form (no island), extra gallery photos wait for the load
  event, card srcsets are capped at 720 w (HTML weight), `web-vitals` standard build instead of
  `/attribution` (−2.5 kB when sampled), and the listing's no-JS submit button lives in
  `<noscript>` (hiding it on hydration shifted the toolbar, CLS 0.03).
