import { formatNumber, formatPercent, formatVital, THRESHOLDS } from "./analytics.ts";
import type { Schemas } from "./api.ts";

type Detection = Schemas["Detection"];

export function detectionParams(d: Detection, locale: string): Record<string, string> {
  const num = (n: number) => formatNumber(n, locale);
  const rate = (part: number, whole: number) => formatPercent(whole > 0 ? part / whole : 0, locale);
  switch (d.kind) {
    case "funnel_drop":
      return {
        recent: rate(d.recent.to, d.recent.from),
        baseline: rate(d.baseline.to, d.baseline.from),
        recentTo: num(d.recent.to),
        recentFrom: num(d.recent.from),
        baselineTo: num(d.baseline.to),
        baselineFrom: num(d.baseline.from),
      };
    case "low_add_to_cart":
      return {
        rate: rate(d.adds, d.views),
        shopRate: rate(d.shop_adds, d.shop_views),
        adds: num(d.adds),
        views: num(d.views),
      };
    case "slow_page":
      return {
        metric: d.metric,
        p75: formatVital(d.metric, d.p75, locale),
        bound: formatVital(d.metric, THRESHOLDS[d.metric]?.[0] ?? 0, locale),
        samples: num(d.samples),
      };
    case "zero_result_search":
      return { searches: num(d.searches) };
  }
}

export function measureText(
  d: Detection,
  m: Schemas["InsightMeasure"],
  locale: string,
  t: (
    key: "insights.change.noData" | `insights.change.${Detection["kind"]}`,
    params?: Record<string, string>,
  ) => string,
): string {
  if (m.value == null) return t("insights.change.noData");
  const value =
    d.kind === "slow_page"
      ? formatVital(d.metric, m.value, locale)
      : d.kind === "zero_result_search"
        ? formatNumber(m.value, locale)
        : formatPercent(m.value, locale);
  return t(`insights.change.${d.kind}`, { value, samples: formatNumber(m.samples, locale) });
}
