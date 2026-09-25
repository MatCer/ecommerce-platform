/**
 * What the edge needs to know about a storefront host, mapped from the API's
 * `GET /internal/v1/resolve?host=` (spec §8.4) by `ApiResolver`.
 */
export interface Site {
  tenant_id: string;
  market_id: string;
  /** The market's default locale (no path prefix); a prefixed request overrides it. */
  locale: string;
  /** Every locale of the market; the non-default ones are served under `/<locale>/…`. */
  locales: string[];
  /** Canonical shop host of this market (the checkout origin is `checkout.<shop_host>`). */
  shop_host: string;
  /** Public storefront token the edge injects; never exposed to theme code. */
  storefront_token: string;
  /** Active theme artifact (content address); `null` until a theme is published for the tenant. */
  theme_artifact: string | null;
  /**
   * Earlier artifacts of this tenant whose `/_astro/*` files stay reachable, so pages rendered
   * (or cached, prerendered, open in a tab) before a publish keep working (spec A22).
   */
  retained_artifacts: string[];
  /** The platform checkout artifact (same for every tenant); `null` if none is published. */
  checkout_artifact?: string | null;
  /**
   * Request-scoped, set by the gateway (never by the resolver): the client address from the
   * proxy's last `X-Forwarded-For` hop, forwarded as `X-Client-Ip` on every API call (rate
   * limits per token + IP, salted hashes; spec §8.1).
   */
  clientIp?: string | undefined;
  /**
   * Set on `preview-<n>--<shop>` hosts (WP23, A21): the site carries the previewed revision's
   * artifact; nothing is cached, counted or handed to the checkout.
   */
  preview?: { revision: number; expiresAt: number };
}

export interface SiteResolver {
  resolve(shopHost: string): Promise<Site | null>;
}

const HOST_RE = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/;

/** Lowercases, drops the port and trailing dot; `null` for anything that is not a hostname. */
export function normalizeHost(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const host = raw
    .trim()
    .toLowerCase()
    .replace(/:\d{1,5}$/, "")
    .replace(/\.$/, "");
  return host.length <= 253 && HOST_RE.test(host) ? host : null;
}

export type Origin = { kind: "shop" | "checkout"; shopHost: string };

/** `checkout.<shop>` is the checkout origin of `<shop>` (spec A1); everything else is a shop. */
export function classifyHost(host: string): Origin {
  return host.startsWith("checkout.")
    ? { kind: "checkout", shopHost: host.slice("checkout.".length) }
    : { kind: "shop", shopHost: host };
}

/**
 * Locale prefix (spec §9.1): `/cs/c/x` on a market whose default locale is `sk` and which also
 * offers `cs` is the `cs` page `/c/x`. The default locale is never prefixed, and anything that
 * is not one of the market's locales stays a plain path (the theme answers 404).
 */
export function splitLocale(
  site: Pick<Site, "locale" | "locales">,
  path: string,
): { locale: string; path: string } | null {
  // The first segment must be exactly one of the market's configured tags (`cs`, `en-GB`).
  const m = /^\/([^/]+)(\/.*)?$/.exec(path);
  const locale = m?.[1];
  if (!m || !locale || locale === site.locale || !site.locales.includes(locale)) return null;
  return { locale, path: m[2] && m[2] !== "/" ? m[2] : "/" };
}

/** Fixed host → site map (tests). */
export class StaticResolver implements SiteResolver {
  #sites: Record<string, Site>;
  constructor(sites: Record<string, Site>) {
    this.#sites = sites;
  }
  /** Replaces the map (used by tests and the local publish/rollback demo). */
  set(host: string, site: Site) {
    this.#sites[host] = site;
  }
  async resolve(shopHost: string) {
    return Object.hasOwn(this.#sites, shopHost) ? (this.#sites[shopHost] ?? null) : null;
  }
}

/** The resolve response of the API (`commerce::tenancy::Resolved`). */
interface Resolved {
  hostname: string;
  tenant_id: string;
  market_id: string;
  default_locale: string;
  locales: string[];
  storefront_token: string;
  theme_artifact: string | null;
  retained_artifacts: string[];
  checkout_artifact: string | null;
}

/**
 * Resolves hosts through the API's internal endpoint with the service token. Unknown hosts
 * (404) resolve to `null`; any other failure throws (the edge answers 502, nothing is cached).
 */
export class ApiResolver implements SiteResolver {
  readonly #apiOrigin: string;
  readonly #token: string;
  readonly #upstream: (r: Request) => Promise<Response>;
  constructor(apiOrigin: string, token: string, upstream?: (r: Request) => Promise<Response>) {
    this.#apiOrigin = apiOrigin;
    this.#token = token;
    this.#upstream = upstream ?? ((r) => fetch(r));
  }
  async resolve(shopHost: string): Promise<Site | null> {
    const res = await this.#upstream(
      new Request(
        `${this.#apiOrigin}/internal/v1/resolve?${new URLSearchParams({ host: shopHost })}`,
        { headers: { authorization: `Bearer ${this.#token}`, accept: "application/json" } },
      ),
    );
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`resolve ${shopHost}: ${res.status}`);
    return toSite((await res.json()) as Resolved);
  }
}

function toSite(r: Resolved): Site {
  return {
    tenant_id: r.tenant_id,
    market_id: r.market_id,
    locale: r.default_locale,
    locales: r.locales,
    shop_host: r.hostname,
    storefront_token: r.storefront_token,
    theme_artifact: r.theme_artifact,
    retained_artifacts: r.retained_artifacts,
    checkout_artifact: r.checkout_artifact,
  };
}

/** `preview-<n>--<shop host>` (WP23). */
export const PREVIEW_HOST_RE = /^preview-(\d{1,9})--([a-z0-9.-]+)$/;

export interface PreviewResolver {
  /** The site to serve on a preview host for a token; `null` if not authentic/expired. */
  resolve(host: string, token: string, now?: number): Promise<Site | null>;
}

/**
 * Preview tokens are verified by the API (`GET /internal/v1/previews/resolve`, A21): the HMAC
 * secret never reaches the edge. Answers are cached per (host, token) for at most 60 s and
 * never beyond the token's expiry.
 */
export class ApiPreviewResolver implements PreviewResolver {
  readonly #apiOrigin: string;
  readonly #token: string;
  readonly #upstream: (r: Request) => Promise<Response>;
  readonly #cache = new Map<string, { site: Site | null; until: number }>();
  constructor(apiOrigin: string, token: string, upstream?: (r: Request) => Promise<Response>) {
    this.#apiOrigin = apiOrigin;
    this.#token = token;
    this.#upstream = upstream ?? ((r) => fetch(r));
  }
  async resolve(host: string, token: string, now = Date.now()): Promise<Site | null> {
    if (!PREVIEW_HOST_RE.test(host) || !/^[0-9a-f]{32}\.\d{1,12}\.[0-9a-f]{64}$/.test(token))
      return null;
    const key = `${host} ${token}`;
    const hit = this.#cache.get(key);
    if (hit && now < hit.until) return hit.site;
    const res = await this.#upstream(
      new Request(
        `${this.#apiOrigin}/internal/v1/previews/resolve?${new URLSearchParams({ host, token })}`,
        { headers: { authorization: `Bearer ${this.#token}`, accept: "application/json" } },
      ),
    );
    let site: Site | null = null;
    let until = now + 60_000;
    if (res.ok) {
      const r = (await res.json()) as {
        site: Resolved;
        revision_number: number;
        expires_at: string;
      };
      const expiresAt = Date.parse(r.expires_at);
      site = { ...toSite(r.site), preview: { revision: r.revision_number, expiresAt } };
      until = Math.min(until, expiresAt);
    } else if (res.status !== 404) throw new Error(`preview resolve: ${res.status}`);
    this.#cache.delete(key);
    if (this.#cache.size >= 1000) this.#cache.delete(this.#cache.keys().next().value ?? "");
    this.#cache.set(key, { site, until });
    return site;
  }
}

/** 60 s cache in front of the resolver (spec §9.3.1), purged by `/_edge/purge`. */
export class CachedResolver implements SiteResolver {
  readonly #inner: SiteResolver;
  readonly #ttlMs: number;
  readonly #cache = new Map<string, { site: Site | null; at: number }>();
  constructor(inner: SiteResolver, ttlMs = 60_000) {
    this.#inner = inner;
    this.#ttlMs = ttlMs;
  }
  async resolve(shopHost: string, now = Date.now()) {
    const hit = this.#cache.get(shopHost);
    if (hit && now - hit.at < this.#ttlMs) return hit.site;
    const site = await this.#inner.resolve(shopHost);
    this.#cache.delete(shopHost);
    // Bounded: unknown hosts are cached too (negative caching), so random Host floods must not grow it.
    if (this.#cache.size >= 10_000) this.#cache.delete(this.#cache.keys().next().value ?? "");
    this.#cache.set(shopHost, { site, at: now });
    return site;
  }
  purge(tenantId?: string) {
    for (const [host, { site }] of this.#cache) {
      if (!tenantId || site?.tenant_id === tenantId || site === null) this.#cache.delete(host);
    }
  }
}
