import { Button, Card, Dialog, EmptyState, SelectField, showToast, TextField } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, Show } from "solid-js";
import { ApiProblem } from "../components/CheckoutSettings.tsx";
import { RefundException } from "../components/order/RefundException.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { TransactionTable } from "./BankTransactions.tsx";
import { OrderException } from "./Orders.tsx";

/**
 * The payment exceptions queue (WP11): bank transfers that paid nothing automatically and
 * orders holding money they cannot keep (late or duplicate payments, A10).
 */
export default function PaymentExceptions() {
  const query = createQuery(() => ({
    queryKey: tenantKey("payment-exceptions"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/payment-exceptions", { params: { header: tenantHeader() } })),
  }));
  return (
    <>
      <PageHeader title={t("pay.exceptionsTitle")} description={t("pay.exceptionsDesc")} />
      <QueryState query={query}>
        {(data) => (
          <Show
            when={data.bank_transactions.length || data.orders.length}
            fallback={
              <EmptyState
                icon="check-circle"
                title={t("pay.noExceptions")}
                description={t("pay.noExceptionsDesc")}
              />
            }
          >
            <div class="grid gap-6">
              <Show when={data.bank_transactions.length}>
                <Card
                  title={t("pay.transfers")}
                  count={data.bank_transactions.length}
                  padding="none"
                >
                  <TransactionTable
                    rows={data.bank_transactions}
                    actions={(tx) => <ResolveTransaction tx={tx} />}
                  />
                </Card>
              </Show>
              <Show when={data.orders.length}>
                <Card title={t("pay.ordersWithMoney")} count={data.orders.length} padding="none">
                  <div class="overflow-x-auto">
                    <table class={tableClass}>
                      <thead>
                        <tr>
                          <For
                            each={[
                              t("orders.number"),
                              t("orders.date"),
                              t("orders.email"),
                              t("orders.total"),
                              t("orders.exception"),
                            ]}
                          >
                            {(label, i) => <Th class={i() === 3 ? "text-right" : undefined}>{label}</Th>}
                          </For>
                          <Th srOnly>{t("common.actions")}</Th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={data.orders}>
                          {(o) => (
                            <tr data-testid="exception-order">
                              <td class={tdClass}>
                                <A
                                  class="font-semibold text-heading hover:text-accent-700 hover:underline"
                                  href={`/orders/${o.id}`}
                                >
                                  {o.number}
                                </A>
                              </td>
                              <td class={`${tdClass} figures whitespace-nowrap text-muted-foreground`}>
                                {formatDateTime(o.placed_at)}
                              </td>
                              <td class={tdClass}>{o.email}</td>
                              <td class={`${tdClass} figures text-right`}>{o.total.formatted}</td>
                              <td class={tdClass}>
                                <OrderException exception={o.exception} />
                              </td>
                              <td class={tdClass}>
                                <div class="flex flex-wrap gap-2">
                                  <ResolveOrderException orderId={o.id} />
                                  <RefundException orderId={o.id} />
                                </div>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Card>
              </Show>
            </div>
          </Show>
        )}
      </QueryState>
    </>
  );
}

function useInvalidate() {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: tenantKey("payment-exceptions") });
    void qc.invalidateQueries({ queryKey: tenantKey("bank-transactions") });
    void qc.invalidateQueries({ queryKey: tenantKey("orders") });
  };
}

function ResolveTransaction(props: { tx: Schemas["BankTransaction"] }) {
  const invalidate = useInvalidate();
  const [open, setOpen] = createSignal(false);
  const canAccept = () => props.tx.status === "partial" || props.tx.status === "overpaid";
  const actions = (): Schemas["ResolveAction"][] =>
    canAccept() ? ["accept", "assign", "dismiss"] : ["assign", "dismiss"];
  const [action, setAction] = createSignal<Schemas["ResolveAction"]>(actions()[0] ?? "dismiss");
  const [number, setNumber] = createSignal(props.tx.order_number ?? "");
  const [note, setNote] = createSignal("");
  const resolve = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/bank-transactions/{id}/resolve", {
          params: { header: tenantHeader(), path: { id: props.tx.id } },
          body: {
            action: action(),
            order_number: action() === "assign" ? number() : null,
            note: note().trim() || null,
          },
        }),
      ),
    onSuccess: () => {
      setOpen(false);
      showToast({ title: t("pay.resolved"), closeLabel: t("common.close") });
      invalidate();
    },
  }));
  const formId = createUniqueId();
  return (
    <>
      <Button size="small" onClick={() => setOpen(true)}>
        {t("pay.resolve")}
      </Button>
      <Dialog
        open={open()}
        onOpenChange={setOpen}
        title={t("pay.resolve")}
        size="sm"
        footer={
          <>
            <Button onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
            <Button type="submit" form={formId} variant="confirm" loading={resolve.isPending}>
              {t("pay.resolve")}
            </Button>
          </>
        }
      >
        <form
          id={formId}
          class="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            resolve.mutate();
          }}
        >
          <SelectField
            label={t("pay.status")}
            value={action()}
            options={actions().map((a) => ({ value: a, label: t(`pay.${a}`) }))}
            onChange={(v) => setAction(actions().find((a) => a === v) ?? "dismiss")}
          />
          <Show when={action() === "assign"}>
            <TextField
              label={t("pay.orderNumber")}
              required
              inputMode="numeric"
              value={number()}
              onChange={setNumber}
            />
          </Show>
          <TextField
            label={t("pay.note")}
            description={action() === "dismiss" ? t("pay.noteRequired") : undefined}
            required={action() === "dismiss"}
            multiline
            maxLength={500}
            value={note()}
            onChange={setNote}
          />
          <ApiProblem error={resolve.error} />
        </form>
      </Dialog>
    </>
  );
}

export function ResolveOrderException(props: { orderId: string }) {
  const invalidate = useInvalidate();
  const qc = useQueryClient();
  const [open, setOpen] = createSignal(false);
  const [note, setNote] = createSignal("");
  const resolve = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/exception/resolve", {
          params: { header: tenantHeader(), path: { id: props.orderId } },
          body: { note: note() },
        }),
      ),
    onSuccess: () => {
      setOpen(false);
      showToast({ title: t("pay.resolved"), closeLabel: t("common.close") });
      invalidate();
      void qc.invalidateQueries({ queryKey: tenantKey("order", props.orderId) });
    },
  }));
  const formId = createUniqueId();
  return (
    <>
      <Button onClick={() => setOpen(true)}>{t("pay.resolve")}</Button>
      <Dialog
        open={open()}
        onOpenChange={setOpen}
        title={t("pay.resolve")}
        size="sm"
        footer={
          <>
            <Button onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
            <Button type="submit" form={formId} variant="confirm" loading={resolve.isPending}>
              {t("pay.resolve")}
            </Button>
          </>
        }
      >
        <form
          id={formId}
          class="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            resolve.mutate();
          }}
        >
          <TextField
            label={t("pay.exceptionNote")}
            required
            multiline
            maxLength={500}
            value={note()}
            onChange={setNote}
          />
          <ApiProblem error={resolve.error} />
        </form>
      </Dialog>
    </>
  );
}
