import { Badge } from "@platform/ui";
import { For, Show } from "solid-js";
import { t } from "../i18n/index.ts";
import type { Schemas } from "../lib/api.ts";
import { Th, tableClass, tdClass } from "./Page.tsx";

type Explain = Schemas["RecommendationExplain"];

/** Scores differ per strategy (orders together, decayed units, position): two decimals. */
const score = (n: number) => (Number.isInteger(n) ? String(n) : n.toFixed(2));

/**
 * "Why recommended" (WP17): the strategies tried, the products with the strategy and score
 * that picked them, and every candidate left out with the reason.
 */
export function ExplainResult(props: { data: Explain }) {
  const r = () => props.data.result;
  return (
    <div class="flex flex-col gap-4">
      <p class="text-sm text-muted-foreground">
        {t("recommendations.chain")}:{" "}
        {r()
          .chain.map((s) => t(`recommendations.strategy_${s}`))
          .join(" → ") || "—"}
      </p>
      <Show when={props.data.affinity?.scores.length}>
        <p class="text-sm text-muted-foreground">
          {t("recommendations.affinity")}:{" "}
          {(props.data.affinity?.scores ?? [])
            .slice(0, 6)
            .map((a) => `${a.key} (${score(a.score)})`)
            .join(", ")}
        </p>
      </Show>
      <Show
        when={r().items.length > 0}
        fallback={<p class="text-sm">{t("recommendations.noResults")}</p>}
      >
        <div class="overflow-x-auto">
          <table class={tableClass}>
            <caption class="sr-only">{t("recommendations.results")}</caption>
            <thead>
              <tr>
                <Th>{t("recommendations.product")}</Th>
                <Th class="text-right">{t("recommendations.price")}</Th>
                <Th>{t("recommendations.strategy")}</Th>
                <Th class="text-right">{t("recommendations.score")}</Th>
              </tr>
            </thead>
            <tbody>
              <For each={r().items}>
                {(i) => (
                  <tr>
                    <td class={`${tdClass} font-medium`}>{i.product.name}</td>
                    <td class={`${tdClass} figures text-right`}>{i.product.price.formatted}</td>
                    <td class={tdClass}>
                      <Badge tone="info">{t(`recommendations.strategy_${i.strategy}`)}</Badge>
                    </td>
                    <td class={`${tdClass} figures text-right`}>{score(i.score)}</td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
      <Show when={r().skipped.length > 0}>
        <details>
          <summary class="cursor-pointer text-sm font-medium">
            {t("recommendations.skipped", { n: String(r().skipped.length) })}
          </summary>
          <ul class="mt-2 flex flex-col gap-1 text-sm">
            <For each={r().skipped}>
              {(s) => (
                <li class="flex flex-wrap items-center gap-2">
                  <span>{props.data.names[s.product_id] ?? s.product_id}</span>
                  <Badge tone="neutral">{t(`recommendations.reason_${s.reason}`)}</Badge>
                  <span class="text-xs text-muted-foreground">
                    {t(`recommendations.strategy_${s.strategy}`)}
                  </span>
                </li>
              )}
            </For>
          </ul>
        </details>
      </Show>
    </div>
  );
}
