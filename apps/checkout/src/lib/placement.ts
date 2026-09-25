import type { PlacedOrder, PlaceOrderInput } from "@platform/storefront-sdk/types";
import { call, type Result } from "./client";

/**
 * A placement whose outcome is unknown (lost response, gateway error, still running) is kept in
 * the tab's session storage and replayed with the same Idempotency-Key and body (A12), also
 * after a reload, when the converted cart no longer shows a checkout. Kept at most 24 h (the
 * server's key retention).
 */
export interface Placement {
  key: string;
  body: PlaceOrderInput;
  at: number;
}

const STORAGE = "checkout:placement";
const MAX_AGE_MS = 24 * 3600 * 1000;

export function loadPlacement(): Placement | null {
  try {
    const raw = sessionStorage.getItem(STORAGE);
    const p = raw ? (JSON.parse(raw) as Placement) : null;
    return p && typeof p.key === "string" && Date.now() - p.at < MAX_AGE_MS ? p : null;
  } catch {
    return null;
  }
}

export function forgetPlacement() {
  try {
    sessionStorage.removeItem(STORAGE);
  } catch {
    // Storage unavailable: nothing was kept.
  }
}

/** Sends (or replays) a placement; keeps it while its outcome is unknown. */
export async function sendPlacement(p: Placement): Promise<Result<PlacedOrder>> {
  try {
    sessionStorage.setItem(STORAGE, JSON.stringify(p));
  } catch {
    // Without storage the in-memory replay still covers a retry without reload.
  }
  const r = await call<PlacedOrder>("POST", "/_p/checkout/place-order", p.body, {
    "idempotency-key": p.key,
  });
  if (!unknownOutcome(r)) forgetPlacement();
  return r;
}

/** The order may or may not exist: network error, 5xx, or the first request still running. */
export function unknownOutcome(r: Result<unknown>): boolean {
  return !r.ok && (r.code === "network" || r.status >= 500 || r.code === "idempotency_in_progress");
}

/** Where to go after a placement: the provider's page, or the order page. */
export function nextUrl(placed: PlacedOrder): string {
  const action = placed.payment.action;
  return action?.type === "redirect" ? action.url : placed.confirmation_url;
}
