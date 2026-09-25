/**
 * Core Web Vitals for RUM (spec §9.6) straight from PerformanceObserver: LCP, CLS and INP,
 * reported once when the page is hidden. It replaces the `web-vitals` library (~3 kB gz) with
 * well under 1 kB, which the product page's JS budget needs (A26 counts the sampled visit too).
 * Chromium-based browsers report all three; others report what they support. The definitions
 * follow web.dev: CLS = largest session window, INP ≈ p98 of interaction latencies.
 */

export type MetricName = "LCP" | "CLS" | "INP";
export interface Metric {
  name: MetricName;
  value: number;
  rating: "good" | "needs-improvement" | "poor";
}

const THRESHOLDS: Record<MetricName, [number, number]> = {
  LCP: [2500, 4000],
  CLS: [0.1, 0.25],
  INP: [200, 500],
};

export const metric = (name: MetricName, value: number): Metric => {
  const [good, poor] = THRESHOLDS[name];
  return {
    name,
    value,
    rating: value <= good ? "good" : value <= poor ? "needs-improvement" : "poor",
  };
};

interface Shift {
  value: number;
  startTime: number;
  hadRecentInput: boolean;
}

/** CLS: the largest session window (shifts < 1 s apart, window ≤ 5 s), input-driven excluded. */
export function clsOf(shifts: readonly Shift[]): number {
  let max = 0;
  let current = 0;
  let first = 0;
  let last = 0;
  for (const s of shifts) {
    if (s.hadRecentInput) continue;
    if (current && s.startTime - last < 1000 && s.startTime - first < 5000) current += s.value;
    else {
      current = s.value;
      first = s.startTime;
    }
    last = s.startTime;
    max = Math.max(max, current);
  }
  return max;
}

/** INP: the slowest interaction, skipping one per 50 interactions (≈ the 98th percentile). */
export function inpOf(latencies: readonly number[]): number {
  const slowest = [...latencies].sort((a, b) => b - a);
  return slowest[Math.min(slowest.length - 1, Math.floor(slowest.length / 50))] ?? 0;
}

/** Observes the page and calls `done` once, with every metric it could measure, on hide. */
export function observeVitals(done: (metrics: Metric[]) => void) {
  let lcp = 0;
  let inputSeen = false;
  const shifts: Shift[] = [];
  const interactions = new Map<number, number>();
  const observe = (type: string, onEntry: (e: PerformanceEntry) => void, extra = {}) => {
    try {
      new PerformanceObserver((list) => list.getEntries().forEach(onEntry)).observe({
        type,
        buffered: true,
        ...extra,
      });
    } catch {
      /* entry type not supported by this browser */
    }
  };
  // LCP stops at the first input, like the browser's own reporting.
  for (const t of ["keydown", "pointerdown"])
    addEventListener(t, () => (inputSeen = true), { once: true, capture: true });
  observe("largest-contentful-paint", (e) => {
    if (!inputSeen) lcp = e.startTime;
  });
  observe("layout-shift", (e) => shifts.push(e as unknown as Shift));
  observe(
    "event",
    (e) => {
      // `interactionId` (Event Timing Level 1) is not in TypeScript's DOM lib yet.
      const id = (e as PerformanceEntry & { interactionId?: number }).interactionId;
      if (id) interactions.set(id, Math.max(interactions.get(id) ?? 0, e.duration));
    },
    { durationThreshold: 40 },
  );

  let sent = false;
  const flush = () => {
    if (sent) return;
    sent = true;
    const out = [metric("CLS", clsOf(shifts))];
    if (lcp) out.unshift(metric("LCP", lcp));
    if (interactions.size) out.push(metric("INP", inpOf([...interactions.values()])));
    done(out);
  };
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && flush());
  addEventListener("pagehide", flush);
}
