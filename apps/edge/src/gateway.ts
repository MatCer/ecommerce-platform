import { timingSafeEqual } from "node:crypto";
import { type ArtifactManifest, readManifest, tokensToCss } from "@platform/theme-kit";
import type { ArtifactFetcher } from "./artifacts.ts";
import {
  assetsBinding,
  CHECKOUT_OPERATIONS,
  ContextRegistry,
  CTX_HEADER,
  contentType,
  readAsset,
  restrictedBinding,
  STOREFRONT_OPERATIONS,
  type Upstream,
} from "./bindings.ts";
import { HtmlCache, normalizeUrl, requestVerdict, responseVerdict } from "./cache.ts";
import { type Counters, templateOf } from "./counters.ts";
import {
  contentSecurityPolicy,
  securityHeaders,
  stripUntrusted,
  workerRequestHeaders,
  workerResponseHeaders,
} from "./headers.ts";
import { type NodeBinding, WorkerPool } from "./runtime.ts";
import {
  CachedResolver,
  classifyHost,
  normalizeHost,
  type Site,
  type SiteResolver,
  splitLocale,
} from "./sites.ts";

export interface GatewayOptions {
  artifactRoot: string;
  resolver: SiteResolver;
  /**
   * The platform-owned checkout artifact (same bundle for all tenants, spec §9.4) when the
   * resolved site does not name one (tests, local fallbacks).
   */
  checkoutArtifact?: string;
  /** Downloads artifacts missing under `artifactRoot` from the API (A22); absent = local only. */
  artifacts?: ArtifactFetcher;
  /** Storefront API origin, e.g. `http://api:8000`. */
  apiOrigin: string;
  /** Public media origin (image variants), proxied under `/media/*`. */
  mediaOrigin: string;
  /** Public scheme of storefront URLs. Never taken from client headers. */
  scheme?: "http" | "https";
  /** Bearer token for `/_edge/purge` (distinct service token, spec A7). */
  purgeToken: string;
  /**
   * The pickup-point widget library (`PACKETA_WIDGET_URL`); its origin is allowed as a script
   * and frame source on the checkout origin only. Default: Packeta's.
   */
  packetaWidgetUrl?: string;
  upstream?: Upstream;
  /** Cookieless page/search counters (A20); flushed to the API by the server. */
  counters?: Counters;
  renderTimeoutMs?: number;
  log?: (event: Record<string, unknown>) => void;
}

const SPECULATION_RULES = JSON.stringify({
  prerender: [
    {
      where: {
        and: [
          { href_matches: "/*" },
          { not: { href_matches: "/_p/*" } },
          { not: { selector_matches: "[rel~=nofollow], [data-no-prerender]" } },
        ],
      },
      eagerness: "moderate",
    },
  ],
});

const CART_OPS: { method: string; path: RegExp }[] = [
  { method: "GET", path: /^$/ },
  { method: "POST", path: /^\/lines$/ },
  { method: "PATCH", path: /^\/lines\/[A-Za-z0-9_-]{1,64}$/ },
  { method: "DELETE", path: /^\/lines\/[A-Za-z0-9_-]{1,64}$/ },
  { method: "POST", path: /^\/coupons$/ },
  { method: "DELETE", path: /^\/coupons\/[A-Za-z0-9_-]{1,64}$/ },
];

/** Customer account operations on the checkout origin (`/_p/account/*`, WP9, A4, A5). */
const ACCOUNT_OPS: { method: string; path: RegExp }[] = [
  { method: "POST", path: /^\/orders\/[0-9a-f-]{36}\/withdrawal$/ },
  { method: "POST", path: /^\/magic-link$/ },
  { method: "POST", path: /^\/magic-link\/consume$/ },
  { method: "POST", path: /^\/login$/ },
  { method: "POST", path: /^\/logout$/ },
  { method: "GET", path: /^\/me$/ },
  { method: "POST", path: /^\/password$/ },
  { method: "GET", path: /^\/addresses$/ },
  { method: "POST", path: /^\/addresses$/ },
  { method: "PUT", path: /^\/addresses\/[0-9a-f-]{36}$/ },
  { method: "DELETE", path: /^\/addresses\/[0-9a-f-]{36}$/ },
];

/** Checkout operations on the checkout origin (`/_p/checkout/*`, WP10, spec §8.2). */
const CHECKOUT_OPS: { method: string; path: RegExp }[] = [
  { method: "GET", path: /^$/ },
  { method: "PUT", path: /^\/(contact|addresses|shipping|payment)$/ },
  { method: "POST", path: /^\/place-order$/ },
];

/** The order page's operations (`/_p/orders/<order token>/*`, A4, A10). */
const WITHDRAW_OPS: { method: string; path: RegExp }[] = [
  { method: "POST", path: /^$/ },
  { method: "POST", path: /^\/[0-9a-f]{64}$/ },
];

const ORDER_OPS: { method: string; path: RegExp }[] = [
  { method: "GET", path: /^\/[0-9a-f]{64}$/ },
  { method: "GET", path: /^\/[0-9a-f]{64}\/payment$/ },
  { method: "POST", path: /^\/[0-9a-f]{64}\/payment-attempts$/ },
  { method: "POST", path: /^\/[0-9a-f]{64}\/payment-attempts\/[0-9a-f-]{36}\/init$/ },
  // WP11: the Stripe test simulator (the API answers 404 unless it runs without a real key).
  { method: "POST", path: /^\/[0-9a-f]{64}\/payment-attempts\/[0-9a-f-]{36}\/simulate$/ },
];

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** Where the fake pay page may send the customer back to: the order page only. */
const ORDER_PAGE_RE = /^\/o\/[0-9a-f]{64}$/;

const MAX_JSON_BODY = 16 * 1024;
const MAX_EVENTS_BODY = 64 * 1024;
/** Context, limit and up to 12 recently viewed ids fit well under this. */
const MAX_RECOMMENDATIONS_QUERY = 1024;
const SHOP_CART_COOKIE = "cart";
const CHECKOUT_CART_COOKIE = "__Host-cart";
/** Customer session (A1): checkout origin only, host-only via the `__Host-` prefix. */
const SESSION_COOKIE = "__Host-sid";
const SESSION_MAX_AGE = 30 * 86_400;
const CLEAR_SESSION_COOKIE = `${SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0`;
/**
 * Consent (A20, `docs/decisions/consent-contract.md`): the anonymous subject id (HttpOnly) and a
 * script-readable summary, both for the shop host and its checkout subdomain, set only after a
 * choice. 13 months.
 */
const CONSENT_ID_COOKIE = "__Secure-consent_id";
const CONSENT_COOKIE = "consent";
const CONSENT_ID_MAX_AGE = 396 * 86_400;
/** Same lifetime and format as the SDK's own write (`@platform/storefront-sdk/consent`). */
const CONSENT_MAX_AGE = 180 * 86_400;
const CONSENT_ID_RE = /^[0-9a-f]{32}$/;
/** Granted purposes, comma-separated; empty = decided, nothing granted. */
const CONSENT_SUMMARY_RE = /^([a-z_]{1,32}(,[a-z_]{1,32}){0,9})?$/;
/** One address, as Caddy appends it to `X-Forwarded-For`. */
const IP_RE = /^[0-9A-Fa-f:.]{2,45}$/;

const text = (status: number, body: string, headers: Record<string, string> = {}) =>
  new Response(body, {
    status,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "cache-control": "no-store",
      ...headers,
    },
  });

const problem = (status: number, code: string, detail: string) =>
  Response.json(
    { type: "about:blank", title: code, status, code, detail },
    {
      status,
      headers: { "content-type": "application/problem+json", "cache-control": "no-store" },
    },
  );

function readCookie(headers: Headers, name: string): string | undefined {
  for (const part of (headers.get("cookie") ?? "").split(";")) {
    const i = part.indexOf("=");
    if (i > 0 && part.slice(0, i).trim() === name) return part.slice(i + 1).trim() || undefined;
  }
  return undefined;
}

const TOKEN_RE = /^[A-Za-z0-9_-]{16,128}$/;
const IDEMPOTENCY_KEY_RE = /^[\x21-\x7e]{1,255}$/;
/** A path on the same shop: one leading slash, no `//` or `/\` host smuggling, no spaces. */
const SAME_SHOP_PATH = /^\/(?![/\\])[^\s\\]*$/;
const CLEAR_CART_COOKIE = `${SHOP_CART_COOKIE}=; Path=/_p; HttpOnly; Secure; SameSite=Lax; Max-Age=0`;
/** `GET /_p/cart` before the first write (the SDK's `EmptyCart`): no cart is created. */
const EMPTY_CART = {
  id: null,
  lines: [],
  item_count: 0,
  coupon: null,
  subtotal: null,
  discount: null,
  total: null,
  vat: [],
  vat_total: null,
  free_shipping_remaining: null,
};

export function createGateway(opts: GatewayOptions) {
  const scheme = opts.scheme ?? "https";
  // The widget script and its iframe come from here (checkout CSP only).
  const widgetOrigin = opts.packetaWidgetUrl ? new URL(opts.packetaWidgetUrl).origin : undefined;
  const upstream: Upstream = opts.upstream ?? ((r) => fetch(r));
  const log = opts.log ?? ((e) => console.log(JSON.stringify(e)));
  const resolver =
    opts.resolver instanceof CachedResolver ? opts.resolver : new CachedResolver(opts.resolver);
  const registry = new ContextRegistry();
  const cache = new HtmlCache();
  const manifests = new Map<string, Promise<ArtifactManifest>>();
  const revalidating = new Set<string>();
  let outboundDenied = 0;

  const manifest = (id: string) => {
    let m = manifests.get(id);
    if (!m) {
      m = (async () => {
        await opts.artifacts?.ensure(id);
        return readManifest(opts.artifactRoot, id);
      })();
      manifests.set(id, m);
      m.catch(() => manifests.delete(id));
    }
    return m;
  };

  const pool = new WorkerPool({
    artifactRoot: opts.artifactRoot,
    bindings: (m, scope): Record<string, NodeBinding> => {
      const common = { registry, artifactId: m.id, scope, apiOrigin: opts.apiOrigin, upstream };
      const assets = assetsBinding(opts.artifactRoot, m);
      return m.kind === "theme"
        ? {
            STOREFRONT: restrictedBinding({
              ...common,
              name: "STOREFRONT",
              operations: STOREFRONT_OPERATIONS,
            }),
            ASSETS: assets,
          }
        : {
            CHECKOUT: restrictedBinding({
              ...common,
              name: "CHECKOUT",
              operations: CHECKOUT_OPERATIONS,
            }),
            ASSETS: assets,
          };
    },
    onOutbound: (m, url) => {
      outboundDenied++;
      log({ level: "warn", msg: "outbound fetch denied", artifact: m.id, host: safeHost(url) });
    },
  });

  const apiHeaders = (site: Site, extra: Record<string, string> = {}) => ({
    "x-tenant": site.tenant_id,
    "x-market": site.market_id,
    "x-locale": site.locale,
    "x-storefront-token": site.storefront_token,
    ...(site.clientIp ? { "x-client-ip": site.clientIp } : {}),
    ...extra,
  });

  // --- static assets -------------------------------------------------------------------------

  /** `/_astro/*` from the active artifact, then retained ones (content-hashed names, A22). */
  async function serveAsset(
    ids: string[],
    path: string,
    immutable: boolean,
  ): Promise<Response | null> {
    for (const id of ids) {
      const m = await manifest(id).catch(() => null);
      const buf = m && (await readAsset(opts.artifactRoot, m, path));
      if (buf) {
        return new Response(buf, {
          headers: {
            "content-type": contentType(path),
            "cache-control": immutable
              ? "public, max-age=31536000, immutable"
              : "public, max-age=300",
            "x-content-type-options": "nosniff",
            // Assets are never documents: an .html/.svg opened directly runs sandboxed (opaque
            // origin, no script), so a theme cannot ship a page that bypasses the edge CSP.
            "content-security-policy": ASSET_CSP,
          },
        });
      }
    }
    return null;
  }

  // --- worker rendering ----------------------------------------------------------------------

  async function render(
    artifactId: string,
    url: URL,
    site: Site,
    extra: {
      cartToken?: string | undefined;
      sessionToken?: string | undefined;
      consentSubject?: string | undefined;
      scope?: string;
    } = {},
  ) {
    const ctxId = registry.open(site, artifactId, {
      cartToken: extra.cartToken,
      sessionToken: extra.sessionToken,
      consentSubject: extra.consentSubject,
    });
    try {
      const headers = workerRequestHeaders();
      headers.set(CTX_HEADER, ctxId);
      // One deadline and one byte cap for headers *and* body: a worker must not hold a request
      // context (or Node memory) open with a never-ending stream.
      const abort = new AbortController();
      const { res, body } = await withTimeout(
        (async () => {
          const res = await pool.fetch(
            artifactId,
            new Request(url, { method: "GET", headers }),
            extra.scope,
          );
          return { res, body: await readCapped(res.body, MAX_RENDER_BYTES, abort.signal) };
        })(),
        opts.renderTimeoutMs ?? 10_000,
        () => abort.abort(),
      );
      const ctx = registry.get(ctxId, artifactId);
      return {
        status: res.status,
        headers: workerResponseHeaders(res.headers),
        rawHeaders: res.headers,
        body,
        pageModels: { anyPrivate: ctx?.anyPrivate ?? false, minMaxAge: ctx?.minMaxAge ?? null },
        tags: [...(ctx?.tags ?? [])],
        subrequests: ctx?.subrequests ?? 0,
      };
    } finally {
      registry.close(ctxId);
    }
  }

  function themeHeaders(m: ArtifactManifest, site: Site, port: string): Record<string, string> {
    const csp = contentSecurityPolicy("theme", {
      scriptHashes: m.csp.script_hashes,
      styleHashes: m.csp.style_hashes,
      checkoutOrigin: `${scheme}://checkout.${site.shop_host}${port}`,
    });
    return {
      ...securityHeaders("theme", csp),
      "speculation-rules": '"/_p/speculation-rules.json"',
    };
  }

  /**
   * `prefix` is the locale prefix the visitor used (`/cs`) when `url` is the unprefixed page:
   * a 404 looks up redirects as the visitor typed the URL first, then unprefixed with the
   * target kept in the visitor's locale.
   */
  async function renderTheme(
    site: Site,
    url: URL,
    req: Request,
    port: string,
    prefix = "",
  ): Promise<Response> {
    if (req.method !== "GET" && req.method !== "HEAD")
      return text(405, "Method not allowed", { allow: "GET, HEAD" });
    // A20: counted before the cache, without identifiers (template + day only).
    if (req.method === "GET") opts.counters?.page(site, templateOf(url.pathname));
    const artifact = site.theme_artifact;
    if (!artifact)
      return text(503, "This shop has not been published yet", { "retry-after": "60" });
    const m = await manifest(artifact);
    // Theme workers get the theme bindings only; a checkout artifact never runs as a theme.
    if (m.kind !== "theme") throw new Error(`artifact ${artifact} is not a theme`);
    const normalized = normalizeUrl(url);
    const verdict = requestVerdict({
      method: req.method,
      url: normalized,
      origin: "shop",
      preview: false,
      headers: req.headers,
    });
    const key = [
      site.tenant_id,
      site.market_id,
      site.locale,
      artifact,
      normalized.host,
      normalized.pathname + normalized.search,
    ].join("|");

    const respond = (
      status: number,
      headers: Headers,
      body: Uint8Array<ArrayBuffer> | null,
      cacheState: string,
      cacheable: boolean,
      subrequests?: number,
    ) => {
      const h = new Headers(headers);
      for (const [k, v] of Object.entries(themeHeaders(m, site, port))) h.set(k, v);
      // The edge holds the shared copy; browsers always revalidate so a purge takes effect at once.
      h.set("cache-control", cacheable ? "public, max-age=0, must-revalidate" : "no-store");
      h.set("x-edge-cache", cacheState);
      // Page-model fan-out of this render (read by the gates as an N+1 signal).
      if (subrequests !== undefined) h.set("x-edge-subrequests", String(subrequests));
      return new Response(req.method === "HEAD" ? null : body, { status, headers: h });
    };

    const store = async () => {
      const gen = cache.generation;
      const r = await render(artifact, normalized, site, { scope: site.tenant_id });
      const rv = responseVerdict({
        status: r.status,
        headers: r.rawHeaders,
        pageModels: r.pageModels,
      });
      const cacheable = verdict.cache && "ttl" in rv;
      // A page that stopped being cacheable must not keep being served stale.
      if (!cacheable) cache.delete(key);
      if (cacheable) {
        cache.set(
          key,
          {
            status: r.status,
            headers: [...r.headers],
            body: r.body,
            storedAt: Date.now(),
            ttlMs: rv.ttl * 1000,
            tags: r.tags,
            tenantId: site.tenant_id,
          },
          gen,
        ); // dropped if a purge happened while this render was in flight
      }
      return { r, cacheable };
    };

    if (verdict.cache) {
      const hit = cache.get(key);
      if (hit) {
        if (!hit.fresh && !revalidating.has(key)) {
          revalidating.add(key);
          store()
            .catch((err) => log({ level: "error", msg: "revalidation failed", err: String(err) }))
            .finally(() => revalidating.delete(key));
        }
        return respond(
          hit.entry.status,
          new Headers(hit.entry.headers),
          hit.entry.body,
          hit.fresh ? "HIT" : "STALE",
          true,
        );
      }
    }
    const { r, cacheable } = await store();
    if (r.status === 404) {
      const redirect =
        (prefix ? await redirectFor(site, `${prefix}${url.pathname}`) : null) ??
        (await redirectFor(site, url.pathname, prefix));
      if (redirect) return redirect;
    }
    return respond(
      r.status,
      r.headers,
      r.body,
      verdict.cache ? "MISS" : "BYPASS",
      cacheable,
      r.subrequests,
    );
  }

  /** Spec §9.5: a page the theme does not know may have a merchant-defined redirect. */
  async function redirectFor(site: Site, path: string, prefix = ""): Promise<Response | null> {
    const query = new URLSearchParams({ path });
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/redirects/resolve?${query}`, {
        headers: apiHeaders(site, { accept: "application/json" }),
      }),
    ).catch(() => null);
    if (!res?.ok) return null;
    const r = (await res.json().catch(() => null)) as { to_path?: unknown; code?: unknown } | null;
    const to = r?.to_path;
    // Same-shop paths only (the API enforces it too): never an open redirect.
    if (typeof to !== "string" || !SAME_SHOP_PATH.test(to)) return null;
    // Keep the visitor's locale unless the rule already targets a localized path.
    const localized = !prefix || to === prefix || to.startsWith(`${prefix}/`);
    return new Response(null, {
      status: r?.code === 302 ? 302 : 301,
      headers: {
        location: localized ? to : `${prefix}${to === "/" ? "" : to}`,
        "cache-control": "no-store",
      },
    });
  }

  // --- platform routes (/_p/*) ---------------------------------------------------------------

  function sameOrigin(req: Request, host: string, port: string) {
    const origin = req.headers.get("origin");
    // Local dev also serves the shops over https (Caddy tls internal) while the canonical scheme
    // stays http; production is https-only, so this only ever widens http → https.
    if (origin)
      return (
        origin === `${scheme}://${host}${port}` ||
        (scheme === "http" && origin === `https://${host}${port}`)
      );
    return req.headers.get("sec-fetch-site") === "same-origin";
  }

  async function readJsonBody(req: Request, max: number): Promise<ArrayBuffer | Response> {
    if (!(req.headers.get("content-type") ?? "").startsWith("application/json")) {
      return problem(415, "unsupported_media_type", "expected application/json");
    }
    const body = await readCapped(req.body, max).catch(() => null);
    return body ? body.buffer : problem(413, "payload_too_large", `body over ${max} bytes`);
  }

  const cartCookie = (token: string) =>
    `${SHOP_CART_COOKIE}=${token}; Path=/_p; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000`;

  const sessionCookie = (token: string) =>
    `${SESSION_COOKIE}=${token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${SESSION_MAX_AGE}`;

  /** A cookie value that must look like one of our 64-hex capabilities. */
  const capabilityCookie = (req: Request, name: string) => {
    const v = readCookie(req.headers, name);
    return v && TOKEN_RE.test(v) ? v : undefined;
  };

  /**
   * `/_p/account/*` on the checkout origin → `/storefront/v1/customer/*`. Same-origin JSON only
   * (CSRF, §14); the session, checkout cart and consent subject go to the API as headers, and
   * only the edge ever sees the session token (it comes back in `x-session-token`).
   */
  async function accountProxy(
    site: Site,
    req: Request,
    rest: string,
    host: string,
    port: string,
    clientIp: string | undefined,
  ): Promise<Response> {
    const op = ACCOUNT_OPS.find((o) => o.method === req.method && o.path.test(rest));
    if (!op) return problem(404, "not_found", "unknown account operation");
    // CSRF (§14): SameSite=Lax plus a same-origin check on every state-changing call.
    if (req.method !== "GET" && !sameOrigin(req, host, port))
      return problem(403, "cross_origin", "cross-origin request");
    let body: ArrayBuffer | undefined;
    if (req.method === "POST" || req.method === "PUT") {
      const b = await readJsonBody(req, MAX_JSON_BODY);
      if (b instanceof Response) return b;
      body = b;
    }
    const session = capabilityCookie(req, SESSION_COOKIE);
    const cart = capabilityCookie(req, CHECKOUT_CART_COOKIE);
    const subject = readCookie(req.headers, CONSENT_ID_COOKIE);
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/customer${rest}`, {
        method: req.method,
        headers: apiHeaders(site, {
          ...(body ? { "content-type": "application/json" } : {}),
          ...(session ? { "x-customer-session": session } : {}),
          ...(cart ? { "x-cart-token": cart } : {}),
          ...(subject && CONSENT_ID_RE.test(subject) ? { "x-consent-subject": subject } : {}),
          ...(clientIp ? { "x-client-ip": clientIp } : {}),
        }),
        body,
      }),
    );
    const headers = new Headers({ "cache-control": "no-store" });
    const type = res.headers.get("content-type");
    if (type) headers.set("content-type", type);
    const minted = res.headers.get("x-session-token");
    if (minted && TOKEN_RE.test(minted)) headers.append("set-cookie", sessionCookie(minted));
    else if (res.headers.get("x-session-clear") === "1" || res.status === 401)
      headers.append("set-cookie", CLEAR_SESSION_COOKIE);
    // 30 days sliding (§5.4): every successful use renews the cookie too.
    else if (res.ok && session) headers.append("set-cookie", sessionCookie(session));
    return new Response(res.status === 204 ? null : await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, headers),
    });
  }

  /**
   * `/_p/checkout/*` and `/_p/orders/*` on the checkout origin (WP10) → the Storefront API.
   * Same-origin JSON only for writes (CSRF, §14). The checkout cart capability, the session,
   * the client IP and a validated `Idempotency-Key` go along as headers; the order routes are
   * authorized by the order token in their path (A4).
   */
  async function checkoutProxy(
    site: Site,
    req: Request,
    apiPath: string,
    rest: string,
    ops: { method: string; path: RegExp }[],
    host: string,
    port: string,
    clientIp: string | undefined,
  ): Promise<Response> {
    if (!ops.some((o) => o.method === req.method && o.path.test(rest)))
      return problem(404, "not_found", "unknown checkout operation");
    if (req.method !== "GET" && !sameOrigin(req, host, port))
      return problem(403, "cross_origin", "cross-origin request");
    const key = req.headers.get("idempotency-key");
    if (key !== null && !IDEMPOTENCY_KEY_RE.test(key))
      return problem(400, "invalid_idempotency_key", "1-255 visible ASCII characters");
    let body: ArrayBuffer | undefined;
    if (req.method === "POST" || req.method === "PUT") {
      const b = await readJsonBody(req, MAX_JSON_BODY);
      if (b instanceof Response) return b;
      body = b;
    }
    const cart = capabilityCookie(req, CHECKOUT_CART_COOKIE);
    const session = capabilityCookie(req, SESSION_COOKIE);
    // A20: place-order links a consented visitor's purchase to their analytics session and
    // (WP20) to the ad platforms; the API resolves both consents from its records.
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1${apiPath}${rest}`, {
        method: req.method,
        headers: apiHeaders(site, {
          ...(body ? { "content-type": "application/json" } : {}),
          ...(cart ? { "x-cart-token": cart } : {}),
          ...(session ? { "x-customer-session": session } : {}),
          ...consentSubject(req),
          ...(clientIp ? { "x-client-ip": clientIp } : {}),
          ...(key ? { "idempotency-key": key } : {}),
        }),
        body,
      }),
    );
    const headers = new Headers({
      "content-type": res.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
      "referrer-policy": "no-referrer",
    });
    const replayed = res.headers.get("idempotent-replayed");
    if (replayed) headers.set("idempotent-replayed", replayed);
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, headers),
    });
  }

  /**
   * The fake gateway's "provider page" (`PAYMENTS_FAKE=1`, local and e2e only): Succeed/Fail
   * buttons whose outcome the API signs and processes like a provider webhook. Plain HTML,
   * no script; afterwards back to the order page (`?return=/o/<token>`, nothing else).
   */
  async function fakePay(
    site: Site,
    req: Request,
    url: URL,
    attempt: string,
    host: string,
    port: string,
  ): Promise<Response> {
    if (!UUID_RE.test(attempt)) return text(404, "Not found");
    const api = `${opts.apiOrigin}/storefront/v1/checkout/fake-pay/${attempt}`;
    // Paying needs the placing browser's cart or the customer's session (A4: the order token
    // is read-only), exactly like the order page's payment routes.
    const cart = capabilityCookie(req, CHECKOUT_CART_COOKIE);
    const session = capabilityCookie(req, SESSION_COOKIE);
    const payer = {
      ...(cart ? { "x-cart-token": cart } : {}),
      ...(session ? { "x-customer-session": session } : {}),
    };
    const ret = url.searchParams.get("return") ?? "";
    const back = ORDER_PAGE_RE.test(ret) ? ret : "/";
    if (req.method === "POST") {
      if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
      const raw = await readCapped(req.body, MAX_JSON_BODY).catch(() => null);
      if (!raw) return problem(413, "payload_too_large", `body over ${MAX_JSON_BODY} bytes`);
      const outcome = new URLSearchParams(new TextDecoder().decode(raw)).get("outcome");
      if (outcome !== "succeeded" && outcome !== "failed")
        return problem(400, "invalid_outcome", "outcome must be succeeded or failed");
      const res = await upstream(
        new Request(api, {
          method: "POST",
          headers: apiHeaders(site, { "content-type": "application/json", ...payer }),
          body: JSON.stringify({ outcome }),
        }),
      );
      await res.body?.cancel();
      if (!res.ok && res.status !== 409) return text(res.status, "Payment could not be processed");
      return new Response(null, {
        status: 303,
        headers: { location: back, "cache-control": "no-store", "referrer-policy": "no-referrer" },
      });
    }
    if (req.method !== "GET" && req.method !== "HEAD")
      return text(405, "Method not allowed", { allow: "GET, POST" });
    const res = await upstream(new Request(api, { headers: apiHeaders(site, payer) }));
    if (!res.ok) {
      await res.body?.cancel();
      return text(res.status === 404 || res.status === 403 ? res.status : 502, "Not found");
    }
    const p = (await res.json()) as {
      order_number?: unknown;
      amount?: { formatted?: unknown };
      status?: unknown;
    };
    const esc = (v: unknown) =>
      String(v ?? "").replace(
        /[&<>"']/g,
        (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] ?? c,
      );
    const action = `/_p/fake-pay/${attempt}${back === "/" ? "" : `?return=${back}`}`;
    const pending = p.status === "pending";
    const html = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex"><title>Test payment</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:3rem auto;padding:0 1rem}button{font:inherit;min-height:2.75rem;padding:0 1.25rem;margin:0 .5rem .5rem 0;border-radius:.5rem;border:1px solid #1b1f2a;cursor:pointer}.ok{background:#1b6e3a;color:#fff}:focus-visible{outline:3px solid #2b5aa8;outline-offset:2px}</style>
<main><h1>Test payment</h1><p>Fake payment gateway (local and test environments only).</p>
<p>Order <strong>${esc(p.order_number)}</strong>: <strong>${esc(p.amount?.formatted)}</strong></p>
${
  pending
    ? `<form method="post" action="${esc(action)}"><button class="ok" name="outcome" value="succeeded">Pay</button><button name="outcome" value="failed">Fail the payment</button></form>`
    : `<p role="status">This payment is ${esc(p.status)}.</p><p><a href="${esc(back)}">Back to the order</a></p>`
}</main></html>`;
    return new Response(req.method === "HEAD" ? null : html, {
      headers: {
        "content-type": "text/html; charset=utf-8",
        "cache-control": "no-store",
        // `same-origin`, not `no-referrer`: with the latter the form post carries `Origin: null`
        // and fails the CSRF check; the URL (order token) still never leaves the origin.
        "referrer-policy": "same-origin",
        "content-security-policy":
          "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        "x-content-type-options": "nosniff",
      },
    });
  }

  /**
   * `/_p/consent` on both origins (A20): the choice is recorded by the API, which mints the
   * anonymous subject on the first one. The cookies are scoped to the shop host so the
   * checkout subdomain (preferences page, sign-in linking) shares them.
   */
  async function consentProxy(
    site: Site,
    req: Request,
    host: string,
    port: string,
    clientIp: string | undefined,
    session: string | undefined,
  ): Promise<Response> {
    if (req.method !== "GET" && req.method !== "POST")
      return text(405, "Method not allowed", { allow: "GET, POST" });
    let body: ArrayBuffer | undefined;
    if (req.method === "POST") {
      if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
      const b = await readJsonBody(req, MAX_JSON_BODY);
      if (b instanceof Response) return b;
      body = b;
    }
    const subject = readCookie(req.headers, CONSENT_ID_COOKIE);
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/consent`, {
        method: req.method,
        headers: apiHeaders(site, {
          ...(body ? { "content-type": "application/json" } : {}),
          ...(subject && CONSENT_ID_RE.test(subject) ? { "x-consent-subject": subject } : {}),
          ...(session ? { "x-customer-session": session } : {}),
          ...(clientIp ? { "x-client-ip": clientIp } : {}),
        }),
        body,
      }),
    );
    const headers = new Headers({
      "content-type": res.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
    });
    const minted = res.headers.get("x-consent-subject");
    const summary = res.headers.get("x-consent-summary");
    const scope = `Domain=${site.shop_host}; Path=/; Secure; SameSite=Lax`;
    const chose = res.ok && req.method === "POST";
    if (chose && minted && CONSENT_ID_RE.test(minted))
      headers.append(
        "set-cookie",
        `${CONSENT_ID_COOKIE}=${minted}; ${scope}; Max-Age=${CONSENT_ID_MAX_AGE}; HttpOnly`,
      );
    if (chose && summary !== null && CONSENT_SUMMARY_RE.test(summary))
      headers.append(
        "set-cookie",
        `${CONSENT_COOKIE}=${encodeURIComponent(summary)}; ${scope}; Max-Age=${CONSENT_MAX_AGE}`,
      );
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, headers),
    });
  }

  async function cartProxy(
    site: Site,
    req: Request,
    rest: string,
    host: string,
    port: string,
  ): Promise<Response> {
    const op = CART_OPS.find((o) => o.method === req.method && o.path.test(rest));
    if (!op) return problem(404, "not_found", "unknown cart operation");
    if (req.method !== "GET" && !sameOrigin(req, host, port))
      return problem(403, "cross_origin", "cross-origin request");

    // Idempotency-Key (§8.1): the API runs a keyed cart mutation once and replays it after. A
    // malformed key is refused, never silently dropped (a retry would then apply twice).
    const key = req.headers.get("idempotency-key");
    if (key !== null && !IDEMPOTENCY_KEY_RE.test(key))
      return problem(400, "invalid_idempotency_key", "1-255 visible ASCII characters");
    let body: ArrayBuffer | undefined;
    if (req.method === "POST" || req.method === "PATCH") {
      const b = await readJsonBody(req, MAX_JSON_BODY);
      if (b instanceof Response) return b;
      body = b;
    }
    let token = readCookie(req.headers, SHOP_CART_COOKIE);
    if (token && !TOKEN_RE.test(token)) token = undefined;
    let setCookie: string | undefined;

    if (!token) {
      if (req.method === "GET")
        return Response.json(EMPTY_CART, { headers: { "cache-control": "no-store" } });
      // First write creates the cart; the API mints the 256-bit capability and stores its hash.
      const created = await upstream(
        new Request(`${opts.apiOrigin}/storefront/v1/cart`, {
          method: "POST",
          headers: apiHeaders(site),
        }),
      );
      const minted = created.headers.get("x-cart-token");
      if (!created.ok || !minted || !TOKEN_RE.test(minted))
        return problem(502, "cart_unavailable", "could not create a cart");
      token = minted;
      setCookie = cartCookie(token);
    }

    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/cart${rest}`, {
        method: req.method,
        headers: apiHeaders(site, {
          "x-cart-token": token,
          ...(body ? { "content-type": "application/json" } : {}),
          ...(key ? { "idempotency-key": key } : {}),
          // A20: the API records add-to-cart for a consented visitor (server-resolved).
          ...consentSubject(req),
        }),
        body,
      }),
    );
    const headers = new Headers({
      "content-type": res.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
    });
    const replayed = res.headers.get("idempotent-replayed");
    if (replayed) headers.set("idempotent-replayed", replayed);
    if (setCookie) headers.append("set-cookie", setCookie);
    // The API answers 404 only for a cart it does not know (line/coupon errors are 422).
    if (res.status === 404 && !setCookie) {
      // Unknown, expired or rotated (handed off) cart: forget the capability.
      headers.append("set-cookie", CLEAR_CART_COOKIE);
      if (req.method === "GET") return Response.json(EMPTY_CART, { headers });
    } else if (res.ok && !setCookie) {
      // Expiry counts from the last use (§10.3): every successful use renews the cookie.
      headers.append("set-cookie", cartCookie(token));
    }
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, headers),
    });
  }

  async function checkoutStart(
    site: Site,
    req: Request,
    host: string,
    port: string,
  ): Promise<Response> {
    if (req.method !== "POST") return text(405, "Method not allowed", { allow: "POST" });
    if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
    const token = readCookie(req.headers, SHOP_CART_COOKIE);
    if (!token || !TOKEN_RE.test(token))
      return new Response(null, {
        status: 303,
        headers: { location: "/", "cache-control": "no-store" },
      });
    // The API revokes the shop capability and mints a single-use, 60 s handoff token (A1, A4).
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/cart/handoff`, {
        method: "POST",
        headers: apiHeaders(site, { "x-cart-token": token, ...consentSubject(req) }),
      }),
    );
    const h = res.ok ? ((await res.json()) as { token?: unknown }).token : undefined;
    if (typeof h !== "string" || !TOKEN_RE.test(h)) {
      return new Response(null, {
        status: 303,
        headers: { location: "/", "cache-control": "no-store" },
      });
    }
    const checkoutHost = `checkout.${site.shop_host}`;
    return new Response(null, {
      status: 303,
      headers: {
        location: `${scheme}://${checkoutHost}${port}/start?h=${h}`,
        "cache-control": "no-store",
        "referrer-policy": "no-referrer",
        // The API rotated the capability (A4); the shop-side token is dead now.
        "set-cookie": CLEAR_CART_COOKIE,
      },
    });
  }

  async function publicProxy(site: Site, req: Request, url: URL, rest: string): Promise<Response> {
    if (req.method !== "GET") return text(405, "Method not allowed", { allow: "GET" });
    if (!STOREFRONT_OPERATIONS.some((o) => o.path.test(rest)))
      return problem(404, "not_found", "unknown public operation");
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1${rest}${url.search}`, {
        headers: apiHeaders(site, { accept: "application/json" }),
      }),
    );
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, {
        "content-type": res.headers.get("content-type") ?? "application/json",
        "cache-control": "no-store",
      }),
    });
  }

  /**
   * `/_p/recommendations` (WP17, shop origin): the private variant of `/recommendations`. Only
   * here does the API get the visitor's cart capability (cross-sell for the cart) and consent
   * subject (A20: personalization and recently viewed only while its records grant
   * `personalization`); SSR and `/_p/public/*` never carry either, so they stay public. Never
   * cached (A2). Unprefixed only, like the cart (its cookie is `Path=/_p`): `?locale=` picks
   * one of the market's locales instead of a path prefix.
   */
  async function recommendationsProxy(site: Site, req: Request, url: URL): Promise<Response> {
    if (req.method !== "GET") return text(405, "Method not allowed", { allow: "GET" });
    if (url.search.length > MAX_RECOMMENDATIONS_QUERY)
      return problem(414, "uri_too_long", "recommendations query too long");
    const locale = url.searchParams.get("locale");
    const localized = locale && site.locales.includes(locale) ? { ...site, locale } : site;
    const token = readCookie(req.headers, SHOP_CART_COOKIE);
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/recommendations${url.search}`, {
        headers: apiHeaders(localized, {
          accept: "application/json",
          ...(token && TOKEN_RE.test(token) ? { "x-cart-token": token } : {}),
          ...consentSubject(req),
        }),
      }),
    );
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, {
        "content-type": res.headers.get("content-type") ?? "application/json",
        "cache-control": "private, no-store",
      }),
    });
  }

  async function events(site: Site, req: Request, host: string, port: string): Promise<Response> {
    if (req.method !== "POST") return text(405, "Method not allowed", { allow: "POST" });
    if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
    // sendBeacon posts text/plain; accept both and let the API validate the payload.
    const capped = await readCapped(req.body, MAX_EVENTS_BODY).catch(() => null);
    if (!capped) return problem(413, "payload_too_large", "events batch too large");
    const body = capped.buffer;
    // A20: the API stores events only if this subject's consent records grant `analytics`,
    // and forwards them to ad platforms only if they grant `ads` (WP20).
    await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/events`, {
        method: "POST",
        headers: apiHeaders(site, {
          "content-type": "application/json",
          ...consentSubject(req),
        }),
        body,
      }),
    )
      .then((res) => res.body?.cancel())
      .catch((err) => log({ level: "warn", msg: "events forward failed", err: String(err) }));
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  }

  /** Newsletter sign-up: same-origin JSON only; double opt-in is the API's job (§11.5). */
  async function newsletter(
    site: Site,
    req: Request,
    host: string,
    port: string,
  ): Promise<Response> {
    if (req.method !== "POST") return text(405, "Method not allowed", { allow: "POST" });
    if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
    // A plain HTML form (works without JS): subscribe, then 303 back to the page it came from
    // with `?newsletter=ok|invalid#newsletter` for the theme to render the outcome.
    if ((req.headers.get("content-type") ?? "").startsWith("application/x-www-form-urlencoded")) {
      const raw = await readCapped(req.body, MAX_JSON_BODY).catch(() => null);
      if (!raw) return problem(413, "payload_too_large", `body over ${MAX_JSON_BODY} bytes`);
      const email = new URLSearchParams(new TextDecoder().decode(raw)).get("email") ?? "";
      const res = await upstream(
        new Request(`${opts.apiOrigin}/storefront/v1/newsletter/subscribe`, {
          method: "POST",
          headers: apiHeaders(site, { "content-type": "application/json" }),
          body: JSON.stringify({ email }),
        }),
      );
      await res.body?.cancel();
      let back = new URL("/", `${scheme}://${host}${port}`);
      const referer = URL.parse(req.headers.get("referer") ?? "");
      // Same shop only (never an open redirect); the query keeps its other parameters.
      if (referer && referer.host === `${host}${port}`) back = referer;
      back.searchParams.set("newsletter", res.ok ? "ok" : "invalid");
      // `//evil.example/x` is a same-host path but a network-path reference as a Location.
      const path = /^\/(?![/\\])/.test(back.pathname) ? back.pathname : "/";
      return new Response(null, {
        status: 303,
        headers: {
          location: `${path}${back.search}#newsletter`,
          "cache-control": "no-store",
        },
      });
    }
    const body = await readJsonBody(req, MAX_JSON_BODY);
    if (body instanceof Response) return body;
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/newsletter/subscribe`, {
        method: "POST",
        headers: apiHeaders(site, { "content-type": "application/json" }),
        body,
      }),
    );
    return new Response(await res.arrayBuffer(), {
      status: res.status,
      headers: withRetryAfter(res, {
        "content-type": res.headers.get("content-type") ?? "application/json",
        "cache-control": "no-store",
      }),
    });
  }

  // --- origins -------------------------------------------------------------------------------

  async function shop(
    site: Site,
    req: Request,
    url: URL,
    host: string,
    port: string,
    clientIp: string | undefined,
  ): Promise<Response> {
    const p = url.pathname;
    if (p === "/_p/consent") return consentProxy(site, req, host, port, clientIp, undefined);
    // `/<locale>/…` of a non-default market locale (spec §9.1): theme pages and island reads
    // render in that locale from the unprefixed path. The locale is part of the cache key; the
    // cart keeps its unprefixed routes (cookie `Path=/_p`).
    const split = splitLocale(site, p);
    if (split) {
      const localized = { ...site, locale: split.locale };
      const inner = new URL(`${split.path}${url.search}`, url);
      if (split.path.startsWith("/_p/public/"))
        return publicProxy(localized, req, inner, split.path.slice("/_p/public".length));
      if (!split.path.startsWith("/_") && !split.path.startsWith("/media/"))
        return renderTheme(localized, inner, req, port, `/${split.locale}`);
      return text(404, "Not found");
    }
    if (p === "/_p/cart" || p.startsWith("/_p/cart/"))
      return cartProxy(site, req, p.slice("/_p/cart".length), host, port);
    if (p === "/_p/checkout/start") return checkoutStart(site, req, host, port);
    if (p.startsWith("/_p/public/"))
      return publicProxy(site, req, url, p.slice("/_p/public".length));
    if (p === "/_p/recommendations") return recommendationsProxy(site, req, url);
    if (p === "/_p/e") return events(site, req, host, port);
    if (p === "/_p/newsletter") return newsletter(site, req, host, port);
    if (p === "/_p/speculation-rules.json") {
      return new Response(SPECULATION_RULES, {
        headers: {
          "content-type": "application/speculationrules+json",
          "cache-control": "public, max-age=3600",
        },
      });
    }
    if (p.startsWith("/_p/") || p.startsWith("/_edge/"))
      return problem(404, "not_found", "unknown platform route");
    if (req.method !== "GET" && req.method !== "HEAD")
      return text(405, "Method not allowed", { allow: "GET, HEAD" });
    if (p.startsWith("/media/")) {
      // The public bucket is shared; a shop serves only its own tenant's images.
      if (!p.startsWith(`/media/${site.tenant_id}/`) || p.includes(".."))
        return text(404, "Not found");
      const res = await upstream(new Request(`${opts.mediaOrigin}${p}`));
      const headers = new Headers();
      for (const k of ["content-type", "cache-control", "etag", "last-modified"]) {
        const v = res.headers.get(k);
        if (v) headers.set(k, v);
      }
      headers.set("x-content-type-options", "nosniff");
      return new Response(res.body, { status: res.status, headers });
    }
    if (/^\/(robots\.txt|llms\.txt|sitemap[a-z0-9_-]*\.xml|feeds\/.+)$/.test(p)) {
      const res = await upstream(
        new Request(`${opts.apiOrigin}/storefront/v1/files${p}`, { headers: apiHeaders(site) }),
      );
      return new Response(res.body, {
        status: res.status,
        headers: {
          "content-type": res.headers.get("content-type") ?? "text/plain",
          "cache-control": "public, max-age=300",
        },
      });
    }
    const active = site.theme_artifact ? [site.theme_artifact] : [];
    if (p.startsWith("/_astro/")) {
      return (
        (await serveAsset([...active, ...site.retained_artifacts], p, true)) ??
        text(404, "Not found")
      );
    }
    const pub = await serveAsset(active, p, false);
    if (pub) return pub;
    return renderTheme(site, url, req, port);
  }

  async function checkout(
    site: Site,
    req: Request,
    url: URL,
    host: string,
    port: string,
    clientIp: string | undefined,
  ): Promise<Response> {
    const p = url.pathname;
    if (p.startsWith("/_p/account/"))
      return accountProxy(site, req, p.slice("/_p/account".length), host, port, clientIp);
    if (p === "/_p/withdraw" || p.startsWith("/_p/withdraw/")) {
      const rest = p.slice("/_p/withdraw".length);
      return checkoutProxy(site, req, "/withdrawals", rest, WITHDRAW_OPS, host, port, clientIp);
    }
    if (p === "/_p/checkout" || p.startsWith("/_p/checkout/")) {
      const rest = p.slice("/_p/checkout".length);
      return checkoutProxy(site, req, "/checkout", rest, CHECKOUT_OPS, host, port, clientIp);
    }
    if (p.startsWith("/_p/orders/")) {
      const rest = p.slice("/_p/orders".length);
      return checkoutProxy(site, req, "/orders", rest, ORDER_OPS, host, port, clientIp);
    }
    if (p.startsWith("/_p/fake-pay/"))
      return fakePay(site, req, url, p.slice("/_p/fake-pay/".length), host, port);
    if (p === "/_p/consent")
      return consentProxy(site, req, host, port, clientIp, capabilityCookie(req, SESSION_COOKIE));
    if (p === "/start") {
      const h = url.searchParams.get("h") ?? "";
      // Only the shop's own 303 (a same-site navigation) may redeem a handoff. A link planted by
      // another site (cross-site) or pasted/opened from mail (none) is refused, so an attacker
      // cannot push their cart into a victim's checkout.
      const fetchSite = req.headers.get("sec-fetch-site");
      const redeemable =
        req.method === "GET" && (fetchSite === "same-site" || fetchSite === "same-origin");
      let cartToken: string | undefined;
      if (redeemable && TOKEN_RE.test(h)) {
        // Bound to this tenant and market by the API: another shop's checkout cannot redeem it.
        const res = await upstream(
          new Request(`${opts.apiOrigin}/storefront/v1/checkout/handoff`, {
            method: "POST",
            headers: apiHeaders(site, { "content-type": "application/json" }),
            body: JSON.stringify({ token: h }),
          }),
        );
        const got = res.ok ? ((await res.json()) as { cart_token?: unknown }).cart_token : null;
        if (typeof got === "string" && TOKEN_RE.test(got)) cartToken = got;
      }
      if (!cartToken) {
        return new Response(
          `<!doctype html><meta charset="utf-8"><title>Odkaz vypršel</title><p>Odkaz na pokladnu vypršel nebo už byl použit. <a href="${scheme}://${site.shop_host}${port}/">Zpět do obchodu</a></p>`,
          {
            status: 400,
            headers: {
              "content-type": "text/html; charset=utf-8",
              "cache-control": "no-store",
              "referrer-policy": "no-referrer",
            },
          },
        );
      }
      return new Response(null, {
        status: 303,
        headers: {
          location: "/",
          "set-cookie": `${CHECKOUT_CART_COOKIE}=${cartToken}; Path=/; HttpOnly; Secure; SameSite=Lax`,
          "cache-control": "no-store",
          "referrer-policy": "no-referrer",
        },
      });
    }
    if (p === "/_p/tokens.css") {
      if (!site.theme_artifact) return text(404, "Not found");
      const m = await manifest(site.theme_artifact);
      return new Response(m.tokens ? tokensToCss(m.tokens) : "", {
        headers: {
          "content-type": "text/css; charset=utf-8",
          "cache-control": "public, max-age=300",
        },
      });
    }
    if (p.startsWith("/_p/") || p.startsWith("/_edge/"))
      return problem(404, "not_found", "unknown platform route");
    if (req.method !== "GET" && req.method !== "HEAD")
      return text(405, "Method not allowed", { allow: "GET, HEAD" });
    const checkoutArtifact = site.checkout_artifact ?? opts.checkoutArtifact;
    if (!checkoutArtifact)
      return text(503, "Checkout is not available yet", { "retry-after": "60" });
    const asset = await serveAsset([checkoutArtifact], p, p.startsWith("/_astro/"));
    if (asset) return asset;

    const m = await manifest(checkoutArtifact);
    if (req.method === "GET") opts.counters?.page(site, "checkout");
    // The checkout binding (cart capability, session) is only ever handed to the platform
    // checkout; the worker itself never sees the cookies, only the edge's context id.
    if (m.kind !== "checkout") throw new Error(`artifact ${checkoutArtifact} is not a checkout`);
    const consentSubject = readCookie(req.headers, CONSENT_ID_COOKIE);
    const r = await render(checkoutArtifact, normalizeUrl(url), site, {
      cartToken: capabilityCookie(req, CHECKOUT_CART_COOKIE),
      sessionToken: capabilityCookie(req, SESSION_COOKIE),
      consentSubject:
        consentSubject && CONSENT_ID_RE.test(consentSubject) ? consentSubject : undefined,
    });
    const headers = r.headers;
    const csp = contentSecurityPolicy("checkout", {
      scriptHashes: m.csp.script_hashes,
      styleHashes: m.csp.style_hashes,
      widgetOrigin,
      // Stripe.js only where a payment can happen: the checkout and the order page.
      stripe: url.pathname === "/" || ORDER_PAGE_RE.test(url.pathname),
    });
    for (const [k, v] of Object.entries(securityHeaders("checkout", csp))) headers.set(k, v);
    headers.set("cache-control", "no-store");
    // A sign-in link (`/account/verify?token=`) or an order page (`/o/<token>`, A4) must not
    // leak its capability through Referer.
    if (
      url.searchParams.has("token") ||
      url.searchParams.has("t") ||
      url.pathname === "/withdraw" ||
      url.pathname.startsWith("/o/")
    )
      headers.set("referrer-policy", "no-referrer");
    return new Response(req.method === "HEAD" ? null : r.body, { status: r.status, headers });
  }

  async function handle(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const rawHost = request.headers.get("host") ?? url.host;
    const host = normalizeHost(rawHost);
    if (!host) return text(400, "Bad host");
    const port = /:\d{1,5}$/.exec(rawHost)?.[0] ?? "";
    // Caddy appends the peer address to X-Forwarded-For; only that last entry is trustworthy.
    // It feeds rate limits and salted hashes (never stored raw) and is stripped with the rest.
    const lastHop = request.headers.get("x-forwarded-for")?.split(",").at(-1)?.trim();
    const clientIp = lastHop && IP_RE.test(lastHop) ? lastHop : undefined;
    const req = new Request(request, { headers: stripUntrusted(request.headers) });
    // Canonical URL the worker sees: public scheme + the validated host.
    const publicUrl = new URL(`${url.pathname}${url.search}`, `${scheme}://${host}${port}`);

    if (host.startsWith("preview-")) return text(404, "Previews are not available yet"); // M3 (WP23)
    const origin = classifyHost(host);
    const resolved = await resolver.resolve(origin.shopHost);
    if (!resolved) return text(404, "Unknown shop");
    // A per-request copy: the resolver's cached object is shared between requests.
    const site: Site = { ...resolved, clientIp };
    return origin.kind === "shop"
      ? shop(site, req, publicUrl, host, port, clientIp)
      : checkout(site, req, publicUrl, host, port, clientIp);
  }

  async function fetchHandler(request: Request): Promise<Response> {
    let response: Response;
    try {
      response = await handle(request);
    } catch (err) {
      log({ level: "error", msg: "request failed", path: safePath(request.url), err: String(err) });
      response = text(err instanceof TimeoutError ? 504 : 502, "Shop temporarily unavailable");
    }
    // Even rejected requests and render failures must not leak a withdrawal capability.
    const path = new URL(request.url).pathname;
    if (path === "/withdraw" || path === "/_p/withdraw" || path.startsWith("/_p/withdraw/")) {
      response.headers.set("referrer-policy", "no-referrer");
      response.headers.set("cache-control", "no-store");
    }
    return response;
  }

  /** Internal admin surface (separate port, never routed by Caddy). */
  async function admin(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/_edge/healthz")
      return Response.json({ status: "ok", workers: pool.size });
    if (url.pathname !== "/_edge/purge" || request.method !== "POST") return text(404, "Not found");
    if (!bearerMatches(request.headers.get("authorization"), opts.purgeToken))
      return text(401, "Unauthorized");
    const body = (await request.json().catch(() => null)) as {
      tenant_id?: unknown;
      tags?: unknown;
      all?: unknown;
    } | null;
    const tenantId = typeof body?.tenant_id === "string" ? body.tenant_id : undefined;
    const tags = Array.isArray(body?.tags)
      ? body.tags.filter((t): t is string => typeof t === "string")
      : undefined;
    const all = body?.all === true;
    if (!tenantId && !tags && !all)
      return problem(400, "bad_request", "tenant_id, tags or all required");
    resolver.purge(all ? undefined : tenantId);
    const purged = cache.purge({ tenantId, tags, all });
    log({ level: "info", msg: "purge", tenant: tenantId, tags: tags?.length ?? 0, all, purged });
    return Response.json({ purged });
  }

  return {
    fetch: fetchHandler,
    admin,
    pool,
    cache,
    registry,
    get outboundDenied() {
      return outboundDenied;
    },
    async dispose() {
      await pool.dispose();
    },
  };
}

export type Gateway = ReturnType<typeof createGateway>;

function bearerMatches(header: string | null, expected: string) {
  const got = Buffer.from(header?.replace(/^Bearer /, "") ?? "");
  const want = Buffer.from(expected);
  return expected.length >= 16 && got.length === want.length && timingSafeEqual(got, want);
}

class TimeoutError extends Error {}

const MAX_RENDER_BYTES = 5 * 1024 * 1024;

const ASSET_CSP =
  "default-src 'none'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; font-src 'self'; sandbox";

/** Reads a body stream, cancelling it as soon as it exceeds `max` bytes (then rejects). */
async function readCapped(
  stream: ReadableStream<Uint8Array> | null,
  max: number,
  signal?: AbortSignal,
): Promise<Uint8Array<ArrayBuffer>> {
  const out = new Uint8Array(new ArrayBuffer(0));
  if (!stream) return out;
  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = stream.getReader();
  const cancel = () => void reader.cancel().catch(() => {});
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > max) {
        await reader.cancel();
        throw new Error(`body over ${max} bytes`);
      }
      chunks.push(value);
    }
  } finally {
    signal?.removeEventListener("abort", cancel);
    reader.releaseLock();
  }
  const body = new Uint8Array(new ArrayBuffer(total));
  let offset = 0;
  for (const c of chunks) {
    body.set(c, offset);
    offset += c.byteLength;
  }
  return body;
}

function withTimeout<T>(p: Promise<T>, ms: number, onTimeout?: () => void): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  return Promise.race([
    p,
    new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        onTimeout?.();
        reject(new TimeoutError(`timed out after ${ms} ms`));
      }, ms);
    }),
  ]).finally(() => clearTimeout(timer));
}

/** The anonymous consent subject from its cookie, as an API header (A20). */
/**
 * The consent subject (A20) and, with it, the browser's user agent: Meta requires it on
 * website events, and the API keeps it only while an `ads`-consented delivery is open (WP20).
 * Nothing without a consent cookie.
 */
function consentSubject(req: Request): Record<string, string> {
  const subject = readCookie(req.headers, CONSENT_ID_COOKIE);
  if (!subject || !CONSENT_ID_RE.test(subject)) return {};
  const ua = (req.headers.get("user-agent") ?? "").replace(/[^\x20-\x7e]/g, "").slice(0, 512);
  return { "x-consent-subject": subject, ...(ua ? { "x-client-user-agent": ua } : {}) };
}

/** API answers keep a rate limit's `Retry-After` (spec §8.1) when proxied. */
function withRetryAfter(res: Response, init: HeadersInit): Headers {
  const headers = new Headers(init);
  const retry = res.headers.get("retry-after");
  if (res.status === 429 && retry && /^\d{1,5}$/.test(retry)) headers.set("retry-after", retry);
  return headers;
}

function safeHost(url: string) {
  try {
    return new URL(url).host;
  } catch {
    return "invalid";
  }
}

function safePath(url: string) {
  try {
    return new URL(url).pathname.replace(/^\/_p\/withdraw\/[^/]+/, "/_p/withdraw/[redacted]");
  } catch {
    return "invalid";
  }
}
