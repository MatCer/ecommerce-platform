import type { Site } from "./sites.ts";
import type { Upstream } from "./bindings.ts";

/**
 * Cookieless page counters (spec A20): before (or without) analytics consent the platform only
 * learns how many requests each route template of a market got per UTC day, plus how often each
 * search query was run. Nothing identifies a visitor: no IP, no cookie, no path beyond the
 * template. Counts are kept in memory and flushed to the API every few seconds; the API
 * minimizes search text before storing it (like the zero-result log).
 */

export type Template =
  | "home"
  | "category"
  | "product"
  | "search"
  | "page"
  | "blog"
  | "checkout"
  | "other";

/** The route template of a theme path (unprefixed by locale). */
export function templateOf(pathname: string): Template {
  if (pathname === "/" || pathname === "") return "home";
  if (pathname.startsWith("/c/")) return "category";
  if (pathname.startsWith("/p/")) return "product";
  if (pathname === "/search") return "search";
  if (pathname.startsWith("/pages/")) return "page";
  if (pathname === "/blog" || pathname.startsWith("/blog/")) return "blog";
  return "other";
}

interface PageRow {
  tenant_id: string;
  market_id: string;
  day: string;
  template: Template;
  requests: number;
}
interface SearchRow {
  tenant_id: string;
  day: string;
  locale: string;
  query: string;
  count: number;
}
export interface CounterBatch {
  counters: PageRow[];
  searches: SearchRow[];
}

/** Keys held between flushes; beyond these, new keys are dropped (memory bound). */
const MAX_PAGE_KEYS = 20_000;
const MAX_SEARCH_KEYS = 5_000;
/** Rows per request (the API caps bodies at 1 MB). */
const CHUNK = 2_000;

const day = (now: Date) => now.toISOString().slice(0, 10);

export class Counters {
  #pages = new Map<string, PageRow>();
  #searches = new Map<string, SearchRow>();
  dropped = 0;

  page(site: Site, template: Template, now = new Date()) {
    const d = day(now);
    const key = `${site.tenant_id}|${site.market_id}|${d}|${template}`;
    const row = this.#pages.get(key);
    if (row) row.requests++;
    else if (this.#pages.size < MAX_PAGE_KEYS)
      this.#pages.set(key, {
        tenant_id: site.tenant_id,
        market_id: site.market_id,
        day: d,
        template,
        requests: 1,
      });
    else this.dropped++;
  }

  search(site: Site, raw: string, now = new Date()) {
    const query = raw.trim().toLowerCase().replace(/\s+/g, " ").slice(0, 200);
    if (!query) return;
    const d = day(now);
    const key = `${site.tenant_id}|${d}|${site.locale}|${query}`;
    const row = this.#searches.get(key);
    if (row) row.count++;
    else if (this.#searches.size < MAX_SEARCH_KEYS)
      this.#searches.set(key, { tenant_id: site.tenant_id, day: d, locale: site.locale, query, count: 1 });
    else this.dropped++;
  }

  get size() {
    return this.#pages.size + this.#searches.size;
  }

  /** Takes everything counted so far. */
  drain(): CounterBatch {
    const batch = { counters: [...this.#pages.values()], searches: [...this.#searches.values()] };
    this.#pages = new Map();
    this.#searches = new Map();
    return batch;
  }

  /** Puts back rows a failed flush could not deliver (merged with newer counts). */
  restore(batch: CounterBatch) {
    for (const r of batch.counters) {
      const key = `${r.tenant_id}|${r.market_id}|${r.day}|${r.template}`;
      const row = this.#pages.get(key);
      if (row) row.requests += r.requests;
      else if (this.#pages.size < MAX_PAGE_KEYS) this.#pages.set(key, r);
      else this.dropped += r.requests;
    }
    for (const r of batch.searches) {
      const key = `${r.tenant_id}|${r.day}|${r.locale}|${r.query}`;
      const row = this.#searches.get(key);
      if (row) row.count += r.count;
      else if (this.#searches.size < MAX_SEARCH_KEYS) this.#searches.set(key, r);
      else this.dropped += r.count;
    }
  }

  /** Sends the counts to the API; anything not delivered is kept for the next flush. */
  async flush(apiOrigin: string, token: string, upstream: Upstream = (r) => fetch(r)) {
    const { counters, searches } = this.drain();
    const chunks = Math.max(Math.ceil(counters.length / CHUNK), Math.ceil(searches.length / CHUNK));
    for (let i = 0; i < chunks; i++) {
      const batch = {
        counters: counters.slice(i * CHUNK, (i + 1) * CHUNK),
        searches: searches.slice(i * CHUNK, (i + 1) * CHUNK),
      };
      try {
        const res = await upstream(
          new Request(`${apiOrigin}/internal/v1/analytics/counters`, {
            method: "POST",
            headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
            body: JSON.stringify(batch),
          }),
        );
        await res.body?.cancel();
        // A 4xx will not get better by retrying (bad rows); drop it rather than loop forever.
        if (res.status >= 500) throw new Error(`HTTP ${res.status}`);
      } catch (err) {
        // This chunk and every later one wait for the next flush.
        this.restore({
          counters: counters.slice(i * CHUNK),
          searches: searches.slice(i * CHUNK),
        });
        throw err;
      }
    }
  }
}
