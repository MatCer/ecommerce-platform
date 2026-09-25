import { Badge, Button, EmptyState, SelectField } from "@platform/ui";
import { A } from "@solidjs/router";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";

const statuses: Schemas["OrderStatus"][] = [
  "pending",
  "confirmed",
  "processing",
  "shipped",
  "delivered",
  "cancelled",
  "returned",
];

export function OrderException(props: { exception?: string | null }) {
  return (
    <Show when={props.exception}>
      <Badge tone="warning">
        {props.exception === "late_payment" ? t("orders.latePayment") : props.exception}
      </Badge>
    </Show>
  );
}

export default function Orders() {
  const [status, setStatus] = createSignal<Schemas["OrderStatus"]>();
  const orders = createInfiniteQuery(() => ({
    queryKey: tenantKey("orders", status() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/orders", {
          params: {
            header: tenantHeader(),
            query: { status: status(), cursor: pageParam, limit: 50 },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => orders.data?.pages.flatMap((page) => page.items) ?? [];
  return (
    <>
      <PageHeader title={t("orders.title")} />
      <div class="mb-4 max-w-xs">
        <SelectField
          label={t("orders.status")}
          value={status() ?? ""}
          options={[
            { value: "", label: t("orders.allStatuses") },
            ...statuses.map((value) => ({ value, label: t(`orderStatuses.${value}`) })),
          ]}
          onChange={(value) => setStatus(statuses.find((s) => s === value))}
        />
      </div>
      <QueryState query={orders}>
        {() => (
          <Show
            when={rows().length}
            fallback={<EmptyState title={t("orders.empty")} description={t("orders.emptyDesc")} />}
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <For
                      each={[
                        t("orders.number"),
                        t("orders.date"),
                        t("orders.email"),
                        t("orders.status"),
                        t("orders.paymentStatus"),
                        t("orders.total"),
                        t("orders.exception"),
                      ]}
                    >
                      {(label) => <Th>{label}</Th>}
                    </For>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(order) => (
                      <tr>
                        <td class={tdClass}>
                          <A
                            class="font-medium text-accent-700 hover:underline"
                            href={`/orders/${order.id}`}
                          >
                            {order.number}
                          </A>
                        </td>
                        <td class={`${tdClass} whitespace-nowrap`}>
                          {formatDateTime(order.placed_at)}
                        </td>
                        <td class={tdClass}>{order.email}</td>
                        <td class={tdClass}>{t(`orderStatuses.${order.status}`)}</td>
                        <td class={tdClass}>{t(`paymentStatuses.${order.payment_status}`)}</td>
                        <td class={`${tdClass} figures whitespace-nowrap`}>
                          {order.total.formatted}
                        </td>
                        <td class={tdClass}>
                          <OrderException exception={order.exception} />
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={orders.hasNextPage}>
              <Button
                class="mt-4"
                loading={orders.isFetchingNextPage}
                onClick={() => orders.fetchNextPage()}
              >
                {t("common.loadMore")}
              </Button>
            </Show>
          </Show>
        )}
      </QueryState>
    </>
  );
}
