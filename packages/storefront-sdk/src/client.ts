import { hasConsent } from "./consent.ts";
import type {
  Cart,
  CartState,
  ConsentPurpose,
  Recommendations,
  SearchHit,
  SearchResult,
  SearchSuggest,
} from "./types.ts";

export {
  consentStorage,
  hasConsent,
  openConsentSettings,
  readConsent,
  saveConsent,
} from "./consent.ts";

/**
 * Browser-side helpers for islands. Everything goes through same-origin gateway routes
 * (`/_p/*`, spec A4); the cart capability is an HttpOnly cookie the browser attaches.
 */

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`request failed: ${res.status}`);
  return (await res.json()) as T;
}

const send = (method: string, path: string, body?: unknown, idempotencyKey?: string) =>
  fetch(path, {
    method,
    headers: {
      ...(body === undefined ? {} : { "content-type": "application/json" }),
      ...(idempotencyKey ? { "idempotency-key": idempotencyKey } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  }).then((r) => json<Cart>(r));

export const cart = {
  get: () => fetch("/_p/cart", { credentials: "same-origin" }).then((r) => json<CartState>(r)),
  /** Pass the same `idempotencyKey` when retrying an add, so it is counted once. */
  add: (variantId: string, quantity = 1, idempotencyKey?: string) =>
    send("POST", "/_p/cart/lines", { variant_id: variantId, quantity }, idempotencyKey),
  update: (lineId: string, quantity: number) =>
    send("PATCH", `/_p/cart/lines/${encodeURIComponent(lineId)}`, { quantity }),
  remove: (lineId: string) => send("DELETE", `/_p/cart/lines/${encodeURIComponent(lineId)}`),
  applyCoupon: (code: string) => send("POST", "/_p/cart/coupons", { code }),
  removeCoupon: (code: string) => send("DELETE", `/_p/cart/coupons/${encodeURIComponent(code)}`),
};

/**
 * Public reads for islands. `base` is `ShopModel.base_path` (`/cs` on a prefixed locale), so
 * the edge answers in the page's locale.
 */
export const suggest = (q: string, opts: { base?: string; signal?: AbortSignal } = {}) =>
  fetch(`${opts.base ?? ""}/_p/public/search/suggest?${new URLSearchParams({ q })}`, {
    signal: opts.signal,
  }).then((r) => json<SearchSuggest>(r));

/** Full search from an island: `params` as in `/storefront/v1/search` (`q`, `f.opt.color`, ...). */
export const search = (
  params: URLSearchParams,
  opts: { base?: string; signal?: AbortSignal } = {},
) =>
  fetch(`${opts.base ?? ""}/_p/public/search?${params}`, { signal: opts.signal }).then((r) =>
    json<SearchResult>(r),
  );

/**
 * Private recommendations for islands (WP17): `context` is `cart` (cross-sell for the cart) or
 * `home` (personal picks while the server-side records grant `personalization`, else the public
 * ones). The edge adds the cart and consent cookies; the answer is never cached (A2). Always the
 * unprefixed route (the cart cookie is `Path=/_p`); `locale` picks the page's language.
 */
export const recommendations = (
  query: { context: string; limit?: number },
  opts: { locale?: string; signal?: AbortSignal } = {},
) => {
  const p = new URLSearchParams({ context: query.context });
  if (query.limit !== undefined) p.set("limit", String(query.limit));
  if (opts.locale) p.set("locale", opts.locale);
  return fetch(`/_p/recommendations?${p}`, {
    credentials: "same-origin",
    signal: opts.signal,
  }).then((r) => json<Recommendations>(r));
};

/**
 * Recently viewed products with live prices and availability (WP17). The device keeps the ids
 * (only while `personalization` is granted, A20); they are rehydrated through the public route,
 * without cookies or any identity, so the server never links the history to anyone.
 */
export const recentlyViewed = (ids: string[], opts: { base?: string; signal?: AbortSignal } = {}) =>
  fetch(
    `${opts.base ?? ""}/_p/public/recommendations?${new URLSearchParams({
      context: "recent",
      ids: ids.join(","),
      limit: String(Math.max(1, ids.length)),
    })}`,
    { credentials: "omit", signal: opts.signal },
  ).then((r) => json<Recommendations>(r));

/** The smallest AVIF (else any) thumbnail of a search hit at least `width` px wide. */
export function hitThumb(hit: SearchHit, width: number): string | undefined {
  const sorted = [...hit.image].sort((a, b) => a.width - b.width);
  const avif = sorted.filter((v) => v.format === "avif");
  const pool = avif.length ? avif : sorted;
  const v = pool.find((x) => x.width >= width) ?? pool.at(-1);
  // Same-origin: the shop serves the public media bucket under /media (CSP img-src 'self').
  return v && `/${v.key}`;
}

// --- events beacon ------------------------------------------------------------------------------

export interface BeaconEvent {
  type: string;
  [key: string]: unknown;
}

/**
 * Batched, consent-aware beacon to `/_p/e`. Sends nothing unless `purpose` was granted; the
 * server re-checks consent from its own records and never trusts the client (A20).
 */
export function createBeacon({
  purpose = "analytics",
  endpoint = "/_p/e",
}: {
  purpose?: ConsentPurpose;
  endpoint?: string;
} = {}) {
  let queue: BeaconEvent[] = [];
  const allowed = () => hasConsent(purpose);
  const flush = () => {
    if (!queue.length) return;
    if (allowed())
      navigator.sendBeacon(endpoint, JSON.stringify({ events: queue, path: location.pathname }));
    queue = [];
  };
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && flush());
  addEventListener("pagehide", flush);
  return {
    track(e: BeaconEvent) {
      if (!allowed()) return;
      queue.push(e);
      if (queue.length >= 20) flush();
    },
    flush,
  };
}

let shared: ReturnType<typeof createBeacon> | undefined;

/**
 * Queues an analytics event (`page_view`, `view_item`, `add_to_cart`, `begin_checkout`) on
 * the page's one shared beacon: nothing is queued or sent without `analytics` consent, and the
 * server checks its own consent records again (A20). Sent when the page is hidden; call
 * `flushEvents()` before a navigation that must not lose it.
 */
const sharedBeacon = () => {
  shared ??= createBeacon();
  return shared;
};
export const track = (e: BeaconEvent) => sharedBeacon().track(e);
export const flushEvents = () => shared?.flush();

/**
 * Web Vitals RUM (spec §9.6): only for consented, sampled page views. The reporter
 * (`vitals.ts`, < 1 kB) loads lazily, so unsampled visitors download nothing; its metrics go
 * out in one beacon when the page is hidden. `template` (`home`, `category`, `product`, ...)
 * groups the dashboard's p75 per template.
 */
export async function startRum(sampleRate: number, template: string, beacon = sharedBeacon()) {
  if (!hasConsent("analytics") || Math.random() >= sampleRate) return;
  const { observeVitals } = await import("./vitals.ts");
  observeVitals((metrics) => {
    for (const m of metrics)
      beacon.track({
        type: "web_vital",
        template,
        name: m.name,
        value: Math.round(m.value * 1000) / 1000,
        rating: m.rating,
      });
    beacon.flush();
  });
}
