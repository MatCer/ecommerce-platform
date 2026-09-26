import { Button, Dialog, showToast, TextField } from "@platform/ui";
import { createMutation } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { formatDateTime, t } from "../../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../../lib/api.ts";
import { eventContent, safeDownloadUrl } from "../../lib/fulfillment.ts";
import { useMembership } from "../../lib/me.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { Th, tableClass, tdClass } from "../Page.tsx";
import { DownloadButton, FulfillmentState, Section, useFulfillmentRefresh } from "./shared.tsx";
import { WithdrawalTable } from "./WithdrawalTable.tsx";

export function OrderSections(props: { data: Schemas["AdminOrder"] }) {
  return (
    <>
      <Section title={t("fulfillment.shipments")}>
        <Show
          when={props.data.shipments.length}
          fallback={<p class="text-sm">{t("common.none")}</p>}
        >
          <div class="overflow-x-auto">
            <table class={tableClass}>
              <thead>
                <tr>
                  <Th>{t("shipping.carrier")}</Th>
                  <Th>{t("orders.status")}</Th>
                  <Th>{t("fulfillment.tracking")}</Th>
                  <Th>{t("fulfillment.carrierStatus")}</Th>
                  <Th>{t("common.actions")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={props.data.shipments}>
                  {(shipment) => (
                    <tr>
                      <td class={tdClass}>{t(`carriers.${shipment.carrier}`)}</td>
                      <td class={tdClass}>
                        <FulfillmentState value={shipment.status} />
                      </td>
                      <td class={tdClass}>
                        <Show
                          when={shipment.tracking_url && safeDownloadUrl(shipment.tracking_url)}
                          fallback={shipment.tracking_number ?? t("common.none")}
                        >
                          {(url) => (
                            <a
                              href={url()}
                              target="_blank"
                              rel="noopener noreferrer"
                              class="text-accent-700 underline"
                            >
                              {shipment.tracking_number ?? t("fulfillment.tracking")}
                            </a>
                          )}
                        </Show>
                      </td>
                      <td class={tdClass}>{shipment.carrier_status ?? t("common.none")}</td>
                      <td class={tdClass}>
                        <Show when={shipment.has_label && shipment.status !== "cancelled"}>
                          <DownloadButton
                            label={t("fulfillment.downloadLabel")}
                            read={(signal) =>
                              unwrap(
                                api.GET("/admin/v1/orders/{id}/shipment/label", {
                                  params: {
                                    header: tenantHeader(),
                                    path: { id: props.data.order.id },
                                  },
                                  signal,
                                }),
                              )
                            }
                          />
                        </Show>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Show>
      </Section>
      <Section title={t("fulfillment.documents")}>
        <Show
          when={props.data.invoices.length}
          fallback={<p class="text-sm">{t("common.none")}</p>}
        >
          <div class="overflow-x-auto">
            <table class={tableClass}>
              <thead>
                <tr>
                  <Th>{t("orders.kind")}</Th>
                  <Th>{t("orders.number")}</Th>
                  <Th>{t("fulfillment.issuedOn")}</Th>
                  <Th>{t("fulfillment.taxableSupply")}</Th>
                  <Th>{t("orders.total")}</Th>
                  <Th>{t("common.actions")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={props.data.invoices}>
                  {(invoice) => (
                    <tr>
                      <td class={tdClass}>
                        {invoice.kind === "credit_note"
                          ? t("fulfillment.creditNote")
                          : t("fulfillment.invoice")}
                      </td>
                      <td class={tdClass}>{invoice.number}</td>
                      <td class={tdClass}>{invoice.issued_on}</td>
                      <td class={tdClass}>{invoice.taxable_supply_date}</td>
                      <td class={tdClass}>{invoice.total.formatted}</td>
                      <td class={`${tdClass} py-2`}>
                        <DownloadButton
                          label={t("fulfillment.downloadPdf")}
                          disabled={!invoice.pdf_ready}
                          read={(signal) =>
                            unwrap(
                              api.GET("/admin/v1/invoices/{id}/pdf", {
                                params: { header: tenantHeader(), path: { id: invoice.id } },
                                signal,
                              }),
                            )
                          }
                        />
                        <Show when={!invoice.pdf_ready}>
                          <p class="mt-1 max-w-xs text-xs text-muted-foreground">
                            {t("fulfillment.pdfPending")}
                          </p>
                        </Show>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Show>
        <p class="mt-3 text-xs text-muted-foreground">{t("fulfillment.accountantHint")}</p>
      </Section>
      <Section title={t("fulfillment.refunds")}>
        <Show when={props.data.refunds.length} fallback={<p class="text-sm">{t("common.none")}</p>}>
          <div class="overflow-x-auto">
            <table class={tableClass}>
              <thead>
                <tr>
                  <Th>{t("orders.total")}</Th>
                  <Th>{t("orders.status")}</Th>
                  <Th>{t("fulfillment.reason")}</Th>
                  <Th>{t("fulfillment.iban")}</Th>
                  <Th>{t("fulfillment.creditNote")}</Th>
                  <Th>{t("fulfillment.withdrawals")}</Th>
                  <Th>{t("orders.date")}</Th>
                  <Th>{t("common.actions")}</Th>
                </tr>
              </thead>
              <tbody>
                <For each={props.data.refunds}>
                  {(refund) => (
                    <tr>
                      <td class={tdClass}>{refund.amount.formatted}</td>
                      <td class={tdClass}>
                        <FulfillmentState value={refund.status} />
                      </td>
                      <td class={tdClass}>{refund.reason}</td>
                      <td class={tdClass}>{refund.iban}</td>
                      <td class={tdClass}>
                        {props.data.invoices.find((i) => i.id === refund.credit_note_id)?.number ??
                          refund.credit_note_id ??
                          t("common.none")}
                      </td>
                      <td class={`${tdClass} break-all`}>
                        {refund.withdrawal_id ?? t("common.none")}
                      </td>
                      <td class={tdClass}>{formatDateTime(refund.created_at)}</td>
                      <td class={tdClass}>
                        <Show
                          when={
                            refund.status === "pending" ||
                            (refund.status === "failed" && refund.credit_note_id)
                          }
                        >
                          <RetryRefund id={refund.id} />
                        </Show>
                      </td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </Show>
      </Section>
      <Section title={t("fulfillment.withdrawals")}>
        <WithdrawalTable items={props.data.withdrawals} />
      </Section>
    </>
  );
}

function RetryRefund(props: { id: string }) {
  const refresh = useFulfillmentRefresh();
  const { can } = useMembership();
  const retry = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/refunds/{id}/retry", {
          params: { header: tenantHeader(), path: { id: props.id } },
        }),
      ),
    onSuccess: () => {
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
      void refresh();
    },
  }));
  return (
    <Show when={can("admin")}>
      <Button loading={retry.isPending} onClick={() => retry.mutate()}>
        {t("common.retry")}
      </Button>
      <ApiProblem error={retry.error} />
    </Show>
  );
}

export function OrderNote(props: { orderId: string }) {
  const [note, setNote] = createSignal("");
  const refresh = useFulfillmentRefresh();
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/notes", {
          params: { header: tenantHeader(), path: { id: props.orderId } },
          body: { note: note().trim() },
        }),
      ),
    onSuccess: () => {
      setNote("");
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
      void refresh();
    },
  }));
  return (
    <form
      class="mt-3 grid gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (note().trim() && !save.isPending) save.mutate();
      }}
    >
      <TextField
        label={t("orders.notes")}
        multiline
        required
        maxLength={2000}
        value={note()}
        onChange={setNote}
      />
      <ApiProblem error={save.error} />
      <div>
        <Button type="submit" disabled={!note().trim()} loading={save.isPending}>
          {t("fulfillment.addNote")}
        </Button>
      </div>
    </form>
  );
}

export function TimelineEvent(props: { event: Schemas["OrderEventView"] }) {
  const content = () => eventContent(props.event.kind, props.event.data);
  const title = () => {
    const key = content().key;
    return key ? t(`fulfillment.events.${key}`) : props.event.kind;
  };
  return (
    <li
      class="border-l-2 border-border pl-3 text-sm"
      classList={{ "text-warning-700": content().warning }}
    >
      <p class="font-medium">{title()}</p>
      <Show when={content().detail}>
        <p class="whitespace-pre-wrap break-words">{content().detail}</p>
      </Show>
      <p>
        {t("orders.actor")}: {props.event.actor}
      </p>
      <time dateTime={props.event.at}>{formatDateTime(props.event.at)}</time>
    </li>
  );
}

export function EditShippingAddress(props: { data: Schemas["AdminOrder"] }) {
  const [open, setOpen] = createSignal(false);
  const [address, setAddress] = createSignal<Schemas["CheckoutAddress"]>({
    name: "",
    street: "",
    city: "",
    postal_code: "",
    country: props.data.ship_to_country,
  });
  const refresh = useFulfillmentRefresh();
  const save = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.PUT("/admin/v1/orders/{id}/shipping-address", {
          params: { header: tenantHeader(), path: { id: props.data.order.id } },
          body: address(),
        }),
      ),
    onSuccess: () => {
      setOpen(false);
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
      void refresh();
    },
  }));
  return (
    <Show when={props.data.actions.edit_address}>
      <Button
        class="mt-3"
        onClick={() => {
          const current = props.data.order.shipping_address;
          setAddress({
            name: current?.name ?? "",
            company: current?.company,
            street: current?.street ?? "",
            city: current?.city ?? "",
            postal_code: current?.postal_code ?? "",
            country: props.data.ship_to_country,
            phone: current?.phone,
          });
          save.reset();
          setOpen(true);
        }}
      >
        {t("fulfillment.edit_address")}
      </Button>
      <Dialog
        open={open()}
        onOpenChange={(value) => !save.isPending && setOpen(value)}
        title={t("fulfillment.edit_address")}
      >
        <form
          class="grid gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (props.data.actions.edit_address && !save.isPending) save.mutate();
          }}
        >
          <For
            each={["name", "company", "street", "city", "postal_code", "country", "phone"] as const}
          >
            {(key) => (
              <TextField
                label={t(`fulfillment.${key}`)}
                value={address()[key] ?? ""}
                onChange={(value) => setAddress({ ...address(), [key]: value })}
                required={!["company", "phone"].includes(key)}
                readOnly={key === "country"}
                description={key === "country" ? t("fulfillment.countryHint") : undefined}
              />
            )}
          </For>
          <ApiProblem error={save.error} />
          <div class="flex justify-end gap-2">
            <Button disabled={save.isPending} onClick={() => setOpen(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" variant="confirm" loading={save.isPending}>
              {t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>
    </Show>
  );
}
