import { Button, Card, Collapse, EmptyState, PermissionDenied } from "@platform/ui";
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
                <EmptyState
                  icon="list-task"
                  title={t("audit.emptyTitle")}
                  description={t("audit.emptyDesc")}
                />
              }
            >
              <Card
                padding="none"
                footer={
                  log.hasNextPage ? (
                    <Button
                      loading={log.isFetchingNextPage}
                      onClick={() => void log.fetchNextPage()}
                    >
                      {t("common.loadMore")}
                    </Button>
                  ) : undefined
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
                              class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}
                            >
                              {formatDateTime(e.at)}
                            </td>
                            <td class={`${tdClass} font-mono text-xs font-semibold text-heading`}>
                              {e.action}
                            </td>
                            <td class={tdClass}>
                              {e.entity}
                              <Show when={e.entity_id}>
                                <span
                                  class="block max-w-48 truncate font-mono text-xs text-muted-foreground"
                                  title={e.entity_id ?? ""}
                                >
                                  {e.entity_id}
                                </span>
                              </Show>
                            </td>
                            <td class={`${tdClass} max-w-40 truncate`} title={e.actor}>
                              {actor(e.actor)}
                            </td>
                            <td class={`${tdClass} py-1`}>
                              <Collapse summary={t("audit.changes")}>
                                <pre class="max-h-64 max-w-xl overflow-auto rounded-md border border-border bg-subtle p-2 font-mono text-xs">
                                  {JSON.stringify(e.diff, null, 2)}
                                </pre>
                              </Collapse>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Card>
            </Show>
          )}
        </QueryState>
      </Show>
    </>
  );
}
