import { Alert, Button, Checkbox, Dialog, TextField } from "@platform/ui";
import { createMutation } from "@tanstack/solid-query";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  onCleanup,
  Show,
} from "solid-js";
import { t } from "../../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../../lib/api.ts";
import { needsRefundIban, type RefundForm, refundInput } from "../../lib/fulfillment.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { useFulfillmentRefresh } from "./shared.tsx";

export function RefundResult(props: { result: Schemas["RefundOutcome"] }) {
  return (
    <Alert live tone="success">
      <p>{t("fulfillment.refundResult", { amount: props.result.plan.amount.formatted })}</p>
      <Show when={props.result.credit_note_id}>
        <p class="break-all">
          {t("fulfillment.creditNote")}: {props.result.credit_note_id}
        </p>
      </Show>
    </Alert>
  );
}

export function RefundDialog(props: { order: Schemas["OrderView"]; onClose: () => void }) {
  const refresh = useFulfillmentRefresh();
  const [form, setForm] = createSignal<RefundForm>({
    quantities: props.order.lines.map(() => "0"),
    shipping: false,
    paymentFee: false,
    reason: "",
    iban: "",
  });
  const set = (patch: Partial<RefundForm>) => setForm({ ...form(), ...patch });
  const [preview, setPreview] = createSignal<{ signature: string; plan: Schemas["RefundPlan"] }>();
  const [previewError, setPreviewError] = createSignal<unknown>();
  const [previewPending, setPreviewPending] = createSignal(false);
  const [result, setResult] = createSignal<Schemas["RefundOutcome"]>();
  const requireIban = () => needsRefundIban(props.order.payment.method);
  const lines = () => props.order.lines.map((line) => ({ id: line.id, quantity: line.quantity }));
  const input = createMemo(() => refundInput(lines(), form(), requireIban()));
  // IBAN is required on submit, but staff can preview quantities before typing bank details.
  const previewInput = createMemo(() => refundInput(lines(), form(), false));
  const signature = () => JSON.stringify([props.order.id, previewInput()]);
  const currentPreview = () => (preview()?.signature === signature() ? preview()?.plan : undefined);
  createEffect(() => {
    const body = previewInput();
    const orderId = props.order.id;
    const key = JSON.stringify([orderId, body]);
    setPreview(undefined);
    setPreviewError(undefined);
    if (!body || result()) {
      setPreviewPending(false);
      return;
    }
    const controller = new AbortController();
    setPreviewPending(true);
    const timer = setTimeout(async () => {
      try {
        const plan = await unwrap(
          api.POST("/admin/v1/orders/{id}/refunds/preview", {
            params: { header: tenantHeader(), path: { id: orderId } },
            body,
            signal: controller.signal,
          }),
        );
        if (!controller.signal.aborted) setPreview({ signature: key, plan });
      } catch (error) {
        if (!controller.signal.aborted) setPreviewError(error);
      } finally {
        if (!controller.signal.aborted) setPreviewPending(false);
      }
    }, 350);
    onCleanup(() => {
      clearTimeout(timer);
      controller.abort();
    });
  });
  const submit = createMutation(() => ({
    mutationFn: (body: Schemas["RefundInput"]) =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/refunds", {
          params: { header: tenantHeader(), path: { id: props.order.id } },
          body,
        }),
      ),
    onSuccess: (outcome) => {
      setResult(outcome);
      void refresh();
    },
  }));
  const formId = createUniqueId();
  return (
    <Dialog
      open
      onOpenChange={(open) => !open && !submit.isPending && props.onClose()}
      title={t("fulfillment.refund")}
      footer={
        <Show
          when={result()}
          fallback={
            <>
              <Button disabled={submit.isPending} onClick={props.onClose}>
                {t("common.cancel")}
              </Button>
              <Button
                type="submit"
                form={formId}
                variant="confirm"
                loading={submit.isPending}
                disabled={!input() || !currentPreview() || previewPending()}
              >
                {t("fulfillment.refund")}
              </Button>
            </>
          }
        >
          <Button onClick={props.onClose}>{t("common.close")}</Button>
        </Show>
      }
    >
      <Show
        when={result()}
        fallback={
          <form
            id={formId}
            class="grid gap-4"
            onSubmit={(event) => {
              event.preventDefault();
              const body = input();
              if (body && currentPreview() && !submit.isPending) submit.mutate(body);
            }}
          >
            <fieldset disabled={submit.isPending} class="grid min-w-0 gap-4">
              <Show when={lines().some((line) => !line.id)}>
                <Alert tone="warning">{t("fulfillment.missingLineIds")}</Alert>
              </Show>
              <For each={props.order.lines}>
                {(line, index) => (
                  <TextField
                    label={`${line.name} (${line.sku})`}
                    type="number"
                    inputMode="numeric"
                    value={form().quantities[index()] ?? "0"}
                    onChange={(value) =>
                      set({
                        quantities: form().quantities.map((q, i) => (i === index() ? value : q)),
                      })
                    }
                    description={t("fulfillment.quantityHint", { max: line.quantity })}
                    ref={(el) => {
                      el.min = "0";
                      el.max = String(line.quantity);
                      el.step = "1";
                    }}
                  />
                )}
              </For>
              <Checkbox
                label={t("orderTotals.shipping_total")}
                checked={form().shipping}
                onChange={(shipping) => set({ shipping })}
                disabled={props.order.shipping_total.amount_minor === 0}
              />
              <Checkbox
                label={t("orderTotals.payment_fee")}
                checked={form().paymentFee}
                onChange={(paymentFee) => set({ paymentFee })}
                disabled={props.order.payment_fee.amount_minor === 0}
              />
              <TextField
                label={t("fulfillment.reason")}
                multiline
                maxLength={2000}
                value={form().reason}
                onChange={(reason) => set({ reason })}
              />
              <TextField
                label={t("fulfillment.iban")}
                required={requireIban()}
                value={form().iban}
                onChange={(iban) => set({ iban })}
                maxLength={42}
              />
            </fieldset>
            <div
              role="status"
              aria-live="polite"
              class="grid gap-1 rounded-md border border-border bg-subtle p-3 text-sm"
            >
              <h3 class="font-semibold text-heading">{t("fulfillment.preview")}</h3>
              <Show when={previewPending()}>{t("common.loading")}</Show>
              <Show
                when={currentPreview()}
                fallback={!previewPending() && t("fulfillment.previewHint")}
              >
                {(plan) => (
                  <>
                    <For each={plan().lines}>
                      {(line) => (
                        <p>
                          {line.name} × {line.quantity}: {line.amount.formatted}
                        </p>
                      )}
                    </For>
                    <p class="figures font-semibold text-heading">
                      {t("orders.total")}: {plan().amount.formatted}
                    </p>
                    <Show when={plan().full}>
                      <p>{t("fulfillment.fullRefund")}</p>
                    </Show>
                  </>
                )}
              </Show>
            </div>
            <ApiProblem error={previewError()} />
            <ApiProblem error={submit.error} />
          </form>
        }
      >
        {(outcome) => <RefundResult result={outcome()} />}
      </Show>
    </Dialog>
  );
}
