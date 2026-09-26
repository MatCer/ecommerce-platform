import { Alert, Card } from "@platform/ui";
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
/** Table title strip, in the same subtle band as the table head below it. */
const captionClass =
  "border-b border-border bg-subtle px-3 py-2 text-left text-sm font-semibold text-heading";

export function ImportReport(props: { report: Schemas["ImportReport"] }) {
  return (
    <section aria-label={t("content.report")}>
      <Card title={t("content.report")}>
        <div class="flex flex-col gap-5">
          <dl class="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <For each={COUNTS}>
              {(key) => (
                <div class="flex flex-col gap-1">
                  <dt class="text-sm text-muted-foreground">{t(`content.${key}`)}</dt>
                  <dd class="figures text-lg font-semibold text-heading">{props.report[key]}</dd>
                </div>
              )}
            </For>
          </dl>
          <Show when={props.report.truncated}>
            <Alert tone="info">{t("content.truncated")}</Alert>
          </Show>
          <div class="overflow-x-auto rounded-md border border-border">
            <table class={tableClass} aria-label={t("content.missingFields")}>
              <caption class={captionClass}>{t("content.missingFields")}</caption>
              <thead>
                <tr>
                  <Th>{t("content.field")}</Th>
                  <Th class="text-right">{t("content.count")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={Object.entries(props.report.missing)}>
                  {([field, count]) => (
                    <tr>
                      <td class={`${tdClass} font-mono text-xs`}>{field}</td>
                      <td class={`${tdClass} figures text-right`}>{count}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
          <div class="overflow-x-auto rounded-md border border-border">
            <table class={tableClass} aria-label={t("content.problems")}>
              <caption class={captionClass}>{t("content.problems")}</caption>
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
                      <td class={`${tdClass} font-mono text-xs`}>{p.item_id}</td>
                      <td class={`${tdClass} font-mono text-xs`}>{p.code}</td>
                      <td class={tdClass}>{p.detail}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
          <div class="overflow-x-auto rounded-md border border-border">
            <table class={tableClass} aria-label={t("content.collisions")}>
              <caption class={captionClass}>{t("content.collisions")}</caption>
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
                      <td class={`${tdClass} font-mono text-xs`}>{c.item_id}</td>
                      <td class={tdClass}>{c.kind}</td>
                      <td class={tdClass}>{c.value}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </div>
      </Card>
    </section>
  );
}
