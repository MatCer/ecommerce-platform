import { Badge, Button, Dialog, showToast } from "@platform/ui";
import { A } from "@solidjs/router";
import { createMutation } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { formatDateTime, t } from "../../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../../lib/api.ts";
import { useMembership } from "../../lib/me.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { Th, tableClass, tdClass } from "../Page.tsx";
import { RefundResult } from "./RefundDialog.tsx";
import { FulfillmentState, useFulfillmentRefresh } from "./shared.tsx";

export function WithdrawalTable(props: { items: Schemas["Withdrawal"][] }) {
  return (
    <Show
      when={props.items.length}
      fallback={<p class="text-sm text-muted-foreground">{t("fulfillment.noWithdrawals")}</p>}
    >
      <div class="overflow-x-auto">
        <table class={tableClass}>
          <thead>
            <tr>
              <Th>{t("orders.number")}</Th>
              <Th>{t("fulfillment.declared")}</Th>
              <Th>{t("fulfillment.refundDue")}</Th>
              <Th>{t("orders.status")}</Th>
              <Th>
                {t("fulfillment.goodsReceived")} / {t("fulfillment.returnProof")}
              </Th>
              <Th>{t("common.actions")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={props.items}>
              {(withdrawal) => (
                <>
                  <tr>
                    <td class={tdClass}>
                      <A
                        class="font-medium text-accent-700 hover:underline"
                        href={`/orders/${withdrawal.order_id}`}
                      >
                        {withdrawal.order_number}
                      </A>
                      <p class="text-xs">{withdrawal.email}</p>
                    </td>
                    <td class={tdClass}>
                      {formatDateTime(withdrawal.declared_at)}
                      <Show when={withdrawal.late}>
                        <Badge tone="warning">{t("fulfillment.late")}</Badge>
                      </Show>
                    </td>
                    <td class={tdClass}>
                      <span classList={{ "font-semibold text-error-700": withdrawal.overdue }}>
                        {formatDateTime(withdrawal.refund_due_at)}
                      </span>
                      <Show when={withdrawal.overdue}>
                        <p class="text-error-700">{t("fulfillment.overdue")}</p>
                      </Show>
                    </td>
                    <td class={tdClass}>
                      <FulfillmentState value={withdrawal.status} />
                      <Show when={withdrawal.refunded_at}>
                        {(at) => (
                          <p>
                            {t("fulfillment.refundedAt")}: {formatDateTime(at())}
                          </p>
                        )}
                      </Show>
                    </td>
                    <td class={tdClass}>
                      <p>
                        {t("fulfillment.goodsReceived")}:{" "}
                        {withdrawal.goods_received_at
                          ? formatDateTime(withdrawal.goods_received_at)
                          : t("common.none")}
                      </p>
                      <p>
                        {t("fulfillment.returnProof")}:{" "}
                        {withdrawal.return_proof_at
                          ? formatDateTime(withdrawal.return_proof_at)
                          : t("common.none")}
                      </p>
                    </td>
                    <td class={tdClass}>
                      <WithdrawalActions withdrawal={withdrawal} />
                    </td>
                  </tr>
                  <tr>
                    <td colSpan={6} class="px-2 py-3">
                      <details>
                        <summary class="cursor-pointer text-sm font-medium">
                          {t("fulfillment.declaration")} · {withdrawal.order_number}
                        </summary>
                        <div class="mt-2 grid gap-2 text-sm">
                          <p class="max-w-prose whitespace-pre-wrap break-words">
                            {withdrawal.declaration}
                          </p>
                          <p>
                            {t("fulfillment.channel")}: {withdrawal.channel}
                          </p>
                          <Show when={withdrawal.delivered_at}>
                            {(at) => (
                              <p>
                                {t("orderStatuses.delivered")}: {formatDateTime(at())}
                              </p>
                            )}
                          </Show>
                          <Show when={withdrawal.iban}>
                            <p>
                              {t("fulfillment.iban")}: {withdrawal.iban}
                            </p>
                          </Show>
                          <Show when={withdrawal.note}>
                            <p class="whitespace-pre-wrap">
                              {t("orders.notes")}: {withdrawal.note}
                            </p>
                          </Show>
                          <ul>
                            <For each={withdrawal.lines}>
                              {(line) => (
                                <li>
                                  {line.name} ({line.sku}) × {line.quantity} ·{" "}
                                  <FulfillmentState value={line.status} />
                                </li>
                              )}
                            </For>
                          </ul>
                        </div>
                      </details>
                    </td>
                  </tr>
                </>
              )}
            </For>
          </tbody>
        </table>
      </div>
    </Show>
  );
}

function WithdrawalActions(props: { withdrawal: Schemas["Withdrawal"] }) {
  const refresh = useFulfillmentRefresh();
  const { can } = useMembership();
  const [action, setAction] = createSignal<"receive" | "proof" | "refund">();
  const [result, setResult] = createSignal<Schemas["RefundOutcome"]>();
  const allowedRefund = () =>
    Boolean(props.withdrawal.goods_received_at || props.withdrawal.return_proof_at);
  const label = () =>
    action() === "refund"
      ? t("fulfillment.refundWithdrawal")
      : action() === "proof"
        ? t("fulfillment.proof")
        : t("fulfillment.receive");
  const mutation = createMutation(() => ({
    mutationFn: async (action: "receive" | "proof" | "refund") => {
      const params = { header: tenantHeader(), path: { id: props.withdrawal.id } };
      if (action === "refund")
        return unwrap(api.POST("/admin/v1/withdrawals/{id}/refund", { params }));
      if (action === "receive")
        await unwrap(api.POST("/admin/v1/withdrawals/{id}/receive", { params }));
      else await unwrap(api.POST("/admin/v1/withdrawals/{id}/proof", { params }));
      return null;
    },
    onSuccess: (outcome) => {
      setResult(outcome ?? undefined);
      setAction(undefined);
      showToast({
        title: outcome
          ? t("fulfillment.refundResult", { amount: outcome.plan.amount.formatted })
          : t("common.saved"),
        closeLabel: t("common.close"),
      });
      void refresh();
    },
  }));
  const open = (value: "receive" | "proof" | "refund") => {
    mutation.reset();
    setAction(value);
  };
  return (
    <div class="grid gap-2 py-2">
      <div class="flex flex-wrap gap-2">
        <Show when={!props.withdrawal.goods_received_at}>
          <Button disabled={mutation.isPending} onClick={() => open("receive")}>
            {t("fulfillment.receive")}
          </Button>
        </Show>
        <Show when={props.withdrawal.status === "open" && !props.withdrawal.return_proof_at}>
          <Button disabled={mutation.isPending} onClick={() => open("proof")}>
            {t("fulfillment.proof")}
          </Button>
        </Show>
        <Show when={props.withdrawal.status === "open" && can("admin")}>
          <Button disabled={!allowedRefund() || mutation.isPending} onClick={() => open("refund")}>
            {t("fulfillment.refundWithdrawal")}
          </Button>
        </Show>
      </div>
      <Show when={props.withdrawal.status === "open" && !allowedRefund()}>
        <p class="max-w-xs text-xs text-muted-foreground">{t("fulfillment.goodsNotBack")}</p>
      </Show>
      <Show when={result()}>{(outcome) => <RefundResult result={outcome()} />}</Show>
      <Dialog
        open={Boolean(action())}
        onOpenChange={(open) => !open && !mutation.isPending && setAction(undefined)}
        title={label()}
        description={t("fulfillment.withdrawalConfirm", { number: props.withdrawal.order_number })}
      >
        <ApiProblem error={mutation.error} />
        <div class="flex justify-end gap-2">
          <Button disabled={mutation.isPending} onClick={() => setAction(undefined)}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="confirm"
            loading={mutation.isPending}
            onClick={() => {
              const value = action();
              if (value && (value !== "refund" || allowedRefund())) mutation.mutate(value);
            }}
          >
            {label()}
          </Button>
        </div>
      </Dialog>
    </div>
  );
}
