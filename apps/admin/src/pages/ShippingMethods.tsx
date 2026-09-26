import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  ConfirmDialog,
  Dialog,
  EmptyState,
  labelClass,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Index, Show } from "solid-js";
import { ApiProblem, MarketSettings, TranslationFields } from "../components/CheckoutSettings.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { locale, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { formatMoney } from "../lib/money.ts";
import { type ShippingForm, shippingForm, shippingInput } from "../lib/shipping-form.ts";

const carriers: Schemas["Carrier"][] = ["packeta_pickup", "packeta_home", "ppl", "personal_pickup"];
type Method = Schemas["ShippingMethod"];

export default function ShippingMethods() {
  return (
    <>
      <PageHeader title={t("shipping.title")} />
      <MarketSettings>{(market) => <Methods market={market} />}</MarketSettings>
    </>
  );
}

function Methods(props: { market: Schemas["Market"] }) {
  const qc = useQueryClient();
  const key = tenantKey("shipping-methods", props.market.id);
  const methods = createQuery(() => ({
    queryKey: key,
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/shipping-methods", {
          params: { header: tenantHeader(), query: { market_id: props.market.id } },
        }),
      ),
  }));
  const [editing, setEditing] = createSignal<Method | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Method | null>(null);
  const [form, setForm] = createSignal(shippingForm());
  const [invalid, setInvalid] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  const [deleteError, setDeleteError] = createSignal<unknown>();
  const set = (patch: Partial<ShippingForm>) => setForm({ ...form(), ...patch });
  const money = (n: number) => formatMoney(n, props.market.currency, locale());
  const name = (m: Method) =>
    m.name_i18n[locale()] ||
    m.name_i18n[props.market.default_locale] ||
    Object.values(m.name_i18n)[0] ||
    m.id;
  const open = (m: Method | "new") => {
    setForm(shippingForm(m === "new" ? undefined : m));
    setError(undefined);
    setInvalid(false);
    setEditing(m);
  };
  const submissionKey = submission();
  const save = createMutation(() => ({
    mutationFn: (input: { target: Method | "new"; body: Schemas["ShippingMethodInput"] }) =>
      input.target === "new"
        ? unwrap(
            api.POST("/admin/v1/shipping-methods", {
              params: { header: submissionKey.header(input.body) },
              body: input.body,
            }),
          )
        : unwrap(
            api.PUT("/admin/v1/shipping-methods/{id}", {
              params: { header: tenantHeader(), path: { id: input.target.id } },
              body: input.body,
            }),
          ),
    onSuccess: async () => {
      submissionKey.done();
      setEditing(null);
      await qc.invalidateQueries({ queryKey: key });
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: setError,
  }));
  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/shipping-methods/{id}", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      setDeleting(null);
      await qc.invalidateQueries({ queryKey: key });
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
    },
    onError: (error: unknown) => {
      setDeleting(null);
      setDeleteError(error);
    },
  }));
  const newButton = () => (
    <Button variant="confirm" onClick={() => open("new")}>
      {t("shipping.new")}
    </Button>
  );
  return (
    <>
      <Show when={methods.data?.items.length}>
        <div class="mb-4 flex justify-end">{newButton()}</div>
      </Show>
      <div class="mb-4 empty:hidden">
        <ApiProblem error={deleteError()} />
      </div>
      <QueryState query={methods}>
        {(data) => (
          <Show
            when={data.items.length}
            fallback={
              <EmptyState
                icon="package"
                title={t("shipping.empty")}
                description={t("shipping.emptyDesc")}
                action={newButton()}
              />
            }
          >
            <Card padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("checkout.name")}</Th>
                      <Th>{t("shipping.carrier")}</Th>
                      <Th class="text-right">{t("shipping.price")}</Th>
                      <Th class="text-right">{t("shipping.freeOver")}</Th>
                      <Th>{t("shipping.cod")}</Th>
                      <Th>{t("shipping.active")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(m) => (
                        <tr>
                          <td class={`${tdClass} font-semibold text-heading`}>{name(m)}</td>
                          <td class={tdClass}>{t(`carriers.${m.carrier}`)}</td>
                          <td class={`${tdClass} figures text-right`}>
                            <Show when={m.weight_tiers.length} fallback={money(m.price_minor)}>
                              <For each={m.weight_tiers}>
                                {(tier) => (
                                  <div>
                                    {t("shipping.upTo", { grams: tier.up_to_g })}:{" "}
                                    {money(tier.price_minor)}
                                  </div>
                                )}
                              </For>
                            </Show>
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {m.free_over_minor == null
                              ? t("common.none")
                              : money(m.free_over_minor)}
                          </td>
                          <td class={`${tdClass} figures`}>
                            {m.cod_allowed
                              ? `${t("common.yes")} · ${money(m.cod_fee_minor)}`
                              : t("common.no")}
                          </td>
                          <td class={tdClass}>
                            <Badge tone={m.active ? "success" : "neutral"}>
                              {m.active ? t("common.yes") : t("common.no")}
                            </Badge>
                          </td>
                          <td class={tdClass}>
                            <div class="flex justify-end gap-1">
                              <Button category="tertiary" size="small" onClick={() => open(m)}>
                                {t("common.edit")}
                              </Button>
                              <Button
                                variant="danger"
                                category="tertiary"
                                size="small"
                                disabled={remove.isPending}
                                onClick={() => {
                                  setDeleteError(undefined);
                                  setDeleting(m);
                                }}
                              >
                                {t("common.delete")}
                              </Button>
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
        )}
      </QueryState>
      <Dialog
        open={editing() !== null}
        onOpenChange={(open) => {
          if (!open && !save.isPending) setEditing(null);
        }}
        title={editing() === "new" ? t("shipping.new") : t("shipping.edit")}
      >
        <form
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            const body = shippingInput(form(), props.market.id);
            const target = editing();
            setInvalid(!body);
            setError(undefined);
            if (body && target) save.mutate({ body, target });
          }}
        >
          <fieldset disabled={save.isPending} class="flex flex-col gap-4">
            <SelectField
              label={t("shipping.carrier")}
              value={form().carrier}
              options={carriers.map((value) => ({ value, label: t(`carriers.${value}`) }))}
              onChange={(value) => {
                const carrier = carriers.find((c) => c === value);
                if (carrier) set({ carrier });
              }}
            />
            <p class="text-sm text-muted-foreground">{t("shipping.nameHint")}</p>
            <TranslationFields
              label={t("checkout.name")}
              values={form().names}
              onChange={(names) => set({ names })}
            />
            <TranslationFields
              label={t("shipping.description")}
              values={form().descriptions}
              onChange={(descriptions) => set({ descriptions })}
            />
            <div class="grid gap-4 sm:grid-cols-2">
              <TextField
                label={`${t("shipping.price")} (${props.market.currency})`}
                value={form().price}
                inputMode="decimal"
                onChange={(price) => set({ price })}
              />
              <TextField
                label={`${t("shipping.freeOver")} (${props.market.currency})`}
                description={t("shipping.freeHint")}
                value={form().freeOver}
                inputMode="decimal"
                onChange={(freeOver) => set({ freeOver })}
              />
            </div>
            <fieldset class="flex flex-col gap-2">
              <legend class={`${labelClass} mb-1`}>{t("shipping.tiers")}</legend>
              <p class="text-sm text-muted-foreground">{t("shipping.tierHint")}</p>
              <Index each={form().tiers}>
                {(row, index) => (
                  <div class="flex flex-wrap items-end gap-2">
                    <TextField
                      label={t("shipping.grams")}
                      value={row().grams}
                      inputMode="numeric"
                      onChange={(grams) =>
                        set({
                          tiers: form().tiers.map((r, i) => (i === index ? { ...r, grams } : r)),
                        })
                      }
                    />
                    <TextField
                      label={`${t("shipping.price")} (${props.market.currency})`}
                      value={row().price}
                      inputMode="decimal"
                      onChange={(price) =>
                        set({
                          tiers: form().tiers.map((r, i) => (i === index ? { ...r, price } : r)),
                        })
                      }
                    />
                    <Button
                      category="tertiary"
                      icon="remove"
                      onClick={() => set({ tiers: form().tiers.filter((_, i) => i !== index) })}
                    >
                      {t("common.remove")}
                    </Button>
                  </div>
                )}
              </Index>
              <Button
                class="self-start"
                icon="plus"
                onClick={() => set({ tiers: [...form().tiers, { grams: "", price: "" }] })}
              >
                {t("shipping.addTier")}
              </Button>
            </fieldset>
            <Checkbox
              label={t("shipping.cod")}
              checked={form().cod}
              onChange={(cod) => set({ cod })}
            />
            <TextField
              label={`${t("shipping.codFee")} (${props.market.currency})`}
              value={form().codFee}
              disabled={!form().cod}
              inputMode="decimal"
              onChange={(codFee) => set({ codFee })}
            />
            <Checkbox
              label={t("shipping.active")}
              checked={form().active}
              onChange={(active) => set({ active })}
            />
            <TextField
              label={t("checkout.position")}
              value={form().position}
              inputMode="numeric"
              onChange={(position) => set({ position })}
            />
            <Show when={invalid()}>
              <Alert tone="error">{t("shipping.invalid")}</Alert>
            </Show>
            <ApiProblem error={error()} />
            <div class="flex justify-end gap-2">
              <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
              <Button type="submit" variant="confirm" loading={save.isPending}>
                {t("common.save")}
              </Button>
            </div>
          </fieldset>
        </form>
      </Dialog>
      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(open) => {
          if (!open && !remove.isPending) setDeleting(null);
        }}
        title={t("shipping.deleteTitle", { name: deleting() ? name(deleting() as Method) : "" })}
        description={t("shipping.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const method = deleting();
          if (method) remove.mutate(method.id);
        }}
      />
    </>
  );
}
