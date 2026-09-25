/**
 * Edge HTML cache (spec §9.3.5) under the A2 policy: an explicit allowlist, edge-owned, and
 * themes can only shorten lifetimes, never extend them.
 */

export const DEFAULT_TTL_S = 60;
export const SWR_S = 300;

/** Theme page routes that may be cached (A2). Optional locale prefix: `/sk/...`. */
const CACHEABLE_PATH =
  /^(\/[a-z]{2})?(\/|\/c\/.+|\/p\/[^/]+|\/pages\/[^/]+|\/blog(\/[^/]+)?|\/search)$/;

/** Query parameters carrying capabilities (A2): never cache such URLs. */
const CAPABILITY_PARAMS = ["token", "h", "sig"];

/** Tracking parameters dropped from the cache key and from the URL the theme sees. */
const TRACKING_PARAM = /^(utm_[a-z]+|gclid|gbraid|wbraid|fbclid|msclkid|sklik_[a-z]+|_gl)$/;

export type Verdict = { cache: true } | { cache: false; reason: string };

export interface RequestFacts {
  method: string;
  url: URL;
  origin: "shop" | "checkout";
  preview: boolean;
  headers: Headers;
}

export function requestVerdict(r: RequestFacts): Verdict {
  if (r.method !== "GET" && r.method !== "HEAD") return { cache: false, reason: "method" };
  if (r.origin === "checkout") return { cache: false, reason: "checkout origin" };
  if (r.preview) return { cache: false, reason: "preview" };
  if (r.url.pathname.startsWith("/_p/")) return { cache: false, reason: "platform route" };
  if (r.headers.has("authorization")) return { cache: false, reason: "authorization" };
  for (const p of CAPABILITY_PARAMS) {
    if (r.url.searchParams.has(p)) return { cache: false, reason: "capability token in URL" };
  }
  if (!CACHEABLE_PATH.test(r.url.pathname)) return { cache: false, reason: "not allowlisted" };
  return { cache: true };
}

export interface ResponseFacts {
  status: number;
  headers: Headers;
  /** From the page models the render used (collected by the STOREFRONT binding). */
  pageModels: { anyPrivate: boolean; minMaxAge: number | null };
}

/** Returns the TTL in seconds, or a reason not to cache. */
export function responseVerdict(
  r: ResponseFacts,
): { ttl: number } | { cache: false; reason: string } {
  if (r.status !== 200) return { cache: false, reason: "status" };
  if (r.headers.has("set-cookie")) return { cache: false, reason: "set-cookie" };
  const cc = parseCacheControl(r.headers.get("cache-control"));
  if (cc.has("private") || cc.has("no-store") || cc.has("no-cache")) {
    return { cache: false, reason: "cache-control" };
  }
  if (r.pageModels.anyPrivate) return { cache: false, reason: "private page model" };
  // Themes may only lower the TTL (s-maxage, then max-age), never raise it above the default.
  const theme = numberOr(cc.get("s-maxage"), numberOr(cc.get("max-age"), DEFAULT_TTL_S));
  const ttl = Math.min(DEFAULT_TTL_S, theme, r.pageModels.minMaxAge ?? DEFAULT_TTL_S);
  return ttl > 0 ? { ttl } : { cache: false, reason: "ttl 0" };
}

function parseCacheControl(v: string | null): Map<string, string> {
  const out = new Map<string, string>();
  for (const part of (v ?? "").split(",")) {
    const [k, val] = part.trim().toLowerCase().split("=");
    if (k) out.set(k, val ?? "");
  }
  return out;
}

function numberOr(v: string | undefined, fallback: number) {
  const n = v === undefined ? Number.NaN : Number.parseInt(v, 10);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}

/** Drops tracking params and sorts the rest, so equivalent URLs share one entry. */
export function normalizeUrl(url: URL): URL {
  const out = new URL(url);
  const kept = [...url.searchParams].filter(([k]) => !TRACKING_PARAM.test(k));
  kept.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  out.search = new URLSearchParams(kept).toString();
  return out;
}

export interface CacheEntry {
  status: number;
  headers: [string, string][];
  body: Uint8Array<ArrayBuffer>;
  storedAt: number;
  ttlMs: number;
  tags: string[];
  tenantId: string;
}

/**
 * In-memory LRU bounded by bytes. `get` reports freshness; stale entries within the SWR window
 * are still served while the caller revalidates.
 */
export class HtmlCache {
  readonly #entries = new Map<string, CacheEntry>();
  readonly #maxBytes: number;
  #bytes = 0;
  #generation = 0;

  constructor(maxBytes = 64 * 1024 * 1024) {
    this.#maxBytes = maxBytes;
  }

  get size() {
    return this.#entries.size;
  }

  get(key: string, now = Date.now()): { entry: CacheEntry; fresh: boolean } | null {
    const entry = this.#entries.get(key);
    if (!entry) return null;
    const age = now - entry.storedAt;
    if (age >= entry.ttlMs + SWR_S * 1000) {
      this.#delete(key);
      return null;
    }
    this.#entries.delete(key); // LRU bump
    this.#entries.set(key, entry);
    return { entry, fresh: age < entry.ttlMs };
  }

  /** Bumped by every purge; a render started before a purge must not write its result. */
  get generation() {
    return this.#generation;
  }

  /** Stores `entry` unless a purge happened since `generation` was read (then it is stale). */
  set(key: string, entry: CacheEntry, generation = this.#generation) {
    this.#delete(key);
    if (generation !== this.#generation) return;
    const size = sizeOf(key, entry);
    if (size > this.#maxBytes / 16) return; // do not let one page evict everything
    this.#entries.set(key, entry);
    this.#bytes += size;
    for (const k of this.#entries.keys()) {
      if (this.#bytes <= this.#maxBytes && this.#entries.size <= MAX_ENTRIES) break;
      this.#delete(k);
    }
  }

  delete(key: string) {
    this.#delete(key);
  }

  /**
   * Purge everything, a tenant, tags, or tags within a tenant (both given = both must match).
   * Returns the number of removed entries.
   */
  purge(sel: { tags?: string[]; tenantId?: string; all?: boolean }): number {
    if (!sel.all && sel.tags === undefined && sel.tenantId === undefined) return 0;
    this.#generation++;
    let n = 0;
    for (const [k, e] of this.#entries) {
      const hit =
        sel.all ||
        ((sel.tenantId === undefined || e.tenantId === sel.tenantId) &&
          (sel.tags === undefined || sel.tags.some((t) => e.tags.includes(t))));
      if (hit) {
        this.#delete(k);
        n++;
      }
    }
    return n;
  }

  #delete(key: string) {
    const e = this.#entries.get(key);
    if (!e) return;
    this.#entries.delete(key);
    this.#bytes -= sizeOf(key, e);
  }
}

/** Entry count cap: empty or tiny pages must not grow the map without bound. */
const MAX_ENTRIES = 20_000;

/** Body plus key, headers and tags, so metadata counts against the byte budget too. */
function sizeOf(key: string, e: CacheEntry) {
  let n = 256 + key.length + e.body.byteLength;
  for (const [k, v] of e.headers) n += k.length + v.length;
  for (const t of e.tags) n += t.length;
  return n;
}
