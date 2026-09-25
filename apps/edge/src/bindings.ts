import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import path from "node:path";
import type { ArtifactManifest } from "@platform/theme-kit";
import type { Site } from "./sites.ts";

/** Header carrying the per-request context id from the edge to the worker and back. */
export const CTX_HEADER = "x-platform-ctx";

/** Server-side state of one in-flight storefront request. Workers only ever see the opaque id. */
export interface RequestContext {
  site: Site;
  /** Artifact that is allowed to use this context (a worker cannot borrow another's). */
  artifactId: string;
  /** Checkout origin only: the checkout-scoped cart capability from the `__Host-cart` cookie. */
  cartToken?: string;
  /** Collected from page models (`cache` hints, spec §8.2) for the edge cache decision. */
  tags: Set<string>;
  anyPrivate: boolean;
  minMaxAge: number | null;
  /** Binding calls made by this render (fan-out budget, see MAX_SUBREQUESTS). */
  subrequests: number;
}

/**
 * Maps opaque, unguessable ids to request contexts for the duration of a render. The tenant a
 * binding call acts for comes from here, never from anything the worker sends (spec A7).
 */
export class ContextRegistry {
  readonly #map = new Map<string, RequestContext>();

  open(site: Site, artifactId: string, extra: Partial<RequestContext> = {}): string {
    const id = randomBytes(18).toString("base64url");
    this.#map.set(id, {
      site,
      artifactId,
      tags: new Set(),
      anyPrivate: false,
      minMaxAge: null,
      subrequests: 0,
      ...extra,
    });
    return id;
  }

  get(id: string | null, artifactId: string): RequestContext | null {
    const ctx = id ? this.#map.get(id) : undefined;
    return ctx && ctx.artifactId === artifactId ? ctx : null;
  }

  close(id: string) {
    this.#map.delete(id);
  }

  get size() {
    return this.#map.size;
  }
}

export type Upstream = (request: Request) => Promise<Response>;

/** Per-render binding call budget (Cloudflare Workers allow 50 subrequests on the free plan). */
export const MAX_SUBREQUESTS = 50;

/** Anything that authenticates or selects a tenant. Workers must never send these (A7). */
const CREDENTIAL_HEADERS = [
  "authorization",
  "proxy-authorization",
  "cookie",
  "x-tenant",
  "x-market",
  "x-locale",
  "x-storefront-token",
  "x-cart-token",
  "x-session",
];

// Encoded separators/dots would let a path smuggle segments past the allowlist.
const SMUGGLING = /%2f|%5c|%2e|\\|\/\//i;

const problem = (status: number, code: string, detail: string) =>
  Response.json(
    { type: "about:blank", title: code, status, code, detail },
    {
      status,
      headers: { "content-type": "application/problem+json" },
    },
  );

interface Operation {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  path: RegExp;
}

/** Public page-model/catalog reads (spec §8.2): the whole surface a theme can reach. */
export const STOREFRONT_OPERATIONS: Operation[] = [
  { method: "GET", path: /^\/shop$/ },
  { method: "GET", path: /^\/pages\/(home|search|blog)$/ },
  { method: "GET", path: /^\/pages\/(category|product|cms|blog)\/[a-z0-9][a-z0-9-/]{0,199}$/ },
  { method: "GET", path: /^\/search\/suggest$/ },
  { method: "GET", path: /^\/recommendations$/ },
  { method: "GET", path: /^\/redirects\/resolve$/ },
];

/** The checkout app's capabilities (A7: a separate binding). Grows with WP9/WP10. */
export const CHECKOUT_OPERATIONS: Operation[] = [
  { method: "GET", path: /^\/shop$/ },
  { method: "GET", path: /^\/cart$/ },
];

/**
 * Builds a restricted binding: validates the call against `operations`, resolves the tenant
 * from the request context and forwards to the Storefront API with platform-injected headers.
 */
export function restrictedBinding(opts: {
  name: string;
  operations: Operation[];
  registry: ContextRegistry;
  artifactId: string;
  /** Tenant this instance is isolated to (theme workers); contexts of other tenants are refused. */
  scope?: string | undefined;
  apiOrigin: string;
  upstream: Upstream;
}) {
  return async (request: Request): Promise<Response> => {
    for (const h of CREDENTIAL_HEADERS) {
      if (request.headers.has(h))
        return problem(400, "credentials_not_allowed", `${opts.name}: header ${h} is not allowed`);
    }
    const found = opts.registry.get(request.headers.get(CTX_HEADER), opts.artifactId);
    const ctx = found && (!opts.scope || found.site.tenant_id === opts.scope) ? found : null;
    if (!ctx)
      return problem(403, "no_request_context", `${opts.name}: missing or unknown request context`);
    // Workers cap subrequests per invocation; so do we, so N+1 page-model fan-out fails loudly.
    if (++ctx.subrequests > MAX_SUBREQUESTS) {
      return problem(
        429,
        "too_many_subrequests",
        `${opts.name}: over ${MAX_SUBREQUESTS} calls in one render`,
      );
    }

    const url = new URL(request.url);
    if (SMUGGLING.test(url.pathname) || url.search.length > 2048) {
      return problem(400, "bad_path", `${opts.name}: path not allowed`);
    }
    const op = opts.operations.find(
      (o) => o.method === request.method && o.path.test(url.pathname),
    );
    if (!op)
      return problem(
        404,
        "operation_not_allowed",
        `${opts.name}: ${request.method} ${url.pathname} is not exposed`,
      );

    const headers = new Headers({
      accept: "application/json",
      "x-tenant": ctx.site.tenant_id,
      "x-market": ctx.site.market_id,
      "x-locale": ctx.site.locale,
      "x-storefront-token": ctx.site.storefront_token,
    });
    if (ctx.cartToken) headers.set("x-cart-token", ctx.cartToken);
    const res = await opts.upstream(
      new Request(`${opts.apiOrigin}/storefront/v1${url.pathname}${url.search}`, {
        method: op.method,
        headers,
      }),
    );
    const body = await res.arrayBuffer();
    if (res.ok && (res.headers.get("content-type") ?? "").includes("json"))
      collectCacheHints(ctx, body);
    // Only the payload goes back: no upstream cookies or internal headers.
    return new Response(body, {
      status: res.status,
      headers: { "content-type": res.headers.get("content-type") ?? "application/json" },
    });
  };
}

function collectCacheHints(ctx: RequestContext, body: ArrayBuffer) {
  let hints: unknown;
  try {
    hints = (JSON.parse(new TextDecoder().decode(body)) as { cache?: unknown }).cache;
  } catch {
    return;
  }
  if (typeof hints !== "object" || hints === null) return;
  const h = hints as { public?: unknown; max_age?: unknown; tags?: unknown };
  if (h.public === false) ctx.anyPrivate = true;
  if (typeof h.max_age === "number" && h.max_age >= 0) {
    ctx.minMaxAge = Math.min(ctx.minMaxAge ?? h.max_age, h.max_age);
  }
  if (Array.isArray(h.tags)) {
    for (const t of h.tags) if (typeof t === "string" && t.length <= 128) ctx.tags.add(t);
  }
}

const CONTENT_TYPES: Record<string, string> = {
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".avif": "image/avif",
  ".webp": "image/webp",
  ".woff2": "font/woff2",
  ".ico": "image/x-icon",
  ".txt": "text/plain; charset=utf-8",
  ".xml": "application/xml",
  ".webmanifest": "application/manifest+json",
};

export const contentType = (p: string) =>
  CONTENT_TYPES[path.extname(p).toLowerCase()] ?? "application/octet-stream";

/** Reads a file listed in the artifact manifest; `null` for anything not listed. */
export async function readAsset(root: string, manifest: ArtifactManifest, urlPath: string) {
  if (!Object.hasOwn(manifest.assets, urlPath)) return null;
  return readFile(path.join(root, manifest.id, "client", urlPath));
}

/**
 * `ASSETS` binding: the Astro Cloudflare entry calls `env.ASSETS.fetch()` for unmatched routes
 * and prerendered error pages. Read-only, limited to this artifact's own manifest.
 */
export function assetsBinding(root: string, manifest: ArtifactManifest) {
  return async (request: Request): Promise<Response> => {
    const p = new URL(request.url).pathname;
    const buf = await readAsset(root, manifest, p);
    return buf
      ? new Response(buf, { headers: { "content-type": contentType(p) } })
      : new Response("Not found", { status: 404 });
  };
}
