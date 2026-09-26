import { t } from "@platform/storefront-sdk/format";
import type { NextAction, OrderPayment, PaymentStart } from "@platform/storefront-sdk/types";
import { createSignal, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { call } from "../lib/client";
import { loadStripe, type StripeElements, type StripeJs } from "../lib/stripe";
import { Button } from "../ui.tsx";

import HydratedControls from "./HydratedControls";

type M = Record<string, string>;

const POLL_MS = 2000;
const MAX_POLLS = 90;

/**
 * The order page's payment box (A10): polls the status while an attempt is open, offers to
 * continue to the provider or to pay again after a failure (a new attempt on the same order).
 * Stripe (WP11) is paid here: the Payment Element (Stripe.js is loaded only now, and only on
 * this page), or, locally without a real key, the clearly labelled test simulator.
 */
export default function PaymentStatus(props: { m: M; token: string; initial: OrderPayment }) {
  const m = props.m;
  const [p, setP] = createSignal(props.initial);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [action, setAction] = createSignal<NextAction | null>(null);
  const base = `/_p/orders/${props.token}`;
  const open = () =>
    p().attempt?.status === "pending" && (p().status === "unpaid" || p().status === "authorized");

  let timer: ReturnType<typeof setInterval> | undefined;
  let polls = 0;
  const poll = () => {
    clearInterval(timer);
    polls = 0;
    timer = setInterval(async () => {
      if (!open() || ++polls > MAX_POLLS) return clearInterval(timer);
      const r = await call<OrderPayment>("GET", `${base}/payment`);
      if (r.ok && r.data) setP(r.data);
    }, POLL_MS);
  };
  onMount(async () => {
    if (p().method === "cod" || p().method === "bank_transfer") return;
    poll();
    const attempt = p().attempt;
    // Back from Stripe's redirect after paying: wait for the webhook, do not offer paying again.
    const returned = new URLSearchParams(location.search).get("redirect_status") === "succeeded";
    if (p().method === "stripe" && p().can_pay && open() && attempt && !returned) {
      // Idempotent (A10): the same intent comes back; its client secret mounts the element.
      const r = await call<PaymentStart>("POST", `${base}/payment-attempts/${attempt.id}/init`, {});
      if (r.ok && r.data?.action) setAction(r.data.action);
      else setError(t(m, "checkout.error"));
    }
  });
  onCleanup(() => clearInterval(timer));

  async function go(path: string) {
    setBusy(true);
    setError("");
    const r = await call<PaymentStart>("POST", path, {});
    const next = r.data?.action;
    if (r.ok && next?.type === "redirect") return location.assign(next.url);
    setBusy(false);
    if (r.ok) return location.reload();
    setError(t(m, "checkout.error"));
  }

  async function simulate(outcome: "succeeded" | "failed") {
    const attempt = p().attempt;
    if (!attempt) return;
    setBusy(true);
    setError("");
    const r = await call<OrderPayment>("POST", `${base}/payment-attempts/${attempt.id}/simulate`, {
      outcome,
    });
    setBusy(false);
    if (!r.ok || !r.data) return setError(t(m, "checkout.error"));
    setP(r.data);
    setAction(null);
    poll();
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
    <HydratedControls class="grid gap-3">
      <p class="text-sm">
        <span class="font-semibold">{t(m, "order.payment")}:</span>{" "}
        <span data-testid="payment-status">{t(m, `order.payment_status.${p().status}`)}</span>
      </p>
      <p role="status" aria-live="polite" class="text-sm">
        {message()}
      </p>
      <Show when={open() && action()}>
        {(a) => (
          <Switch>
            <Match when={a().type === "stripe_simulator"}>
              <section
                aria-labelledby="stripe-simulator"
                class="grid gap-2 rounded-md border-2 border-dashed border-muted-foreground bg-muted p-3"
              >
                <h3 id="stripe-simulator" class="font-display font-bold">
                  {t(m, "order.stripe_sim_title")}
                </h3>
                <p class="text-sm text-muted-foreground">{t(m, "order.stripe_sim_note")}</p>
                <div class="flex flex-wrap gap-2">
                  <Button
                    variant="primary"
                    loading={busy()}
                    onClick={() => void simulate("succeeded")}
                  >
                    {t(m, "order.stripe_sim_succeed")}
                  </Button>
                  <Button
                    variant="secondary"
                    disabled={busy()}
                    onClick={() => void simulate("failed")}
                  >
                    {t(m, "order.stripe_sim_fail")}
                  </Button>
                </div>
              </section>
            </Match>
            <Match when={a().type === "stripe" && a()}>
              {(s) => <StripePayment m={m} action={s() as StripeAction} onError={setError} />}
            </Match>
          </Switch>
        )}
      </Show>
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
      <Show when={p().can_pay && open() && p().method === "fake" && p().attempt}>
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
    </HydratedControls>
  );
}

type StripeAction = Extract<NextAction, { type: "stripe" }>;

/** The real Payment Element (a real Stripe key is configured). */
function StripePayment(props: { m: M; action: StripeAction; onError: (e: string) => void }) {
  let host: HTMLDivElement | undefined;
  const [ready, setReady] = createSignal(false);
  const [paying, setPaying] = createSignal(false);
  let stripe: StripeJs | undefined;
  let elements: StripeElements | undefined;
  onMount(async () => {
    try {
      stripe = await loadStripe(props.action.publishable_key, props.action.account_id);
      elements = stripe.elements({ clientSecret: props.action.client_secret });
      if (host) elements.create("payment").mount(host);
      setReady(true);
    } catch {
      props.onError(t(props.m, "checkout.error"));
    }
  });
  async function pay(e: SubmitEvent) {
    e.preventDefault();
    if (!stripe || !elements) return;
    setPaying(true);
    // Stripe redirects back here; the page then polls until the webhook confirms (A11).
    const r = await stripe.confirmPayment({
      elements,
      confirmParams: { return_url: location.href.split("?")[0] ?? location.href },
    });
    setPaying(false);
    if (r.error) props.onError(r.error.message ?? t(props.m, "checkout.error"));
  }
  return (
    <form class="grid gap-3" onSubmit={(e) => void pay(e)}>
      <Show when={!ready()}>
        <p class="text-sm text-muted-foreground">{t(props.m, "order.stripe_loading")}</p>
      </Show>
      <div ref={host} />
      <div>
        <Button type="submit" variant="primary" loading={paying()} disabled={!ready()}>
          {t(props.m, "order.stripe_pay")}
        </Button>
      </div>
    </form>
  );
}
