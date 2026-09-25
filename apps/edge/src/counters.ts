import { randomUUID } from "node:crypto";
import type { Upstream } from "./bindings.ts";
import type { Site } from "./sites.ts";

/**
 * Cookieless page counters (spec A20): before (or without) analytics consent the platform only
 * learns how many requests each route template of a market got per UTC day. Nothing identifies
 * a visitor: no IP, no cookie, no path or query beyond the template. Counts are kept in memory
 * and flushed to the API every few seconds in batches with an id; a batch that failed is resent
 * unchanged (same id), so the API never counts it twice.
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
export interface CounterBatch {
  batch_id: string;
  counters: PageRow[];
}

/** Keys held between flushes; beyond this, new keys are dropped (memory bound). */
const MAX_KEYS = 20_000;
/** Rows per request: ~150 bytes each, far below the API's 1 MB body limit. */
const CHUNK = 2_000;
/** Failed batches kept for resending; the oldest is dropped beyond this. */
const MAX_PENDING = 20;

const day = (now: Date) => now.toISOString().slice(0, 10);

export class Counters {
  #pages = new Map<string, PageRow>();
  #pending: CounterBatch[] = [];
  dropped = 0;

  page(site: Site, template: Template, now = new Date()) {
    const d = day(now);
    const key = `${site.tenant_id}|${site.market_id}|${d}|${template}`;
    const row = this.#pages.get(key);
    if (row) row.requests++;
    else if (this.#pages.size < MAX_KEYS)
      this.#pages.set(key, {
        tenant_id: site.tenant_id,
        market_id: site.market_id,
        day: d,
        template,
        requests: 1,
      });
    else this.dropped++;
  }

  /** Counted rows not handed to a batch yet, plus rows of failed batches. */
  get size() {
    return this.#pages.size + this.#pending.reduce((n, b) => n + b.counters.length, 0);
  }

  /** Moves the current counts into new batches (fresh ids) and returns every batch to send. */
  #batches(): CounterBatch[] {
    const rows = [...this.#pages.values()];
    this.#pages = new Map();
    for (let i = 0; i < rows.length; i += CHUNK)
      this.#pending.push({ batch_id: randomUUID(), counters: rows.slice(i, i + CHUNK) });
    while (this.#pending.length > MAX_PENDING) {
      this.dropped += this.#pending.shift()?.counters.length ?? 0;
    }
    return [...this.#pending];
  }

  /** Sends pending batches in order; a failure keeps it (and the later ones) for next time. */
  async flush(apiOrigin: string, token: string, upstream: Upstream = (r) => fetch(r)) {
    for (const batch of this.#batches()) {
      const res = await upstream(
        new Request(`${apiOrigin}/internal/v1/analytics/counters`, {
          method: "POST",
          headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
          body: JSON.stringify(batch),
        }),
      );
      await res.body?.cancel();
      // A 4xx will not get better by resending (bad rows): drop it and count the loss.
      if (res.status >= 500) throw new Error(`HTTP ${res.status}`);
      if (!res.ok) this.dropped += batch.counters.length;
      this.#pending.shift();
    }
  }
}
