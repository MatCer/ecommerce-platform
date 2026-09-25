import { Badge, Button, Checkbox, showToast, TextField } from "@platform/ui";
import { useSearchParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, onMount, Show } from "solid-js";
import { ApiProblem, MarketSettings, TranslationFields } from "../components/CheckoutSettings.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { formatDateTime, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { cleanTranslations } from "../lib/shipping-form.ts";

export default function PaymentMethods() {
  return (
    <>
      <PageHeader title={t("payments.title")} />
      <StripeCard />
      <MarketSettings>
        {(market) => (
          <div class="grid gap-4">
            <BankAccountForm market={market} />
            <Methods market={market} />
          </div>
        )}
      </MarketSettings>
    </>
  );
}

/** Stripe Connect (WP11): the shop's connected account, onboarding and capabilities. */
function StripeCard() {
  const qc = useQueryClient();
  // A function: the tenant may be chosen after the first render (keys must follow it).
  const key = () => tenantKey("stripe");
  const [params, setParams] = useSearchParams();
  const status = createQuery(() => ({
    queryKey: key(),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/payments/stripe", { params: { header: tenantHeader() } })),
  }));
  const onboard = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/payments/stripe/onboarding", { params: { header: tenantHeader() } }),
      ),
    onSuccess: (link) => {
      // Stripe-hosted onboarding (or, in simulator mode, this page again).
      if (new URL(link.url, location.href).origin === location.origin) {
        void qc.invalidateQueries({ queryKey: key() });
        showToast({ title: t("pay.simulated"), closeLabel: t("common.close") });
      } else location.assign(link.url);
    },
  }));
  const refresh = createMutation(() => ({
    mutationFn: () =>
      unwrap(api.POST("/admin/v1/payments/stripe/refresh", { params: { header: tenantHeader() } })),
    onSuccess: (data) => qc.setQueryData(key(), data),
  }));
  const simulate = createMutation(() => ({
    mutationFn: (enabled: boolean) =>
      unwrap(
        api.POST("/admin/v1/payments/stripe/simulate", {
          params: { header: tenantHeader() },
          body: { enabled },
        }),
      ),
    onSuccess: () => {
      showToast({ title: t("pay.simulated"), closeLabel: t("common.close") });
      setTimeout(() => void qc.invalidateQueries({ queryKey: key() }), 3000);
    },
  }));
  // Back from Stripe-hosted onboarding: read the account state once.
  onMount(() => {
    if (params.stripe === "return" || params.stripe === "refresh") {
      setParams({ stripe: undefined }, { replace: true });
      refresh.mutate();
    }
  });
  return (
    <section class="mb-6 rounded-md border border-border p-4" aria-labelledby="stripe-heading">
      <h2 id="stripe-heading" class="mb-3 font-semibold">
        {t("pay.stripeTitle")}
      </h2>
      <QueryState query={status}>
        {(s) => (
          <Show
            when={s.mode}
            fallback={<p class="text-sm text-muted-foreground">{t("pay.stripeNotConfigured")}</p>}
          >
            <div class="grid gap-3 text-sm">
              <Show when={s.mode === "simulator"}>
                <p class="rounded-md border border-warning-700 bg-warning-50 p-2">
                  {t("pay.stripeSimulator")}
                </p>
              </Show>
              <Show
                when={s.account}
                fallback={<p class="text-muted-foreground">{t("pay.stripeNoAccount")}</p>}
              >
                {(a) => (
                  <>
                    <p>
                      <Badge tone={a().ready ? "success" : "warning"}>
                        {a().ready ? t("pay.stripeReady") : t("pay.stripeNotReady")}
                      </Badge>
                    </p>
                    <dl class="grid max-w-lg grid-cols-2 gap-1">
                      <dt>{t("pay.stripeAccount")}</dt>
                      <dd class="break-all">{a().account_id}</dd>
                      <dt>{t("pay.stripeCharges")}</dt>
                      <dd>{a().charges_enabled ? t("common.yes") : t("common.no")}</dd>
                      <dt>{t("pay.stripeCards")}</dt>
                      <dd>{a().card_payments}</dd>
                      <dt>{t("pay.stripeDetails")}</dt>
                      <dd>{a().details_submitted ? t("common.yes") : t("common.no")}</dd>
                      <Show when={a().disabled_reason}>
                        <dt>{t("pay.stripeDisabled")}</dt>
                        <dd>{a().disabled_reason}</dd>
                      </Show>
                    </dl>
                  </>
                )}
              </Show>
              <div class="flex flex-wrap gap-2">
                <Show when={!s.account?.ready}>
                  <Button
                    variant="primary"
                    loading={onboard.isPending}
                    onClick={() => onboard.mutate()}
                  >
                    {s.account ? t("pay.stripeContinue") : t("pay.stripeConnect")}
                  </Button>
                </Show>
                <Show when={s.account && s.mode !== "simulator"}>
                  <Button loading={refresh.isPending} onClick={() => refresh.mutate()}>
                    {t("pay.stripeRefresh")}
                  </Button>
                </Show>
                <Show when={s.account && s.mode === "simulator"}>
                  <Button
                    loading={simulate.isPending}
                    onClick={() => simulate.mutate(!s.account?.ready)}
                  >
                    {s.account?.ready ? t("pay.simulateLoss") : t("pay.simulateRestore")}
                  </Button>
                </Show>
              </div>
              <ApiProblem error={onboard.error ?? refresh.error ?? simulate.error} />
            </div>
          </Show>
        )}
      </QueryState>
    </section>
  );
}

/** The market's receiving account for bank transfers (A25). */
function BankAccountForm(props: { market: Schemas["Market"] }) {
  const qc = useQueryClient();
  const key = () => tenantKey("bank-account", props.market.id);
  const account = createQuery(() => ({
    queryKey: key(),
    queryFn: async () => {
      try {
        return await unwrap(
          api.GET("/admin/v1/markets/{id}/bank-account", {
            params: { header: tenantHeader(), path: { id: props.market.id } },
          }),
        );
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      }
    },
  }));
  return (
    <QueryState query={account}>
      {(current) => (
        <BankAccountFields
          market={props.market}
          current={current}
          onSaved={(saved) => {
            qc.setQueryData(key(), saved);
            void qc.invalidateQueries({ queryKey: tenantKey("payment-methods", props.market.id) });
          }}
        />
      )}
    </QueryState>
  );
}

function BankAccountFields(props: {
  market: Schemas["Market"];
  current: Schemas["BankAccount"] | null;
  onSaved: (a: Schemas["BankAccount"]) => void;
}) {
  const [iban, setIban] = createSignal(props.current?.iban ?? "");
  const [bic, setBic] = createSignal(props.current?.bic ?? "");
  const [name, setName] = createSignal(props.current?.account_name ?? "");
  const [token, setToken] = createSignal("");
  const [clear, setClear] = createSignal(false);
  const save = createMutation(() => ({
    mutationFn: (body: Schemas["BankAccountInput"]) =>
      unwrap(
        api.PUT("/admin/v1/markets/{id}/bank-account", {
          params: { header: tenantHeader(), path: { id: props.market.id } },
          body,
        }),
      ),
    onSuccess: (saved) => {
      setToken("");
      setClear(false);
      props.onSaved(saved);
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
  }));
  return (
    <form
      class="rounded-md border border-border p-4"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate({
          iban: iban(),
          bic: bic().trim() || null,
          account_name: name(),
          fio_token: token().trim() || null,
          clear_fio_token: clear(),
        });
      }}
    >
      <fieldset disabled={save.isPending} class="flex flex-col gap-3">
        <legend class="mb-1 font-semibold">
          {t("pay.bankAccount")} ({props.market.currency})
        </legend>
        <p class="text-sm text-muted-foreground">{t("pay.bankAccountDesc")}</p>
        <div class="grid gap-3 sm:grid-cols-3">
          <TextField label={t("pay.iban")} required value={iban()} onChange={setIban} />
          <TextField label={t("pay.bic")} value={bic()} onChange={setBic} />
          <TextField label={t("pay.accountName")} required value={name()} onChange={setName} />
        </div>
        <TextField
          label={t("pay.fioToken")}
          description={t("pay.fioTokenHint")}
          type="password"
          autocomplete="off"
          value={token()}
          onChange={setToken}
        />
        <Show when={props.current?.fio_connected}>
          <p class="text-sm">
            <Badge tone="success">{t("pay.fioConnected")}</Badge>{" "}
            <Show when={props.current?.fio_synced_at}>
              {(at) => t("pay.fioSynced", { at: formatDateTime(at()) })}
            </Show>
          </p>
          <Checkbox label={t("pay.clearFio")} checked={clear()} onChange={setClear} />
        </Show>
        <ApiProblem error={save.error} />
        <div>
          <Button type="submit" variant="primary" loading={save.isPending}>
            {t("pay.saveAccount")}
          </Button>
        </div>
      </fieldset>
    </form>
  );
}

function Methods(props: { market: Schemas["Market"] }) {
  const query = createQuery(() => ({
    queryKey: tenantKey("payment-methods", props.market.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/markets/{id}/payment-methods", {
          params: { header: tenantHeader(), path: { id: props.market.id } },
        }),
      ),
  }));
  return (
    <QueryState query={query}>
      {(data) => (
        <div class="grid gap-4">
          <For each={data.items.map((method) => method.kind)}>
            {(kind) => (
              <PaymentRow
                method={
                  data.items.find((method) => method.kind === kind) as Schemas["PaymentMethod"]
                }
              />
            )}
          </For>
        </div>
      )}
    </QueryState>
  );
}

const reasons = ["not_configured", "no_bank_account", "stripe_onboarding"] as const;

function unavailableText(reason: string | null | undefined): string {
  const known = reasons.find((r) => r === reason);
  return known ? t(`pay.unavailable_${known}`) : t("payments.unavailable");
}

function PaymentRow(props: { method: Schemas["PaymentMethod"] }) {
  const qc = useQueryClient();
  const key = tenantKey("payment-methods", props.method.market_id);
  const [enabled, setEnabled] = createSignal(props.method.enabled);
  const [names, setNames] = createSignal({ ...props.method.name_i18n });
  const [timeout, setTimeout] = createSignal(
    props.method.timeout_minutes == null ? "" : String(props.method.timeout_minutes),
  );
  const [position, setPosition] = createSignal(String(props.method.position));
  const [invalid, setInvalid] = createSignal(false);
  const save = createMutation(() => ({
    mutationFn: (body: Schemas["PaymentMethodInput"]) =>
      unwrap(
        api.PUT("/admin/v1/markets/{id}/payment-methods/{kind}", {
          params: {
            header: tenantHeader(),
            path: { id: props.method.market_id, kind: props.method.kind },
          },
          body,
        }),
      ),
    onSuccess: () => {
      // Keep other rows' unsaved edits intact; fetch fresh data when this screen is revisited.
      void qc.invalidateQueries({ queryKey: key, refetchType: "none" });
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
  }));
  return (
    <form
      class="rounded-md border border-border p-4"
      onSubmit={(e) => {
        e.preventDefault();
        const minutes =
          props.method.kind === "cod" || timeout().trim() === "" ? null : Number(timeout());
        const order = Number(position());
        const valid =
          (minutes === null ||
            (/^\d+$/.test(timeout()) &&
              Number.isInteger(minutes) &&
              minutes >= 5 &&
              minutes <= 43200)) &&
          /^-?\d+$/.test(position()) &&
          Number.isInteger(order) &&
          order >= -2147483648 &&
          order <= 2147483647;
        setInvalid(!valid);
        if (valid)
          save.mutate({
            enabled: enabled(),
            name_i18n: cleanTranslations(names()),
            timeout_minutes: minutes,
            position: order,
          });
      }}
    >
      <fieldset disabled={save.isPending} class="flex flex-col gap-3">
        <legend class="mb-3 font-semibold">{t(`paymentKinds.${props.method.kind}`)}</legend>
        <Show when={!props.method.available}>
          <p class="text-sm text-warning-700">{unavailableText(props.method.unavailable_reason)}</p>
        </Show>
        <Checkbox label={t("payments.enabled")} checked={enabled()} onChange={setEnabled} />
        <TranslationFields
          label={t("payments.nameOverride")}
          values={names()}
          onChange={setNames}
        />
        <div class="grid gap-3 sm:grid-cols-2">
          <Show when={props.method.kind !== "cod"}>
            <TextField
              label={t("payments.timeout")}
              description={t("payments.timeoutHint")}
              value={timeout()}
              inputMode="numeric"
              onChange={setTimeout}
            />
          </Show>
          <TextField
            label={t("checkout.position")}
            value={position()}
            inputMode="numeric"
            onChange={setPosition}
          />
        </div>
        <Show when={invalid()}>
          <p role="alert" class="text-sm text-error-700">
            {t("payments.invalid")}
          </p>
        </Show>
        <ApiProblem error={save.error} />
        <div>
          <Button type="submit" variant="primary" loading={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </fieldset>
    </form>
  );
}
