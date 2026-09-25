import { Button, Dialog, showToast, TextField } from "@platform/ui";
import { createMutation } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { t } from "../../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../../lib/api.ts";
import { needsRefundIban } from "../../lib/fulfillment.ts";
import { useMembership } from "../../lib/me.ts";
import { ApiProblem } from "../CheckoutSettings.tsx";
import { RefundDialog, RefundResult } from "./RefundDialog.tsx";
import { useFulfillmentRefresh } from "./shared.tsx";

const transitions = [
  "start_processing",
  "cancel_label",
  "ship",
  "deliver",
  "returned_to_sender",
] as const;
type Transition = (typeof transitions)[number];

export function OrderActions(props: { data: Schemas["AdminOrder"] }) {
  const refresh = useFulfillmentRefresh();
  const { can } = useMembership();
  const [dialog, setDialog] = createSignal<
    "create_label" | "cancel" | "refund" | "returned_to_sender" | null
  >(null);
  const [weight, setWeight] = createSignal("");
  const [reason, setReason] = createSignal("");
  const [iban, setIban] = createSignal("");
  const [outcome, setOutcome] = createSignal<Schemas["RefundOutcome"]>();
  const params = () => ({ header: tenantHeader(), path: { id: props.data.order.id } });
  const saved = () => {
    setDialog(null);
    showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    void refresh();
  };
  const transition = createMutation(() => ({
    mutationFn: (action: Transition) => {
      const options = { params: params() };
      if (action === "start_processing")
        return unwrap(api.POST("/admin/v1/orders/{id}/processing", options));
      if (action === "cancel_label")
        return unwrap(api.DELETE("/admin/v1/orders/{id}/shipment", options));
      if (action === "ship") return unwrap(api.POST("/admin/v1/orders/{id}/ship", options));
      if (action === "deliver") return unwrap(api.POST("/admin/v1/orders/{id}/deliver", options));
      return unwrap(api.POST("/admin/v1/orders/{id}/returned-to-sender", options));
    },
    onSuccess: saved,
  }));
  const label = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/shipment", {
          params: params(),
          body: { weight_g: weight().trim() ? Number(weight()) : undefined },
        }),
      ),
    onSuccess: saved,
  }));
  const cancel = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/orders/{id}/cancel", {
          params: params(),
          body: {
            reason: reason().trim() || null,
            iban: iban().replace(/\s/g, "").toUpperCase() || null,
          },
        }),
      ),
    onSuccess: (result) => {
      setOutcome(result.refund ?? undefined);
      saved();
    },
  }));
  const pending = () => transition.isPending || label.isPending || cancel.isPending;
  const validWeight = () =>
    !weight().trim() ||
    (/^\d+$/.test(weight()) && Number(weight()) > 0 && Number(weight()) <= 2147483647);
  const requireIban = () =>
    needsRefundIban(props.data.order.payment.method) &&
    ["paid", "partially_refunded"].includes(props.data.order.payment.status);
  const close = () => {
    if (!pending()) setDialog(null);
  };
  const open = (value: NonNullable<ReturnType<typeof dialog>>) => {
    transition.reset();
    label.reset();
    cancel.reset();
    setWeight("");
    setReason("");
    setIban("");
    setDialog(value);
  };
  return (
    <div class="mb-4 grid gap-3">
      <div class="flex flex-wrap gap-2">
        <For each={transitions}>
          {(action) => (
            <Show when={props.data.actions[action]}>
              <Button
                disabled={pending()}
                onClick={() =>
                  action === "returned_to_sender" ? open(action) : transition.mutate(action)
                }
              >
                {t(`fulfillment.${action}`)}
              </Button>
            </Show>
          )}
        </For>
        <Show when={props.data.actions.create_label}>
          <Button disabled={pending()} onClick={() => open("create_label")}>
            {t("fulfillment.create_label")}
          </Button>
        </Show>
        <Show when={props.data.actions.cancel && can("admin")}>
          <Button disabled={pending()} onClick={() => open("cancel")}>
            {t("fulfillment.cancel")}
          </Button>
        </Show>
        <Show when={props.data.actions.refund && can("admin")}>
          <Button disabled={pending()} onClick={() => open("refund")}>
            {t("fulfillment.refund")}
          </Button>
        </Show>
      </div>
      <Show when={!dialog()}>
        <ApiProblem error={transition.error ?? label.error ?? cancel.error} />
      </Show>
      <Show when={outcome()}>{(result) => <RefundResult result={result()} />}</Show>
      <Show when={dialog() === "refund"}>
        <RefundDialog order={props.data.order} onClose={close} />
      </Show>
      <Dialog
        open={dialog() !== null && dialog() !== "refund"}
        onOpenChange={(isOpen) => !isOpen && close()}
        title={
          dialog() === "cancel"
            ? t("fulfillment.cancel")
            : dialog() === "returned_to_sender"
              ? t("fulfillment.returned_to_sender")
              : t("fulfillment.create_label")
        }
        description={
          dialog() === "cancel"
            ? t("fulfillment.cancelConfirm")
            : dialog() === "returned_to_sender"
              ? t("fulfillment.returnedConfirm")
              : undefined
        }
      >
        <form
          class="grid gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (pending()) return;
            if (dialog() === "create_label" && validWeight() && props.data.actions.create_label)
              label.mutate();
            if (
              dialog() === "cancel" &&
              props.data.actions.cancel &&
              (!requireIban() || iban().trim())
            )
              cancel.mutate();
            if (dialog() === "returned_to_sender" && props.data.actions.returned_to_sender)
              transition.mutate("returned_to_sender");
          }}
        >
          <Show when={dialog() === "create_label"}>
            <TextField
              label={t("fulfillment.weight")}
              type="number"
              value={weight()}
              onChange={setWeight}
              error={!validWeight() && t("fulfillment.weightInvalid")}
              ref={(el) => {
                el.min = "1";
                el.step = "1";
              }}
            />
          </Show>
          <Show when={dialog() === "cancel"}>
            <TextField
              label={t("fulfillment.reason")}
              multiline
              value={reason()}
              onChange={setReason}
              maxLength={2000}
            />
            <Show when={requireIban()}>
              <TextField
                label={t("fulfillment.iban")}
                required
                value={iban()}
                onChange={setIban}
                maxLength={42}
              />
            </Show>
          </Show>
          <ApiProblem error={transition.error ?? label.error ?? cancel.error} />
          <div class="flex justify-end gap-2">
            <Button disabled={pending()} onClick={close}>
              {t("common.cancel")}
            </Button>
            <Button
              type="submit"
              variant={dialog() === "cancel" ? "danger" : "primary"}
              loading={pending()}
              disabled={dialog() === "create_label" && !validWeight()}
            >
              {dialog() === "cancel"
                ? t("fulfillment.cancel")
                : dialog() === "returned_to_sender"
                  ? t("fulfillment.returned_to_sender")
                  : t("fulfillment.create_label")}
            </Button>
          </div>
        </form>
      </Dialog>
    </div>
  );
}
