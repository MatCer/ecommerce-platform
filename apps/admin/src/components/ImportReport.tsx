import { For, Show } from "solid-js";
import { t } from "../i18n/index.ts";
import type { Schemas } from "../lib/api.ts";
import { Th, tableClass, tdClass } from "./Page.tsx";

const COUNTS = [
  "items",
  "new_products",
  "updated_products",
  "variants",
  "new_categories",
  "new_parameters",
  "new_images",
  "redirects",
  "skipped_items",
] as const;
export function ImportReport(props: { report: Schemas["ImportReport"] }) {
  return (
    <section aria-label={t("content.report")} class="flex flex-col gap-4">
      <h2 class="font-semibold">{t("content.report")}</h2>
      <dl class="grid grid-cols-2 gap-3 sm:grid-cols-3">
        <For each={COUNTS}>
          {(key) => (
            <div>
              <dt class="text-xs text-muted-foreground">{t(`content.${key}`)}</dt>
              <dd class="figures text-lg">{props.report[key]}</dd>
            </div>
          )}
        </For>
      </dl>
      <Show when={props.report.truncated}>
        <p role="status" class="text-sm">
          {t("content.truncated")}
        </p>
      </Show>
      <div class="overflow-x-auto">
        <table class={tableClass} aria-label={t("content.missingFields")}>
          <caption class="py-2 text-left font-medium">{t("content.missingFields")}</caption>
          <thead>
            <tr>
              <Th>{t("content.field")}</Th>
              <Th>{t("content.count")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={Object.entries(props.report.missing)}>
              {([field, count]) => (
                <tr>
                  <td class={tdClass}>{field}</td>
                  <td class={tdClass}>{count}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
      <div class="overflow-x-auto">
        <table class={tableClass} aria-label={t("content.problems")}>
          <caption class="py-2 text-left font-medium">{t("content.problems")}</caption>
          <thead>
            <tr>
              <Th>{t("content.itemId")}</Th>
              <Th>{t("content.code")}</Th>
              <Th>{t("content.detail")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={props.report.problems}>
              {(p) => (
                <tr>
                  <td class={tdClass}>{p.item_id}</td>
                  <td class={tdClass}>{p.code}</td>
                  <td class={tdClass}>{p.detail}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
      <div class="overflow-x-auto">
        <table class={tableClass} aria-label={t("content.collisions")}>
          <caption class="py-2 text-left font-medium">{t("content.collisions")}</caption>
          <thead>
            <tr>
              <Th>{t("content.itemId")}</Th>
              <Th>{t("content.kind")}</Th>
              <Th>{t("content.detail")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={props.report.collisions}>
              {(c) => (
                <tr>
                  <td class={tdClass}>{c.item_id}</td>
                  <td class={tdClass}>{c.kind}</td>
                  <td class={tdClass}>{c.value}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </section>
  );
}
