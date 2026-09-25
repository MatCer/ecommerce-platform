import type { PickupPoint } from "@platform/storefront-sdk/types";

/**
 * The Packeta pickup-point widget (spec §10.5), loaded on interaction only: Packeta's
 * `library.js` in production, the local mock (`apps/mocks`) in development; both expose
 * `Packeta.Widget.pick(apiKey, callback, options)`. The edge allows the widget origin in the
 * checkout CSP (script + frame source).
 */
export interface PacketaApi {
  Widget: {
    pick(
      apiKey: string,
      callback: (point: unknown) => void,
      options?: { country?: string; language?: string },
    ): void;
  };
}

let loading: Promise<PacketaApi> | null = null;

function current(): PacketaApi | null {
  const p = (window as { Packeta?: unknown }).Packeta;
  return typeof p === "object" && p !== null && "Widget" in p ? (p as PacketaApi) : null;
}

export function loadPacketa(src: string): Promise<PacketaApi> {
  const ready = current();
  if (ready) return Promise.resolve(ready);
  loading ??= new Promise<PacketaApi>((resolve, reject) => {
    const s = document.createElement("script");
    s.src = src;
    s.async = true;
    s.onload = () => {
      const api = current();
      if (api) resolve(api);
      else reject(new Error("the widget did not load"));
    };
    s.onerror = () => reject(new Error("the widget did not load"));
    document.head.append(s);
  }).catch((e: unknown) => {
    loading = null;
    throw e;
  });
  return loading;
}

const str = (v: unknown) => (typeof v === "string" || typeof v === "number" ? String(v) : "");

/** The widget's point (`null` when closed) as the checkout API's snapshot. */
export function toPickupPoint(raw: unknown): PickupPoint | null {
  if (typeof raw !== "object" || raw === null) return null;
  const p = raw as Record<string, unknown>;
  const point = {
    id: str(p.id),
    name: str(p.name),
    street: str(p.street),
    city: str(p.city),
    zip: str(p.zip),
    country: str(p.country).toUpperCase(),
  };
  return Object.values(point).every((v) => v !== "") ? point : null;
}
