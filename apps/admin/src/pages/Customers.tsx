import { Avatar, Badge, Button, Card, EmptyState, linkClass, SearchBox } from "@platform/ui";
import { A } from "@solidjs/router";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

/**
 * Customer accounts (read-only): search by email or name, newest first. A customer's orders
 * open in the orders list filtered by the account; access/erasure requests stay in Data →
 * Privacy (owner/admin).
 */
export default function Customers() {
  const [q, setQ] = createSignal("");
  const query = () => q().trim() || undefined;
  const list = createInfiniteQuery(() => ({
    queryKey: tenantKey("customers", query() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/customers", {
          params: { header: tenantHeader(), query: { q: query(), limit: 50, cursor: pageParam } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => list.data?.pages.flatMap((p) => p.items) ?? [];

  return (
    <>
      <PageHeader title={t("customers.title")} description={t("customers.desc")} />
      <div class="mb-4 flex flex-wrap items-center gap-2">
        <SearchBox
          class="min-w-60 flex-1"
          label={t("customers.search")}
          placeholder={t("customers.search")}
          clearLabel={t("common.clearSearch")}
          value={q()}
          onChange={setQ}
        />
      </div>
      <QueryState query={list}>
        {(data) => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                icon={query() ? "search" : "user"}
                title={t("customers.empty")}
                description={t("customers.emptyDesc")}
              />
            }
          >
            <Card
              padding="none"
              footer={
                list.hasNextPage ? (
                  <Button
                    loading={list.isFetchingNextPage}
                    onClick={() => void list.fetchNextPage()}
                  >
                    {t("common.loadMore")}
                  </Button>
                ) : undefined
              }
            >
              <p
                role="status"
                class="border-b border-border bg-subtle px-3 py-2 text-sm text-muted-foreground"
              >
                {t("customers.total", { n: String(data.pages[0]?.total ?? 0) })}
              </p>
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("customers.email")}</Th>
                      <Th>{t("customers.name")}</Th>
                      <Th>{t("customers.phone")}</Th>
                      <Th>{t("customers.account")}</Th>
                      <Th class="text-right">{t("customers.orders")}</Th>
                      <Th class="text-right">{t("customers.since")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(c) => (
                        <tr>
                          <td class={tdClass}>
                            <span class="flex items-center gap-2">
                              <Avatar name={c.name ?? c.email} size={24} />
                              <span class="font-semibold text-heading">{c.email}</span>
                            </span>
                          </td>
                          <td class={tdClass}>{c.name ?? "—"}</td>
                          <td class={`${tdClass} figures whitespace-nowrap`}>{c.phone ?? "—"}</td>
                          <td class={tdClass}>
                            <Badge tone={c.has_password ? "success" : "neutral"}>
                              {c.has_password ? t("customers.password") : t("customers.linkOnly")}
                            </Badge>
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            <Show when={c.orders > 0} fallback="0">
                              <A
                                class={`font-semibold ${linkClass}`}
                                href={`/orders?customer_id=${c.id}`}
                              >
                                {c.orders}
                                <span class="sr-only">
                                  {" "}
                                  {t("customers.ordersOf", { email: c.email })}
                                </span>
                              </A>
                            </Show>
                          </td>
                          <td
                            class={`${tdClass} figures text-right whitespace-nowrap text-muted-foreground`}
                          >
                            {formatDateTime(c.created_at)}
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
    </>
  );
}
