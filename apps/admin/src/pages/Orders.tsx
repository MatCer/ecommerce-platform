import { Badge, Button, Card, Checkbox, EmptyState, linkClass, SelectField, type Tone } from "@platform/ui";
import { A, useSearchParams } from "@solidjs/router";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Show } from "solid-js";
import { BulkDocuments } from "../components/order/BulkDocuments.tsx";
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

export const orderStatusTone: Record<Schemas["OrderStatus"], Tone> = {
  pending: "neutral",
  confirmed: "info",
  processing: "info",
  shipped: "info",
  delivered: "success",
  cancelled: "neutral",
  returned: "warning",
};

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

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
  const [exception, setException] = createSignal(false);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [status, setStatus] = createSignal<Schemas["OrderStatus"]>();
  // `?customer_id=` from the customers list; anything but a UUID is ignored.
  const [params] = useSearchParams();
  const customer = () => {
    const id = params.customer_id;
    return typeof id === "string" && UUID.test(id) ? id : undefined;
  };
  const orders = createInfiniteQuery(() => ({
    queryKey: tenantKey("orders", status() ?? "", exception(), customer() ?? ""),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/orders", {
          params: {
            header: tenantHeader(),
            query: {
              status: status(),
              customer_id: customer(),
              exception: exception() || undefined,
              cursor: pageParam,
              limit: 50,
            },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => orders.data?.pages.flatMap((page) => page.items) ?? [];
  createEffect(() => {
    tenantKey("orders");
    status();
    exception();
    customer();
    setSelected(new Set<string>());
  });
  const select = (id: string, checked: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  return (
    <>
      <PageHeader title={t("orders.title")} />
      <Show when={customer()}>
        <p class="mb-4 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
          <span>{t("customers.filtered")}</span>
          <A class={linkClass} href="/orders">
            {t("customers.allOrders")}
          </A>
        </p>
      </Show>
      {/* Filter bar (Pajamas list page). */}
      <div class="mb-4 flex flex-wrap items-center gap-x-4 gap-y-2">
        <SelectField
          class="w-56"
          hideLabel
          label={t("orders.status")}
          value={status() ?? ""}
          options={[
            { value: "", label: t("orders.allStatuses") },
            ...statuses.map((value) => ({ value, label: t(`orderStatuses.${value}`) })),
          ]}
          onChange={(value) => setStatus(statuses.find((s) => s === value))}
        />
        <Checkbox
          label={t("fulfillment.exceptionOnly")}
          checked={exception()}
          onChange={setException}
        />
      </div>
      <Show keyed when={JSON.stringify(tenantKey("bulk-documents"))}>
        {(_key) => <BulkDocuments orders={rows().filter((row) => selected().has(row.id))} />}
      </Show>
      <QueryState query={orders}>
        {() => (
          <Show
            when={rows().length}
            fallback={
              <EmptyState
                icon="list-task"
                title={t("orders.empty")}
                description={t("orders.emptyDesc")}
              />
            }
          >
            <Card
              padding="none"
              footer={
                orders.hasNextPage ? (
                  <Button
                    loading={orders.isFetchingNextPage}
                    onClick={() => void orders.fetchNextPage()}
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
                    <Th class="w-10">
                      <Checkbox
                        label={t("fulfillment.selectAll")}
                        checked={rows().length > 0 && rows().every((row) => selected().has(row.id))}
                        onChange={(checked) =>
                          setSelected(new Set(checked ? rows().map((row) => row.id) : []))
                        }
                      />
                    </Th>
                    <For
                      each={[
                        t("orders.number"),
                        t("orders.date"),
                        t("orders.email"),
                        t("orders.status"),
                        t("orders.paymentStatus"),
                      ]}
                    >
                      {(label) => <Th>{label}</Th>}
                    </For>
                    <Th class="text-right">{t("orders.total")}</Th>
                    <Th>{t("orders.exception")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(order) => (
                      <tr class="hover:bg-subtle">
                        <td class={tdClass}>
                          <Checkbox
                            label={
                              <span class="sr-only">
                                {t("fulfillment.selectOrder", { number: order.number })}
                              </span>
                            }
                            checked={selected().has(order.id)}
                            onChange={(checked) => select(order.id, checked)}
                          />
                        </td>
                        <td class={tdClass}>
                          <A
                            class="font-semibold text-heading hover:text-accent-700 hover:underline"
                            href={`/orders/${order.id}`}
                          >
                            {order.number}
                          </A>
                        </td>
                        <td class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}>
                          {formatDateTime(order.placed_at)}
                        </td>
                        <td class={tdClass}>{order.email}</td>
                        <td class={tdClass}>
                          <Badge tone={orderStatusTone[order.status]}>
                            {t(`orderStatuses.${order.status}`)}
                          </Badge>
                        </td>
                        <td class={tdClass}>{t(`paymentStatuses.${order.payment_status}`)}</td>
                        <td class={`${tdClass} figures whitespace-nowrap text-right`}>
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
            </Card>
          </Show>
        )}
      </QueryState>
    </>
  );
}
