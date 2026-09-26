import { Badge, Button, SelectField, showToast, TextField } from "@platform/ui";
import { useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { ApiProblem } from "../components/CheckoutSettings.tsx";
import { OrderActions } from "../components/order/OrderActions.tsx";
import {
  EditShippingAddress,
  OrderNote,
  OrderSections,
  TimelineEvent,
} from "../components/order/OrderSections.tsx";
import { RefundException } from "../components/order/RefundException.tsx";
import { Section } from "../components/order/shared.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, locale, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";
import { OrderException } from "./Orders.tsx";
import { ResolveOrderException } from "./PaymentExceptions.tsx";

function Address(props: { address?: Schemas["OrderAddress"] | null }) {
  return (
    <Show when={props.address} fallback={<p>{t("common.none")}</p>}>
      {(address) => (
        <address class="text-sm not-italic">
          <p>{address().name}</p>
          <Show when={address().company}>
            <p>{address().company}</p>
          </Show>
          <p>{address().street}</p>
          <p>
            {address().postal_code} {address().city}
          </p>
          <p>{address().country}</p>
          <Show when={address().phone}>
            <p>{address().phone}</p>
          </Show>
        </address>
      )}
    </Show>
  );
}
export default function OrderDetail() {
  const params = useParams();
  const query = createQuery(() => ({
    queryKey: tenantKey("order", params.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/orders/{id}", {
          params: { header: tenantHeader(), path: { id: params.id ?? "" } },
        }),
      ),
  }));
  return (
    <Show
      when={!query.isError && query.data}
      fallback={<QueryState query={query}>{() => null}</QueryState>}
    >
      {(data) => (
        <Show keyed when={JSON.stringify(tenantKey("order-view", data().order.id))}>
          {(_identity) => (
            <>
              <PageHeader
                title={data().order.number}
                description={formatDateTime(data().order.placed_at)}
                back={{ href: "/orders", label: t("orders.title") }}
                actions={
                  <Button loading={query.isFetching} onClick={() => void query.refetch()}>
                    {t("fulfillment.refresh")}
                  </Button>
                }
              />
              <div class="mb-4 flex flex-wrap items-center gap-3 text-sm">
                <Badge>
                  {t("orders.status")}: {t(`orderStatuses.${data().order.status}`)}
                </Badge>
                <Badge>
                  {t("orders.paymentStatus")}: {t(`paymentStatuses.${data().order.payment.status}`)}
                </Badge>
                <Badge>
                  {t("orders.fulfillment")}:{" "}
                  {t(`fulfillmentStatuses.${data().order.fulfillment_status}`)}
                </Badge>
                <OrderException exception={data().order.exception} />
                <Show when={data().order.exception && !data().exception_resolved_at}>
                  <ResolveOrderException orderId={data().order.id} />
                  <RefundException orderId={data().order.id} />
                </Show>
                <Show when={data().exception_resolved_at}>
                  {(at) => (
                    <span class="text-muted-foreground">
                      {t("pay.resolved")} {formatDateTime(at())}: {data().exception_note}
                    </span>
                  )}
                </Show>
              </div>
              <OrderActions data={data()} />
              <div class="flex flex-col gap-4">
                <div class="grid items-start gap-4 xl:grid-cols-[minmax(0,1fr)_20rem]">
                  <div class="flex min-w-0 flex-col gap-4">
                    <OrderSections data={data()} />
                    <Section title={t("orders.lines")} padding="none">
                      <div class="overflow-x-auto">
                        <table class={tableClass}>
                          <thead>
                            <tr>
                              <For
                                each={[
                                  t("orders.product"),
                                  t("orders.sku"),
                                  t("orders.quantity"),
                                  t("orders.unitPrice"),
                                  t("orders.discount"),
                                  t("orders.taxRate"),
                                  t("orders.total"),
                                ]}
                              >
                                {(label) => <Th>{label}</Th>}
                              </For>
                            </tr>
                          </thead>
                          <tbody>
                            <For each={data().order.lines}>
                              {(line) => (
                                <tr>
                                  <td class={tdClass}>
                                    {line.name}
                                    <p class="text-xs text-muted-foreground">
                                      {line.options_label}
                                    </p>
                                  </td>
                                  <td class={tdClass}>{line.sku}</td>
                                  <td class={tdClass}>{line.quantity}</td>
                                  <td class={tdClass}>{line.unit_price.formatted}</td>
                                  <td class={tdClass}>{line.discount.formatted}</td>
                                  <td class={tdClass}>{line.tax_rate} %</td>
                                  <td class={tdClass}>{line.total.formatted}</td>
                                </tr>
                              )}
                            </For>
                          </tbody>
                        </table>
                      </div>
                    </Section>
                    <div class="grid gap-4 2xl:grid-cols-2">
                      <Section title={t("orders.totals")}>
                        <dl class="grid grid-cols-2 gap-x-4 gap-y-2 text-sm [&_dt]:text-muted-foreground">
                          <For
                            each={
                              [
                                "subtotal",
                                "discount",
                                "shipping_total",
                                "payment_fee",
                                "rounding",
                                "vat_total",
                                "total",
                              ] as const
                            }
                          >
                            {(key) => (
                              <>
                                <dt>{t(`orderTotals.${key}`)}</dt>
                                <dd class="figures text-right">{data().order[key].formatted}</dd>
                              </>
                            )}
                          </For>
                          <Show when={data().order.coupon_code}>
                            <dt>{t("orders.coupon")}</dt>
                            <dd class="text-right">{data().order.coupon_code}</dd>
                          </Show>
                        </dl>
                      </Section>
                      <Section title={t("orders.vatRecap")} padding="none">
                        <div class="overflow-x-auto">
                          <table class={tableClass}>
                            <thead>
                              <tr>
                                <Th>{t("orders.taxRate")}</Th>
                                <Th>{t("orders.net")}</Th>
                                <Th>{t("orders.vat")}</Th>
                                <Th>{t("orders.gross")}</Th>
                              </tr>
                            </thead>
                            <tbody>
                              <For each={data().order.vat}>
                                {(row) => (
                                  <tr>
                                    <td class={tdClass}>{row.rate} %</td>
                                    <td class={tdClass}>{row.net.formatted}</td>
                                    <td class={tdClass}>{row.vat.formatted}</td>
                                    <td class={tdClass}>{row.gross.formatted}</td>
                                  </tr>
                                )}
                              </For>
                            </tbody>
                          </table>
                        </div>
                      </Section>
                    </div>
                    <Show when={data().order.charges.length}>
                      <Section title={t("orders.charges")} padding="none">
                        <div class="overflow-x-auto">
                          <table class={tableClass}>
                            <thead>
                              <tr>
                                <Th>{t("orders.kind")}</Th>
                                <Th>{t("orders.vat")}</Th>
                                <Th>{t("orders.total")}</Th>
                              </tr>
                            </thead>
                            <tbody>
                              <For each={data().order.charges}>
                                {(charge) => (
                                  <tr>
                                    <td class={tdClass}>{t(`chargeKinds.${charge.kind}`)}</td>
                                    <td class={tdClass}>{charge.tax.formatted}</td>
                                    <td class={tdClass}>{charge.total.formatted}</td>
                                  </tr>
                                )}
                              </For>
                            </tbody>
                          </table>
                        </div>
                      </Section>
                    </Show>
                  </div>
                  <aside class="flex min-w-0 flex-col gap-4">
                    <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-1">
                      <Section title={t("orders.customer")}>
                        <div class="break-words text-sm">
                          <p>{data().order.email}</p>
                          <p>{data().order.phone}</p>
                          <Show when={data().customer_id}>
                            <p>
                              {t("orders.customerId")}: {data().customer_id}
                            </p>
                          </Show>
                          <p>
                            {t("checkout.market")}: {data().market_id}
                          </p>
                          <p>
                            {t("orders.country")}: {data().ship_to_country}
                          </p>
                        </div>
                      </Section>
                      <Section title={t("orders.billingAddress")}>
                        <Address address={data().order.billing_address} />
                      </Section>
                      <Section title={t("orders.shippingAddress")}>
                        <Address address={data().order.shipping_address} />
                        <EditShippingAddress data={data()} />
                      </Section>
                    </div>
                    <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-1">
                      <Section title={t("shipping.title")}>
                        <div class="text-sm">
                          <p>{data().order.shipping.name}</p>
                          <p>{t(`carriers.${data().order.shipping.carrier}`)}</p>
                          <Show when={data().order.shipping.pickup_point}>
                            {(point) => (
                              <div class="mt-3">
                                <h3 class="font-medium">{t("orders.pickupPoint")}</h3>
                                <p>
                                  {point().name} ({point().id})
                                </p>
                                <p>{point().street}</p>
                                <p>
                                  {point().zip} {point().city}
                                </p>
                                <p>{point().country}</p>
                              </div>
                            )}
                          </Show>
                        </div>
                      </Section>
                      <Section title={t("payments.title")}>
                        <dl class="grid grid-cols-2 gap-2 text-sm">
                          <dt>{t("orders.method")}</dt>
                          <dd>{t(`paymentKinds.${data().order.payment.method}`)}</dd>
                          <dt>{t("orders.paymentStatus")}</dt>
                          <dd>{t(`paymentStatuses.${data().order.payment.status}`)}</dd>
                          <dt>{t("orders.canRetry")}</dt>
                          <dd>
                            {data().order.payment.can_retry ? t("common.yes") : t("common.no")}
                          </dd>
                          <Show when={data().order.payment.expires_at}>
                            {(expires) => (
                              <>
                                <dt>{t("orders.expires")}</dt>
                                <dd>{formatDateTime(expires())}</dd>
                              </>
                            )}
                          </Show>
                          <Show when={data().order.payment.attempt}>
                            {(attempt) => (
                              <>
                                <dt>{t("orders.currentAttempt")}</dt>
                                <dd class="break-all">
                                  {attempt().id} · {t(`attemptStatuses.${attempt().status}`)} ·{" "}
                                  {formatDateTime(attempt().created_at)}
                                </dd>
                              </>
                            )}
                          </Show>
                        </dl>
                      </Section>
                    </div>
                  </aside>
                </div>
                <Show when={data().attempts.find((a) => a.method === "cod")}>
                  {(a) => <CodPanel orderId={data().order.id} attempt={a()} />}
                </Show>
                <Section title={t("orders.attempts")} padding="none">
                  <Show
                    when={data().attempts.length}
                    fallback={<p class="p-4 text-sm">{t("orders.noAttempts")}</p>}
                  >
                    <div class="overflow-x-auto">
                      <table class={tableClass}>
                        <thead>
                          <tr>
                            <For
                              each={[
                                t("orders.attempt"),
                                t("orders.method"),
                                t("orders.status"),
                                t("orders.total"),
                                t("orders.date"),
                                t("orders.expires"),
                                t("orders.completed"),
                                t("orders.providerRef"),
                              ]}
                            >
                              {(label) => <Th>{label}</Th>}
                            </For>
                          </tr>
                        </thead>
                        <tbody>
                          <For each={data().attempts}>
                            {(attempt) => (
                              <tr>
                                <td class={`${tdClass} font-mono text-xs break-all`}>
                                  {attempt.id}
                                </td>
                                <td class={tdClass}>{t(`paymentKinds.${attempt.method}`)}</td>
                                <td class={tdClass}>{t(`attemptStatuses.${attempt.status}`)}</td>
                                <td class={tdClass}>
                                  {formatMoney(attempt.amount_minor, attempt.currency, locale())}
                                </td>
                                <td class={tdClass}>{formatDateTime(attempt.created_at)}</td>
                                <td class={tdClass}>
                                  {attempt.expires_at
                                    ? formatDateTime(attempt.expires_at)
                                    : t("common.none")}
                                </td>
                                <td class={tdClass}>
                                  {attempt.completed_at
                                    ? formatDateTime(attempt.completed_at)
                                    : t("common.none")}
                                </td>
                                <td class={`${tdClass} break-all`}>
                                  {attempt.provider_ref ?? t("common.none")}
                                </td>
                              </tr>
                            )}
                          </For>
                        </tbody>
                      </table>
                    </div>
                  </Show>
                </Section>
                <Section title={t("orders.events")}>
                  <Show
                    when={data().events.length}
                    fallback={<p class="text-sm">{t("orders.noEvents")}</p>}
                  >
                    <ol class="flex flex-col gap-3">
                      <For each={data().events}>{(event) => <TimelineEvent event={event} />}</For>
                    </ol>
                  </Show>
                </Section>
                <Section title={t("orders.notes")}>
                  <Show when={data().order.notes}>
                    <p class="whitespace-pre-wrap break-words text-sm">{data().order.notes}</p>
                  </Show>
                  <OrderNote orderId={data().order.id} />
                </Section>
              </div>
            </>
          )}
        </Show>
      )}
    </Show>
  );
}

const tenders = ["cash", "card"] as const;
const collectors = ["carrier", "merchant"] as const;

/** Cash on delivery (A16): delivered → collected (tender, collector, cash rounding) → remitted. */
function CodPanel(props: { orderId: string; attempt: Schemas["Attempt"] }) {
  const qc = useQueryClient();
  const [tender, setTender] = createSignal<(typeof tenders)[number]>("cash");
  const [collector, setCollector] = createSignal<(typeof collectors)[number]>("carrier");
  const [note, setNote] = createSignal("");
  const state = () => props.attempt.cod_status ?? "pending";
  const act = createMutation(() => ({
    mutationFn: (action: "deliver" | "collect" | "remit") => {
      const params = { header: tenantHeader(), path: { id: props.orderId } };
      if (action === "deliver")
        return unwrap(api.POST("/admin/v1/orders/{id}/cod/deliver", { params }));
      if (action === "collect")
        return unwrap(
          api.POST("/admin/v1/orders/{id}/cod/collect", {
            params,
            body: { tender: tender(), collector: collector() },
          }),
        );
      return unwrap(
        api.POST("/admin/v1/orders/{id}/cod/remit", {
          params,
          body: { note: note().trim() || null },
        }),
      );
    },
    onSuccess: () => {
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
      void qc.invalidateQueries({ queryKey: tenantKey("order", props.orderId) });
    },
  }));
  return (
    <Section title={t("pay.codTitle")}>
      <div class="grid gap-3 text-sm">
        <dl class="grid max-w-md grid-cols-2 gap-1">
          <dt>{t("pay.codState")}</dt>
          <dd data-testid="cod-state">{t(`codStates.${state()}`)}</dd>
          <Show when={props.attempt.tender && props.attempt.tender !== "unknown"}>
            <dt>{t("pay.tender")}</dt>
            <dd>{t(`tenders.${props.attempt.tender ?? "unknown"}`)}</dd>
          </Show>
          <Show when={props.attempt.collector}>
            {(c) => (
              <>
                <dt>{t("pay.collector")}</dt>
                <dd>{t(`collectors.${c()}`)}</dd>
              </>
            )}
          </Show>
          <dt>{t("orders.total")}</dt>
          <dd class="figures">
            {formatMoney(props.attempt.amount_minor, props.attempt.currency, locale())}
          </dd>
        </dl>
        <Show when={state() === "pending"}>
          <div>
            <Button loading={act.isPending} onClick={() => act.mutate("deliver")}>
              {t("pay.markDelivered")}
            </Button>
          </div>
        </Show>
        <Show when={state() === "pending" || state() === "delivered"}>
          <form
            class="grid gap-3 sm:grid-cols-3 sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              act.mutate("collect");
            }}
          >
            <SelectField
              label={t("pay.tender")}
              value={tender()}
              options={tenders.map((v) => ({ value: v, label: t(`tenders.${v}`) }))}
              onChange={(v) => setTender(tenders.find((x) => x === v) ?? "cash")}
            />
            <SelectField
              label={t("pay.collector")}
              value={collector()}
              options={collectors.map((v) => ({ value: v, label: t(`collectors.${v}`) }))}
              onChange={(v) => setCollector(collectors.find((x) => x === v) ?? "carrier")}
            />
            <div>
              <Button type="submit" variant="confirm" loading={act.isPending}>
                {t("pay.collect")}
              </Button>
            </div>
          </form>
        </Show>
        <Show when={state() === "collected"}>
          <form
            class="grid gap-3 sm:grid-cols-2 sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              act.mutate("remit");
            }}
          >
            <TextField
              label={t("pay.remitNote")}
              value={note()}
              onChange={setNote}
              maxLength={500}
            />
            <div>
              <Button type="submit" variant="confirm" loading={act.isPending}>
                {t("pay.remit")}
              </Button>
            </div>
          </form>
        </Show>
        <ApiProblem error={act.error} />
      </div>
    </Section>
  );
}
