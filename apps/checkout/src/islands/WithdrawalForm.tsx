import { t } from "@platform/storefront-sdk/format";
import type {
  DeclareInput,
  WithdrawalForm as Form,
  WithdrawalReceipt,
} from "@platform/storefront-sdk/types";
import { Button, TextField } from "@platform/ui";
import { createSignal, For, Show } from "solid-js";
import { call } from "../lib/client";

export default function WithdrawalForm(props: {
  form: Form;
  target: { mode: "account" | "token"; id: string };
  m: Record<string, string>;
  locale: string;
}) {
  const m = props.m;
  const [quantities, setQuantities] = createSignal<Record<string, number>>({});
  const [chosen, setChosen] = createSignal<Record<string, boolean>>({});
  const [iban, setIban] = createSignal("");
  const [note, setNote] = createSignal("");
  const [review, setReview] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [expired, setExpired] = createSignal(false);
  const [receipt, setReceipt] = createSignal<WithdrawalReceipt | null>(null);
  let stage: HTMLHeadingElement | undefined;
  const date = (value: string) =>
    new Intl.DateTimeFormat(props.locale, { dateStyle: "medium", timeStyle: "short" }).format(
      new Date(value),
    );
  const selected = () => props.form.lines.filter((l) => chosen()[l.order_line_id]);
  function continueToReview(e: SubmitEvent) {
    e.preventDefault();
    setError("");
    if (
      !selected().length ||
      selected().some(
        (l) =>
          !Number.isInteger(quantities()[l.order_line_id]) ||
          (quantities()[l.order_line_id] ?? 0) < 1 ||
          (quantities()[l.order_line_id] ?? 0) > l.withdrawable,
      )
    ) {
      setError(t(m, "withdraw.invalid_withdrawal"));
      return;
    }
    setReview(true);
    stage?.focus();
  }
  async function confirm() {
    if (busy() || !review() || expired()) return;
    setBusy(true);
    setError("");
    const body: DeclareInput = {
      lines: selected().map((l) => ({
        order_line_id: l.order_line_id,
        quantity: quantities()[l.order_line_id] ?? 0,
      })),
      ...(props.form.needs_iban ? { iban: iban() } : {}),
      note: note() || undefined,
      confirm: true,
    };
    const path =
      props.target.mode === "account"
        ? `/_p/account/orders/${props.target.id}/withdrawal`
        : `/_p/withdraw/${props.target.id}`;
    const r = await call<WithdrawalReceipt>("POST", path, body);
    setBusy(false);
    if (r.ok && r.data) {
      setReceipt(r.data);
      stage?.focus();
      return;
    }
    if (r.status === 404) setExpired(true);
    const known = [
      "iban_required",
      "invalid_iban",
      "invalid_withdrawal",
      "not_withdrawable",
      "confirmation_required",
    ];
    setError(
      t(
        m,
        r.status === 404
          ? "withdraw.link_invalid"
          : r.status === 429
            ? "withdraw.too_many"
            : known.includes(r.code ?? "")
              ? `withdraw.${r.code}`
              : "withdraw.error",
      ),
    );
  }
  return (
    <section class="grid gap-4 rounded-lg border border-border bg-card p-4">
      <h2 ref={stage} tabIndex={-1} class="font-display text-lg font-bold">
        {t(m, receipt() ? "withdraw.received" : review() ? "withdraw.review" : "withdraw.title")}
      </h2>
      <Show
        when={receipt()}
        fallback={
          <>
            <p class="font-semibold">
              {t(m, "withdraw.order_number")}: {props.form.order_number}
            </p>
            <dl class="grid gap-1 text-sm">
              <div>
                <dt class="inline">{t(m, "withdraw.placed_at")}: </dt>
                <dd class="inline">{date(props.form.placed_at)}</dd>
              </div>
              <Show when={props.form.delivered_at}>
                {(v) => (
                  <div>
                    <dt class="inline">{t(m, "withdraw.delivered_at")}: </dt>
                    <dd class="inline">{date(v())}</dd>
                  </div>
                )}
              </Show>
              <Show when={props.form.deadline}>
                {(v) => (
                  <div>
                    <dt class="inline">{t(m, "withdraw.deadline")}: </dt>
                    <dd class="inline">{date(v())}</dd>
                  </div>
                )}
              </Show>
            </dl>
            <Show when={props.form.deadline && Date.parse(props.form.deadline) < Date.now()}>
              <p class="text-sm text-muted-foreground">{t(m, "withdraw.past_deadline")}</p>
            </Show>
            <Show when={props.form.eligible} fallback={<p>{t(m, "withdraw.not_eligible")}</p>}>
              <Show
                when={!review()}
                fallback={
                  <div class="grid gap-3">
                    <p>{t(m, "withdraw.review_hint")}</p>
                    <ul class="grid gap-2">
                      <For each={selected()}>
                        {(l) => (
                          <li>
                            {quantities()[l.order_line_id]}× {l.name} {l.options_label}{" "}
                            <span class="text-muted-foreground">{l.sku}</span>
                          </li>
                        )}
                      </For>
                    </ul>
                    <Show when={props.form.needs_iban}>
                      <p>
                        {t(m, "withdraw.iban")}: {iban()}
                      </p>
                    </Show>
                    <Show when={note()}>
                      <p class="whitespace-pre-wrap">
                        {t(m, "withdraw.note")}: {note()}
                      </p>
                    </Show>
                    <Button
                      type="button"
                      variant="primary"
                      loading={busy()}
                      disabled={expired()}
                      onClick={confirm}
                    >
                      {t(m, "withdraw.confirm")}
                    </Button>
                    <Button
                      type="button"
                      disabled={busy()}
                      onClick={() => {
                        setReview(false);
                        setError("");
                        stage?.focus();
                      }}
                    >
                      {t(m, "withdraw.back")}
                    </Button>
                  </div>
                }
              >
                <form class="grid gap-4" onSubmit={continueToReview}>
                  <fieldset class="grid gap-3">
                    <legend class="mb-2 font-semibold">{t(m, "withdraw.items")}</legend>
                    <For each={props.form.lines}>
                      {(l) => (
                        <div class="grid gap-2 border-b border-border pb-3">
                          <label class="flex items-start gap-2">
                            <input
                              type="checkbox"
                              disabled={l.withdrawable === 0}
                              checked={chosen()[l.order_line_id] ?? false}
                              onChange={(e) => {
                                setChosen((v) => ({
                                  ...v,
                                  [l.order_line_id]: e.currentTarget.checked,
                                }));
                                setQuantities((q) => ({ ...q, [l.order_line_id]: 1 }));
                              }}
                            />
                            <span>
                              {l.name} {l.options_label}{" "}
                              <span class="text-muted-foreground">{l.sku}</span>
                              {l.withdrawable === 0 && <> — {t(m, "withdraw.already")}</>}
                            </span>
                          </label>
                          <Show when={chosen()[l.order_line_id]}>
                            <label class="grid gap-1 text-sm">
                              {t(m, "withdraw.quantity")}
                              <span class="sr-only"> {l.name}</span>
                              <input
                                class="w-24 rounded-md border border-border bg-background px-3 py-2"
                                type="number"
                                min={1}
                                max={l.withdrawable}
                                step={1}
                                required
                                value={quantities()[l.order_line_id]}
                                onInput={(e) =>
                                  setQuantities((q) => ({
                                    ...q,
                                    [l.order_line_id]: e.currentTarget.valueAsNumber,
                                  }))
                                }
                              />
                            </label>
                          </Show>
                        </div>
                      )}
                    </For>
                  </fieldset>
                  <Show when={props.form.needs_iban}>
                    <TextField
                      label={t(m, "withdraw.iban")}
                      description={t(m, "withdraw.iban_hint")}
                      required
                      value={iban()}
                      onChange={setIban}
                    />
                  </Show>
                  <label class="grid gap-1 text-sm">
                    {t(m, "withdraw.note")}
                    <textarea
                      class="rounded-md border border-border bg-background px-3 py-2"
                      rows={3}
                      value={note()}
                      onInput={(e) => setNote(e.currentTarget.value)}
                    />
                  </label>
                  <Button type="submit" variant="primary" disabled={expired()}>
                    {t(m, "withdraw.continue")}
                  </Button>
                </form>
              </Show>
            </Show>
            <noscript>{t(m, "withdraw.no_js")}</noscript>
          </>
        }
      >
        {(r) => (
          <div class="grid gap-3" role="status">
            <p>
              {t(m, "withdraw.declared_at")}: {date(r().declared_at)}
            </p>
            <p>
              {t(m, "withdraw.refund_due_at")}: {date(r().refund_due_at)}
            </p>
            <pre class="whitespace-pre-wrap break-words font-sans text-sm">{r().declaration}</pre>
            <p>{t(m, "withdraw.copy_sent")}</p>
          </div>
        )}
      </Show>
      <Show when={error()}>
        <p role="alert" class="text-sale">
          {error()}
        </p>
      </Show>
      <Show when={expired()}>
        <a href="/withdraw" class="text-identity-ink underline">
          {t(m, "withdraw.send")}
        </a>
      </Show>
    </section>
  );
}
