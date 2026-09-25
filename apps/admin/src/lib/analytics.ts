/** Pure helpers for the analytics dashboard and the webhook settings (days are UTC). */

const DAY_MS = 86_400_000;
export const PRESETS = [7, 30, 90] as const;
const DEFAULT_PRESET = 30;
/** The API accepts `to` at most this many days after `from`. */
const MAX_SPAN_DAYS = 366;

export interface DayRange {
  from: string;
  to: string;
}

const isoDay = (ms: number) => new Date(ms).toISOString().slice(0, 10);

/** Milliseconds of a real `YYYY-MM-DD` date (UTC midnight), else `null`. */
function dayMs(s: string | undefined): number | null {
  if (!s || !/^\d{4}-\d{2}-\d{2}$/.test(s)) return null;
  const ms = Date.parse(`${s}T00:00:00Z`);
  return Number.isNaN(ms) || isoDay(ms) !== s ? null : ms;
}

/** The last `days` UTC days, today included. */
export function presetRange(days: number, now = new Date()): DayRange {
  const today = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
  return { from: isoDay(today - (days - 1) * DAY_MS), to: isoDay(today) };
}

export function validRange(from: string, to: string): boolean {
  const f = dayMs(from);
  const t = dayMs(to);
  return f !== null && t !== null && t >= f && t - f <= MAX_SPAN_DAYS * DAY_MS;
}

/** The range the URL asks for: a valid `from`/`to`, else a known `range` preset, else 30 days. */
export function resolveRange(
  params: { range?: string; from?: string; to?: string },
  now = new Date(),
): DayRange & { preset: number | null } {
  if (params.from && params.to && validRange(params.from, params.to)) {
    return { preset: null, from: params.from, to: params.to };
  }
  const preset = PRESETS.find((p) => String(p) === params.range) ?? DEFAULT_PRESET;
  return { preset, ...presetRange(preset, now) };
}

export interface DayValue {
  date: string;
  revenue_minor: number;
  orders: number;
}

/** Daily sales per currency with every day of the range present (days without orders are 0). */
export function dailySeries(
  rows: readonly (DayValue & { currency: string })[],
  from: string,
  to: string,
): { currency: string; days: DayValue[] }[] {
  const start = dayMs(from);
  const end = dayMs(to);
  if (start === null || end === null) return [];
  const currencies = [...new Set(rows.map((r) => r.currency))].sort();
  return currencies.map((currency) => {
    const byDay = new Map(rows.filter((r) => r.currency === currency).map((r) => [r.date, r]));
    const days: DayValue[] = [];
    for (let ms = start; ms <= end; ms += DAY_MS) {
      const date = isoDay(ms);
      const r = byDay.get(date);
      days.push({ date, revenue_minor: r?.revenue_minor ?? 0, orders: r?.orders ?? 0 });
    }
    return { currency, days };
  });
}

/** A clean axis maximum >= `v`: 1, 2, 2.5 or 5 times a power of ten. */
export function niceMax(v: number): number {
  if (v <= 0) return 1;
  const pow = 10 ** Math.floor(Math.log10(v));
  const step = [1, 2, 2.5, 5, 10].find((s) => s * pow >= v) ?? 10;
  return step * pow;
}

export type VitalRating = "good" | "needsImprovement" | "poor";

/** Core Web Vitals thresholds (p75): LCP ms, INP ms, CLS unitless. */
const THRESHOLDS: Record<string, [number, number]> = {
  LCP: [2500, 4000],
  INP: [200, 500],
  CLS: [0.1, 0.25],
};

export function vitalRating(metric: string, p75: number): VitalRating | null {
  const limits = THRESHOLDS[metric];
  if (!limits) return null;
  if (p75 <= limits[0]) return "good";
  return p75 <= limits[1] ? "needsImprovement" : "poor";
}

/** `order.created` -> group `order`, in first-seen order. */
export function groupEvents(types: readonly string[]): { group: string; events: string[] }[] {
  const groups = new Map<string, string[]>();
  for (const e of types) {
    const g = e.split(".")[0] ?? e;
    groups.set(g, [...(groups.get(g) ?? []), e]);
  }
  return [...groups].map(([group, events]) => ({ group, events }));
}
