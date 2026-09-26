import { t } from "@platform/storefront-sdk/format";
import { Button, TextField } from "@platform/ui";
import { createSignal, Show } from "solid-js";
import { call } from "../lib/client";
import HydratedControls from "./HydratedControls";

export default function WithdrawalRequest(props: { m: Record<string, string> }) {
  const [number, setNumber] = createSignal("");
  const [email, setEmail] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [sent, setSent] = createSignal(false);
  const [error, setError] = createSignal("");
  async function send(e: SubmitEvent) {
    e.preventDefault();
    if (busy()) return;
    setBusy(true);
    setError("");
    const r = await call("POST", "/_p/withdraw", { order_number: number(), email: email() });
    setBusy(false);
    if (r.ok) setSent(true);
    else
      setError(
        t(
          props.m,
          r.code === "invalid_email"
            ? "withdraw.invalid_email"
            : r.status === 429
              ? "withdraw.too_many"
              : "withdraw.error",
        ),
      );
  }
  return (
    <div class="grid gap-3" aria-live="polite">
      <Show when={!sent()} fallback={<p role="status">{t(props.m, "withdraw.sent")}</p>}>
        <form onSubmit={send}>
          <HydratedControls class="grid gap-3">
            <TextField
              label={t(props.m, "withdraw.order_number")}
              required
              value={number()}
              onChange={setNumber}
            />
            <TextField
              label={t(props.m, "withdraw.email")}
              type="email"
              autocomplete="email"
              required
              value={email()}
              onChange={setEmail}
            />
            <Button type="submit" variant="primary" loading={busy()}>
              {t(props.m, "withdraw.send")}
            </Button>
          </HydratedControls>
        </form>
      </Show>
      <Show when={error()}>
        <p role="alert" class="text-sale">
          {error()}
        </p>
      </Show>
      <noscript>{t(props.m, "withdraw.no_js")}</noscript>
    </div>
  );
}
