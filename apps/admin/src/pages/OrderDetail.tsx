import { useParams } from "@solidjs/router";
import { createQuery } from "@tanstack/solid-query";
import { For, type JSX, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, locale, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";
import { OrderException } from "./Orders.tsx";

function Section(props: { title: string; children: JSX.Element }) {
  return (
    <section class="min-w-0 rounded-md border border-border p-4">
      <h2 class="mb-3 font-semibold">{props.title}</h2>
      {props.children}
    </section>
  );
}
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
    <QueryState query={query}>
      {(data) => (
        <>
          <PageHeader
            title={data.order.number}
            description={formatDateTime(data.order.placed_at)}
            back={{ href: "/orders", label: t("orders.title") }}
          />
          <div class="mb-4 flex flex-wrap items-center gap-3 text-sm">
            <span>
              {t("orders.status")}: {t(`orderStatuses.${data.order.status}`)}
            </span>
            <span>
              {t("orders.paymentStatus")}: {t(`paymentStatuses.${data.order.payment.status}`)}
            </span>
            <span>
              {t("orders.fulfillment")}: {t(`fulfillmentStatuses.${data.order.fulfillment_status}`)}
            </span>
            <OrderException exception={data.order.exception} />
          </div>
          <div class="flex flex-col gap-4">
            <Section title={t("orders.lines")}>
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
                    <For each={data.order.lines}>
                      {(line) => (
                        <tr>
                          <td class={tdClass}>
                            {line.name}
                            <p class="text-xs text-muted-foreground">{line.options_label}</p>
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
            <div class="grid gap-4 lg:grid-cols-2">
              <Section title={t("orders.totals")}>
                <dl class="grid grid-cols-2 gap-2 text-sm">
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
                        <dd class="figures text-right">{data.order[key].formatted}</dd>
                      </>
                    )}
                  </For>
                  <Show when={data.order.coupon_code}>
                    <dt>{t("orders.coupon")}</dt>
                    <dd class="text-right">{data.order.coupon_code}</dd>
                  </Show>
                </dl>
              </Section>
              <Section title={t("orders.vatRecap")}>
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
                      <For each={data.order.vat}>
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
            <Show when={data.order.charges.length}>
              <Section title={t("orders.charges")}>
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
                      <For each={data.order.charges}>
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
            <div class="grid gap-4 md:grid-cols-3">
              <Section title={t("orders.customer")}>
                <div class="break-words text-sm">
                  <p>{data.order.email}</p>
                  <p>{data.order.phone}</p>
                  <Show when={data.customer_id}>
                    <p>
                      {t("orders.customerId")}: {data.customer_id}
                    </p>
                  </Show>
                  <p>
                    {t("checkout.market")}: {data.market_id}
                  </p>
                  <p>
                    {t("orders.country")}: {data.ship_to_country}
                  </p>
                </div>
              </Section>
              <Section title={t("orders.billingAddress")}>
                <Address address={data.order.billing_address} />
              </Section>
              <Section title={t("orders.shippingAddress")}>
                <Address address={data.order.shipping_address} />
              </Section>
            </div>
            <div class="grid gap-4 md:grid-cols-2">
              <Section title={t("shipping.title")}>
                <div class="text-sm">
                  <p>{data.order.shipping.name}</p>
                  <p>{t(`carriers.${data.order.shipping.carrier}`)}</p>
                  <Show when={data.order.shipping.pickup_point}>
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
                  <dd>{t(`paymentKinds.${data.order.payment.method}`)}</dd>
                  <dt>{t("orders.paymentStatus")}</dt>
                  <dd>{t(`paymentStatuses.${data.order.payment.status}`)}</dd>
                  <dt>{t("orders.canRetry")}</dt>
                  <dd>{data.order.payment.can_retry ? t("common.yes") : t("common.no")}</dd>
                  <Show when={data.order.payment.expires_at}>
                    {(expires) => (
                      <>
                        <dt>{t("orders.expires")}</dt>
                        <dd>{formatDateTime(expires())}</dd>
                      </>
                    )}
                  </Show>
                  <Show when={data.order.payment.attempt}>
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
            <Section title={t("orders.attempts")}>
              <Show
                when={data.attempts.length}
                fallback={<p class="text-sm">{t("orders.noAttempts")}</p>}
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
                      <For each={data.attempts}>
                        {(attempt) => (
                          <tr>
                            <td class={tdClass}>{attempt.id}</td>
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
                            <td class={tdClass}>{attempt.provider_ref ?? t("common.none")}</td>
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
                when={data.events.length}
                fallback={<p class="text-sm">{t("orders.noEvents")}</p>}
              >
                <ol class="flex flex-col gap-3">
                  <For each={data.events}>
                    {(event) => (
                      <li class="border-l-2 border-border pl-3 text-sm">
                        <p class="font-medium">{event.kind}</p>
                        <p>
                          {t("orders.actor")}: {event.actor}
                        </p>
                        <time dateTime={event.at}>{formatDateTime(event.at)}</time>
                      </li>
                    )}
                  </For>
                </ol>
              </Show>
            </Section>
            <Show when={data.order.notes}>
              <Section title={t("orders.notes")}>
                <p class="whitespace-pre-wrap break-words text-sm">{data.order.notes}</p>
              </Section>
            </Show>
          </div>
        </>
      )}
    </QueryState>
  );
}
