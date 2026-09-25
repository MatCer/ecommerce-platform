import { Button, EmptyState, PermissionDenied } from "@platform/ui";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { claims } from "../lib/session.ts";

export default function AuditLog() {
  const { can } = useMembership();
  const log = createInfiniteQuery(() => ({
    queryKey: tenantKey("audit-log"),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/audit-log", {
          params: { header: tenantHeader(), query: { limit: 50, cursor: pageParam } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    enabled: can("admin"),
  }));
  const rows = () => log.data?.pages.flatMap((p) => p.items) ?? [];
  const actor = (a: string) =>
    a === "platform" ? t("audit.platform") : a === claims()?.sub ? t("audit.you") : a;

  return (
    <>
      <PageHeader title={t("audit.title")} />
      <Show
        when={can("admin")}
        fallback={
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        }
      >
        <QueryState query={log}>
          {() => (
            <Show
              when={rows().length > 0}
              fallback={
                <EmptyState title={t("audit.emptyTitle")} description={t("audit.emptyDesc")} />
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("audit.time")}</Th>
                      <Th>{t("audit.action")}</Th>
                      <Th>{t("audit.entity")}</Th>
                      <Th>{t("audit.actor")}</Th>
                      <Th>{t("audit.changes")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(e) => (
                        <tr class="align-top">
                          <td
                            class={`${tdClass} figures text-xs whitespace-nowrap text-faint-foreground`}
                          >
                            {formatDateTime(e.at)}
                          </td>
                          <td class={`${tdClass} figures text-xs font-medium`}>{e.action}</td>
                          <td class={`${tdClass} text-xs`}>
                            {e.entity}
                            <Show when={e.entity_id}>
                              <span
                                class="figures block max-w-48 truncate text-faint-foreground"
                                title={e.entity_id ?? ""}
                              >
                                {e.entity_id}
                              </span>
                            </Show>
                          </td>
                          <td class={`${tdClass} max-w-40 truncate text-xs`} title={e.actor}>
                            {actor(e.actor)}
                          </td>
                          <td class={`${tdClass} py-1`}>
                            <details>
                              <summary class="cursor-pointer text-xs text-accent-700">
                                {t("audit.changes")}
                              </summary>
                              <pre class="figures mt-1 max-h-64 max-w-xl overflow-auto rounded-sm bg-muted p-2 text-xs">
                                {JSON.stringify(e.diff, null, 2)}
                              </pre>
                            </details>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
              <Show when={log.hasNextPage}>
                <div class="mt-3">
                  <Button loading={log.isFetchingNextPage} onClick={() => void log.fetchNextPage()}>
                    {t("common.loadMore")}
                  </Button>
                </div>
              </Show>
            </Show>
          )}
        </QueryState>
      </Show>
    </>
  );
}
