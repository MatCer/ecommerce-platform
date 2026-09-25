import { Button, EmptyState, TextField } from "@platform/ui";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, locale, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";

/** Imported historical orders (WP13b, A28): a read-only archive. */
export default function ArchivedOrders() {
  const [q, setQ] = createSignal("");
  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("archived-orders", q().trim()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/archived-orders", {
          params: {
            header: tenantHeader(),
            query: { q: q().trim() || undefined, limit: 50, cursor: pageParam },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <>
      <PageHeader title={t("data.archive")} />
      <p class="mb-4 max-w-2xl text-sm text-muted-foreground">{t("data.archiveHint")}</p>
      <div class="mb-4 max-w-sm">
        <TextField type="search" label={t("data.archiveSearch")} value={q()} onChange={setQ} />
      </div>
      <QueryState query={list}>
        {() => (
          <Show when={rows().length} fallback={<EmptyState title={t("data.archiveEmpty")} />}>
            <div class="overflow-x-auto">
              <table class={tableClass} aria-label={t("data.archive")}>
                <thead>
                  <tr>
                    <Th>{t("data.number")}</Th>
                    <Th>{t("data.placed")}</Th>
                    <Th>{t("data.customer")}</Th>
                    <Th>{t("data.oldStatus")}</Th>
                    <Th>{t("data.items")}</Th>
                    <Th class="text-right">{t("data.total")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(o) => (
                      <tr>
                        <td class={`${tdClass} figures`}>{o.number}</td>
                        <td class={tdClass}>{formatDateTime(o.placed_at)}</td>
                        <td class={tdClass}>
                          {o.name ? `${o.name} · ` : ""}
                          {o.email}
                        </td>
                        <td class={tdClass}>{o.status_label ?? "—"}</td>
                        <td class={`${tdClass} figures`}>
                          {Array.isArray(o.lines) ? o.lines.length : 0}
                        </td>
                        <td class={`${tdClass} figures text-right`}>
                          {formatMoney(o.total_minor, o.currency, locale())}
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={list.hasNextPage}>
              <div class="mt-4">
                <Button loading={list.isFetchingNextPage} onClick={() => void list.fetchNextPage()}>
                  {t("data.more")}
                </Button>
              </div>
            </Show>
          </Show>
        )}
      </QueryState>
    </>
  );
}
