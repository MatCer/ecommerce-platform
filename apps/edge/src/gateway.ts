import { timingSafeEqual } from "node:crypto";
import { type ArtifactManifest, readManifest, tokensToCss } from "@platform/theme-kit";
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
import { HandoffStore } from "./handoff.ts";
import {
  contentSecurityPolicy,
  securityHeaders,
  stripUntrusted,
  themeRequestHeaders,
  workerResponseHeaders,
} from "./headers.ts";
import { type NodeBinding, WorkerPool } from "./runtime.ts";
import {
  CachedResolver,
  classifyHost,
  normalizeHost,
  type Site,
  type SiteResolver,
} from "./sites.ts";

export interface GatewayOptions {
  artifactRoot: string;
  resolver: SiteResolver;
  /** The one platform-owned checkout artifact (same bundle for all tenants, spec §9.4). */
  checkoutArtifact: string;
  /** Storefront API origin, e.g. `http://api:8000` (the stub lives in apps/mocks until WP6). */
  apiOrigin: string;
  /** Public media origin (image variants), proxied under `/media/*`. */
  mediaOrigin: string;
  /** Public scheme of storefront URLs. Never taken from client headers. */
  scheme?: "http" | "https";
  /** Bearer token for `/_edge/purge` (distinct service token, spec A7). */
  purgeToken: string;
  upstream?: Upstream;
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

const MAX_JSON_BODY = 16 * 1024;
const MAX_EVENTS_BODY = 64 * 1024;
const SHOP_CART_COOKIE = "cart";
const CHECKOUT_CART_COOKIE = "__Host-cart";

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

export function createGateway(opts: GatewayOptions) {
  const scheme = opts.scheme ?? "https";
  const upstream: Upstream = opts.upstream ?? ((r) => fetch(r));
  const log = opts.log ?? ((e) => console.log(JSON.stringify(e)));
  const resolver =
    opts.resolver instanceof CachedResolver ? opts.resolver : new CachedResolver(opts.resolver);
  const registry = new ContextRegistry();
  const cache = new HtmlCache();
  const handoffs = new HandoffStore();
  const manifests = new Map<string, Promise<ArtifactManifest>>();
  const revalidating = new Set<string>();
  let outboundDenied = 0;

  const manifest = (id: string) => {
    let m = manifests.get(id);
    if (!m) {
      m = readManifest(opts.artifactRoot, id);
      manifests.set(id, m);
      m.catch(() => manifests.delete(id));
    }
    return m;
  };

  const pool = new WorkerPool({
    artifactRoot: opts.artifactRoot,
    bindings: (m): Record<string, NodeBinding> => {
      const common = { registry, artifactId: m.id, apiOrigin: opts.apiOrigin, upstream };
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
    incoming: Headers,
    site: Site,
    extra: { cartToken?: string } = {},
  ) {
    const ctxId = registry.open(site, artifactId, extra);
    try {
      const headers = themeRequestHeaders(incoming);
      headers.set(CTX_HEADER, ctxId);
      const res = await withTimeout(
        pool.fetch(artifactId, new Request(url, { method: "GET", headers })),
        opts.renderTimeoutMs ?? 10_000,
      );
      const body = new Uint8Array(await res.arrayBuffer());
      const ctx = registry.get(ctxId, artifactId);
      return {
        status: res.status,
        headers: workerResponseHeaders(res.headers),
        rawHeaders: res.headers,
        body,
        pageModels: { anyPrivate: ctx?.anyPrivate ?? false, minMaxAge: ctx?.minMaxAge ?? null },
        tags: [...(ctx?.tags ?? [])],
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

  async function renderTheme(site: Site, url: URL, req: Request, port: string): Promise<Response> {
    if (req.method !== "GET" && req.method !== "HEAD")
      return text(405, "Method not allowed", { allow: "GET, HEAD" });
    const artifact = site.theme_artifact;
    const m = await manifest(artifact);
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
    ) => {
      const h = new Headers(headers);
      for (const [k, v] of Object.entries(themeHeaders(m, site, port))) h.set(k, v);
      // The edge holds the shared copy; browsers always revalidate so a purge takes effect at once.
      h.set("cache-control", cacheable ? "public, max-age=0, must-revalidate" : "no-store");
      h.set("x-edge-cache", cacheState);
      return new Response(req.method === "HEAD" ? null : body, { status, headers: h });
    };

    const store = async () => {
      const r = await render(artifact, normalized, req.headers, site);
      const rv = responseVerdict({
        status: r.status,
        headers: r.rawHeaders,
        pageModels: r.pageModels,
      });
      const cacheable = verdict.cache && "ttl" in rv;
      if (cacheable) {
        cache.set(key, {
          status: r.status,
          headers: [...r.headers],
          body: r.body,
          storedAt: Date.now(),
          ttlMs: rv.ttl * 1000,
          tags: r.tags,
          tenantId: site.tenant_id,
        });
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
    return respond(r.status, r.headers, r.body, verdict.cache ? "MISS" : "BYPASS", cacheable);
  }

  // --- platform routes (/_p/*) ---------------------------------------------------------------

  function sameOrigin(req: Request, host: string, port: string) {
    const origin = req.headers.get("origin");
    if (origin) return origin === `${scheme}://${host}${port}`;
    return req.headers.get("sec-fetch-site") === "same-origin";
  }

  async function readJsonBody(req: Request, max: number): Promise<ArrayBuffer | Response> {
    if (!(req.headers.get("content-type") ?? "").startsWith("application/json")) {
      return problem(415, "unsupported_media_type", "expected application/json");
    }
    const body = await req.arrayBuffer();
    return body.byteLength > max
      ? problem(413, "payload_too_large", `body over ${max} bytes`)
      : body;
  }

  const cartCookie = (token: string) =>
    `${SHOP_CART_COOKIE}=${token}; Path=/_p; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000`;

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
        return Response.json(
          { id: null, lines: [], item_count: 0 },
          { headers: { "cache-control": "no-store" } },
        );
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
        }),
        body,
      }),
    );
    const headers = new Headers({
      "content-type": res.headers.get("content-type") ?? "application/json",
      "cache-control": "no-store",
    });
    if (setCookie) headers.append("set-cookie", setCookie);
    if (res.status === 404 && !setCookie) {
      // Unknown/expired cart: forget the capability.
      headers.append(
        "set-cookie",
        `${SHOP_CART_COOKIE}=; Path=/_p; HttpOnly; Secure; SameSite=Lax; Max-Age=0`,
      );
    }
    return new Response(await res.arrayBuffer(), { status: res.status, headers });
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
    const res = await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/cart/checkout-token`, {
        method: "POST",
        headers: apiHeaders(site, { "x-cart-token": token }),
      }),
    );
    const checkoutToken = res.ok ? ((await res.json()) as { token?: unknown }).token : undefined;
    if (typeof checkoutToken !== "string" || !TOKEN_RE.test(checkoutToken)) {
      return new Response(null, {
        status: 303,
        headers: { location: "/", "cache-control": "no-store" },
      });
    }
    const checkoutHost = `checkout.${site.shop_host}`;
    const h = handoffs.mint({ checkoutHost, tenantId: site.tenant_id, cartToken: checkoutToken });
    return new Response(null, {
      status: 303,
      headers: {
        location: `${scheme}://${checkoutHost}${port}/start?h=${h}`,
        "cache-control": "no-store",
        "referrer-policy": "no-referrer",
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
      headers: {
        "content-type": res.headers.get("content-type") ?? "application/json",
        "cache-control": "no-store",
      },
    });
  }

  async function events(site: Site, req: Request, host: string, port: string): Promise<Response> {
    if (req.method !== "POST") return text(405, "Method not allowed", { allow: "POST" });
    if (!sameOrigin(req, host, port)) return problem(403, "cross_origin", "cross-origin request");
    // sendBeacon posts text/plain; accept both and let the API validate the payload.
    const body = await req.arrayBuffer();
    if (body.byteLength > MAX_EVENTS_BODY)
      return problem(413, "payload_too_large", "events batch too large");
    await upstream(
      new Request(`${opts.apiOrigin}/storefront/v1/events`, {
        method: "POST",
        headers: apiHeaders(site, { "content-type": "application/json" }),
        body,
      }),
    ).catch((err) => log({ level: "warn", msg: "events forward failed", err: String(err) }));
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  }

  // --- origins -------------------------------------------------------------------------------

  async function shop(
    site: Site,
    req: Request,
    url: URL,
    host: string,
    port: string,
  ): Promise<Response> {
    const p = url.pathname;
    if (p === "/_p/cart" || p.startsWith("/_p/cart/"))
      return cartProxy(site, req, p.slice("/_p/cart".length), host, port);
    if (p === "/_p/checkout/start") return checkoutStart(site, req, host, port);
    if (p.startsWith("/_p/public/"))
      return publicProxy(site, req, url, p.slice("/_p/public".length));
    if (p === "/_p/e") return events(site, req, host, port);
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
    if (p.startsWith("/_astro/")) {
      return (
        (await serveAsset([site.theme_artifact, ...site.retained_artifacts], p, true)) ??
        text(404, "Not found")
      );
    }
    const pub = await serveAsset([site.theme_artifact], p, false);
    if (pub) return pub;
    return renderTheme(site, url, req, port);
  }

  async function checkout(
    site: Site,
    req: Request,
    url: URL,
    host: string,
    port: string,
  ): Promise<Response> {
    const p = url.pathname;
    if (p === "/start") {
      const h = url.searchParams.get("h") ?? "";
      const handoff = req.method === "GET" ? handoffs.consume(h, host) : null;
      if (!handoff || handoff.tenantId !== site.tenant_id) {
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
          "set-cookie": `${CHECKOUT_CART_COOKIE}=${handoff.cartToken}; Path=/; HttpOnly; Secure; SameSite=Lax`,
          "cache-control": "no-store",
          "referrer-policy": "no-referrer",
        },
      });
    }
    if (p === "/_p/tokens.css") {
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
    const asset = await serveAsset([opts.checkoutArtifact], p, p.startsWith("/_astro/"));
    if (asset) return asset;

    let cartToken = readCookie(req.headers, CHECKOUT_CART_COOKIE);
    if (cartToken && !TOKEN_RE.test(cartToken)) cartToken = undefined;
    const m = await manifest(opts.checkoutArtifact);
    const r = await render(opts.checkoutArtifact, normalizeUrl(url), req.headers, site, {
      cartToken,
    });
    const headers = r.headers;
    const csp = contentSecurityPolicy("checkout", {
      scriptHashes: m.csp.script_hashes,
      styleHashes: m.csp.style_hashes,
    });
    for (const [k, v] of Object.entries(securityHeaders("checkout", csp))) headers.set(k, v);
    headers.set("cache-control", "no-store");
    return new Response(req.method === "HEAD" ? null : r.body, { status: r.status, headers });
  }

  async function handle(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const rawHost = request.headers.get("host") ?? url.host;
    const host = normalizeHost(rawHost);
    if (!host) return text(400, "Bad host");
    const port = /:\d{1,5}$/.exec(rawHost)?.[0] ?? "";
    const req = new Request(request, { headers: stripUntrusted(request.headers) });
    // Canonical URL the worker sees: public scheme + the validated host.
    const publicUrl = new URL(`${url.pathname}${url.search}`, `${scheme}://${host}${port}`);

    if (host.startsWith("preview-")) return text(404, "Previews are not available yet"); // M3 (WP23)
    const origin = classifyHost(host);
    const site = await resolver.resolve(origin.shopHost);
    if (!site) return text(404, "Unknown shop");
    return origin.kind === "shop"
      ? shop(site, req, publicUrl, host, port)
      : checkout(site, req, publicUrl, host, port);
  }

  async function fetchHandler(request: Request): Promise<Response> {
    try {
      return await handle(request);
    } catch (err) {
      log({ level: "error", msg: "request failed", path: safePath(request.url), err: String(err) });
      return text(err instanceof TimeoutError ? 504 : 502, "Shop temporarily unavailable");
    }
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

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  return Promise.race([
    p,
    new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new TimeoutError(`timed out after ${ms} ms`)), ms);
    }),
  ]).finally(() => clearTimeout(timer));
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
    return new URL(url).pathname;
  } catch {
    return "invalid";
  }
}
