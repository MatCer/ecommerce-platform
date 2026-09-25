import type { Schemas } from "./api.ts";

export type FlowConfig = Schemas["FlowConfig"];

/** Steps a flow may have (the API's `FlowConfig::validate`). */
export function stepCount(kind: string): number {
  return kind === "abandoned_cart" ? 3 : 1;
}

const HOURS = /^\d{1,4}$/;
const MAX_HOURS = 24 * 90;

/**
 * Settings form -> `FlowConfig`, or the i18n key of the first problem. Mirrors the API's
 * validation so the form says what is wrong before saving; the API stays authoritative.
 * `delays` are the step fields in order; empty trailing fields mean "no such step".
 */
export function flowConfig(
  kind: string,
  delays: string[],
  couponPercent: string | null,
): FlowConfig | "flows.errDelays" | "flows.errCoupon" {
  const filled = delays.map((d) => d.trim());
  while (filled.length > 0 && filled[filled.length - 1] === "") filled.pop();
  if (filled.length === 0 || filled.length > stepCount(kind) || !filled.every((d) => HOURS.test(d)))
    return "flows.errDelays";
  const hours = filled.map(Number);
  if (
    hours.some((h) => h > MAX_HOURS) ||
    hours.some((h, i) => i > 0 && h <= (hours[i - 1] ?? 0)) ||
    (kind === "abandoned_cart" && (hours[0] ?? 0) < 1)
  )
    return "flows.errDelays";
  let coupon: number | null = null;
  if (couponPercent !== null) {
    const p = couponPercent.trim();
    if (kind !== "abandoned_cart" || !/^\d{1,2}$/.test(p) || Number(p) < 1 || Number(p) > 50)
      return "flows.errCoupon";
    coupon = Number(p);
  }
  return { delays_hours: hours, coupon_percent: coupon };
}
