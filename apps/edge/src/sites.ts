import { readFile } from "node:fs/promises";

/**
 * What the edge needs to know about a storefront host. This is the response contract of
 * `GET /internal/v1/resolve?host=` (spec §8.4), which WP6 implements in the API; until then
 * `StaticResolver` serves it from a JSON file.
 */
export interface Site {
  tenant_id: string;
  market_id: string;
  locale: string;
  /** Canonical shop host of this market (the checkout origin is `checkout.<shop_host>`). */
  shop_host: string;
  /** Public storefront token the edge injects; never exposed to theme code. */
  storefront_token: string;
  /** Active theme artifact (content address). Publish/rollback = changing this pointer. */
  theme_artifact: string;
  /**
   * Earlier artifacts of this tenant whose `/_astro/*` files stay reachable, so pages rendered
   * (or cached, prerendered, open in a tab) before a publish keep working (spec A22).
   */
  retained_artifacts: string[];
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

/** Host → site map from a JSON file (`{ "demo.localhost": Site, ... }`). */
export class StaticResolver implements SiteResolver {
  #sites: Record<string, Site>;
  constructor(sites: Record<string, Site>) {
    this.#sites = sites;
  }
  static async fromFile(file: string) {
    return new StaticResolver(JSON.parse(await readFile(file, "utf8")) as Record<string, Site>);
  }
  /** Replaces the map (used by tests and the local publish/rollback demo). */
  set(host: string, site: Site) {
    this.#sites[host] = site;
  }
  async resolve(shopHost: string) {
    return Object.hasOwn(this.#sites, shopHost) ? (this.#sites[shopHost] ?? null) : null;
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
