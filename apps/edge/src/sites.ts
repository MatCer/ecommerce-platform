/**
 * What the edge needs to know about a storefront host, mapped from the API's
 * `GET /internal/v1/resolve?host=` (spec §8.4) by `ApiResolver`.
 */
export interface Site {
  tenant_id: string;
  market_id: string;
  locale: string;
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
    const r = (await res.json()) as Resolved;
    return {
      tenant_id: r.tenant_id,
      market_id: r.market_id,
      locale: r.default_locale,
      shop_host: r.hostname,
      storefront_token: r.storefront_token,
      theme_artifact: r.theme_artifact,
      retained_artifacts: r.retained_artifacts,
      checkout_artifact: r.checkout_artifact,
    };
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
