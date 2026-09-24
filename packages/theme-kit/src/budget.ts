/** Performance budget (spec §9.6) and how a measured page is judged against it. */
export const BUDGET = {
  lcpMs: 1500,
  tbtMs: 150,
  cls: 0.05,
  /** All first-load JS incl. scroll-triggered islands (A26), gzip bytes. */
  jsGzip: { home: 35 * 1024, default: 30 * 1024 },
  thirdPartyOrigins: 0,
  axeSeriousOrCritical: 0,
  /** Storefront binding calls per page render (N+1 guard; the edge caps a render at 50). */
  maxSubrequests: 10,
} as const;

export interface PageResult {
  path: string;
  kind: "home" | "category" | "product" | "other";
  lcpMs: number;
  tbtMs: number;
  cls: number;
  jsGzip: number;
  jsTransfer: number;
  /** Worst case: analytics consent given and the 10 % RUM sample hit (web-vitals loaded). */
  jsGzipWithRum: number;
  thirdPartyOrigins: string[];
  axe: { id: string; impact: string; nodes: number }[];
  cspViolations: string[];
  /** Page-model calls of an uncached render (`x-edge-subrequests`). */
  subrequests: number;
}

export function judge(r: PageResult): string[] {
  const fails: string[] = [];
  const js = r.kind === "home" ? BUDGET.jsGzip.home : BUDGET.jsGzip.default;
  if (r.lcpMs > BUDGET.lcpMs) fails.push(`LCP ${Math.round(r.lcpMs)} ms > ${BUDGET.lcpMs}`);
  if (r.tbtMs > BUDGET.tbtMs) fails.push(`TBT ${Math.round(r.tbtMs)} ms > ${BUDGET.tbtMs}`);
  if (r.cls > BUDGET.cls) fails.push(`CLS ${r.cls.toFixed(3)} > ${BUDGET.cls}`);
  if (r.jsGzip > js) fails.push(`JS ${(r.jsGzip / 1024).toFixed(1)} kB gz > ${js / 1024}`);
  if (r.thirdPartyOrigins.length > BUDGET.thirdPartyOrigins)
    fails.push(`third-party origins: ${r.thirdPartyOrigins.join(", ")}`);
  const serious = r.axe.filter((v) => v.impact === "serious" || v.impact === "critical");
  if (serious.length > BUDGET.axeSeriousOrCritical)
    fails.push(`axe: ${serious.map((v) => v.id).join(", ")}`);
  if (r.cspViolations.length) fails.push(`CSP violations: ${r.cspViolations.length}`);
  if (r.subrequests > BUDGET.maxSubrequests)
    fails.push(`${r.subrequests} storefront calls per render > ${BUDGET.maxSubrequests}`);
  return fails;
}

export const median = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? (s[m] ?? 0) : ((s[m - 1] ?? 0) + (s[m] ?? 0)) / 2;
};
