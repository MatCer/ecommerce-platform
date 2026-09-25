import { Button, Checkbox, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { ApiProblem, MarketSettings, TranslationFields } from "../components/CheckoutSettings.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { cleanTranslations } from "../lib/shipping-form.ts";

export default function PaymentMethods() {
  return (
    <>
      <PageHeader title={t("payments.title")} />
      <MarketSettings>{(market) => <Methods market={market} />}</MarketSettings>
    </>
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
          <p class="text-sm text-warning-700">{t("payments.unavailable")}</p>
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
