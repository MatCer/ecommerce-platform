import { t } from "@platform/storefront-sdk/format";
import type { OrderPayment, PaymentStart } from "@platform/storefront-sdk/types";
import { Button } from "@platform/ui";
import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { call } from "../lib/client";

type M = Record<string, string>;

const POLL_MS = 2000;
const MAX_POLLS = 90;

/**
 * The order page's payment box (A10): polls the status while an attempt is open, offers to
 * continue to the provider or to pay again after a failure (a new attempt on the same order).
 */
export default function PaymentStatus(props: { m: M; token: string; initial: OrderPayment }) {
  const m = props.m;
  const [p, setP] = createSignal(props.initial);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const base = `/_p/orders/${props.token}`;
  const open = () =>
    p().attempt?.status === "pending" && (p().status === "unpaid" || p().status === "authorized");

  let timer: ReturnType<typeof setInterval> | undefined;
  let polls = 0;
  onMount(() => {
    if (p().method === "cod" || p().method === "bank_transfer") return;
    timer = setInterval(async () => {
      if (!open() || ++polls > MAX_POLLS) return clearInterval(timer);
      const r = await call<OrderPayment>("GET", `${base}/payment`);
      if (r.ok && r.data) setP(r.data);
    }, POLL_MS);
  });
  onCleanup(() => clearInterval(timer));

  async function go(path: string) {
    setBusy(true);
    setError("");
    const r = await call<PaymentStart>("POST", path, {});
    const action = r.data?.action;
    if (r.ok && action?.type === "redirect") return location.assign(action.url);
    setBusy(false);
    if (r.ok) return location.reload();
    setError(t(m, "checkout.error"));
  }

  const message = () => {
    const s = p().status;
    if (s === "paid") return t(m, "order.payment_paid");
    if (p().method === "cod") return t(m, "order.cod_note");
    if (s === "expired") return t(m, "order.payment_expired");
    if (s === "failed") return t(m, "order.payment_failed");
    if (p().method === "bank_transfer") return t(m, "order.bank_transfer_note");
    return t(m, "order.payment_waiting");
  };

  return (
    <div class="grid gap-3">
      <p class="text-sm">
        <span class="font-semibold">{t(m, "order.payment")}:</span>{" "}
        <span data-testid="payment-status">{t(m, `order.payment_status.${p().status}`)}</span>
      </p>
      <p role="status" aria-live="polite" class="text-sm">
        {message()}
      </p>
      <Show when={p().can_retry}>
        <div>
          <Button
            variant="primary"
            loading={busy()}
            onClick={() => void go(`${base}/payment-attempts`)}
          >
            {t(m, "order.payment_retry")}
          </Button>
        </div>
      </Show>
      <Show when={p().can_pay && open() && p().method !== "cod" && p().attempt}>
        {(a) => (
          <div>
            <Button
              variant="secondary"
              loading={busy()}
              onClick={() => void go(`${base}/payment-attempts/${a().id}/init`)}
            >
              {t(m, "order.payment_continue")}
            </Button>
          </div>
        )}
      </Show>
      <Show when={error()}>
        <p role="alert" class="text-sm text-sale">
          {error()}
        </p>
      </Show>
    </div>
  );
}
