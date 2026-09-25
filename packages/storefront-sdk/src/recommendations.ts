import type { Recommendations } from "./types.ts";

/**
 * Recommendation reads for islands (WP17). A module of its own, so only the lazily loaded
 * islands that use it (cart drawer, "for you", recently viewed) download it, never the shared
 * first-load chunk.
 */

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`request failed: ${res.status}`);
  return (await res.json()) as T;
}

/**
 * Private recommendations: `context` is `cart` (cross-sell for the cart) or `home` (personal
 * picks while the server-side records grant `personalization`, else the public ones). The edge
 * adds the cart and consent cookies; the answer is never cached (A2). Always the unprefixed
 * route (the cart cookie is `Path=/_p`); `locale` picks the page's language.
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
 * Recently viewed products with live prices and availability. The device keeps the ids (only
 * while `personalization` is granted, A20); they are rehydrated through the public route,
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
