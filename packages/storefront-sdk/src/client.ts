import type { Cart, ConsentPurpose, SearchSuggest } from "./types.ts";

/**
 * Browser-side helpers for islands. Everything goes through same-origin gateway routes
 * (`/_p/*`, spec A4); the cart capability is an HttpOnly cookie the browser attaches.
 */

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`request failed: ${res.status}`);
  return (await res.json()) as T;
}

const send = (method: string, path: string, body?: unknown) =>
  fetch(path, {
    method,
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  }).then((r) => json<Cart>(r));

export const cart = {
  get: () => send("GET", "/_p/cart"),
  add: (variantId: string, quantity = 1) =>
    send("POST", "/_p/cart/lines", { variant_id: variantId, quantity }),
  update: (lineId: string, quantity: number) =>
    send("PATCH", `/_p/cart/lines/${encodeURIComponent(lineId)}`, { quantity }),
  remove: (lineId: string) => send("DELETE", `/_p/cart/lines/${encodeURIComponent(lineId)}`),
};

export const suggest = (q: string, signal?: AbortSignal) =>
  fetch(`/_p/public/search/suggest?${new URLSearchParams({ q })}`, { signal }).then((r) =>
    json<SearchSuggest>(r),
  );

// --- consent (spec A20: before consent, no device storage and no beacons) ---------------------

const CONSENT_COOKIE = "consent";

/** `null` = the visitor has not decided yet (show the banner). */
export function readConsent(cookie = globalThis.document?.cookie ?? ""): ConsentPurpose[] | null {
  const raw = cookie
    .split(";")
    .map((c) => c.trim())
    .find((c) => c.startsWith(`${CONSENT_COOKIE}=`))
    ?.slice(CONSENT_COOKIE.length + 1);
  if (raw === undefined) return null;
  return decodeURIComponent(raw)
    .split(",")
    .filter((p): p is ConsentPurpose =>
      ["analytics", "ads", "personalization", "email_marketing", "review_invites"].includes(p),
    );
}

/** Stores the choice (also "none") for 180 days and reports it to the platform. */
export function writeConsent(purposes: ConsentPurpose[]) {
  // A strictly necessary cookie (it records the choice itself), so it is allowed before consent.
  // biome-ignore lint/suspicious/noDocumentCookie: Cookie Store API is not available in Safari/Firefox
  document.cookie = `${CONSENT_COOKIE}=${encodeURIComponent(purposes.join(","))}; Path=/; Max-Age=15552000; SameSite=Lax; Secure`;
  navigator.sendBeacon?.("/_p/e", JSON.stringify({ events: [{ type: "consent", purposes }] }));
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
  const allowed = () => readConsent()?.includes(purpose) ?? false;
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

/**
 * Web Vitals RUM (spec §9.6): only for consented, sampled page views; `web-vitals` is loaded
 * lazily, so unsampled visitors download nothing.
 */
export async function startRum(sampleRate: number, beacon = createBeacon()) {
  if (!(readConsent()?.includes("analytics") ?? false) || Math.random() >= sampleRate) return;
  const { onCLS, onINP, onLCP } = await import("web-vitals/attribution");
  const report = (m: { name: string; value: number; rating: string }) =>
    beacon.track({
      type: "web_vital",
      name: m.name,
      value: Math.round(m.value * 1000) / 1000,
      rating: m.rating,
    });
  onLCP(report);
  onINP(report);
  onCLS(report);
}
