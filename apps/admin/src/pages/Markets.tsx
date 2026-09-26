import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, t } from "../i18n/index.ts";
import { api, idempotencyKey, type Schemas, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { useMarkets } from "../lib/queries.ts";

type TaxMode = Schemas["TaxMode"];

const list = (s: string) =>
  s
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

const blank = () => ({
  code: "",
  name: "",
  countries: "",
  currency: "EUR",
  default_locale: "",
  locales: "",
  tax_mode: "gross" as TaxMode,
  is_default: false,
});

export default function Markets() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const markets = useMarkets();
  const [open, setOpen] = createSignal(false);
  const [form, setForm] = createSignal(blank());
  const [error, setError] = createSignal<string>();
  const set = (patch: Partial<ReturnType<typeof blank>>) => setForm({ ...form(), ...patch });

  const create = createMutation(() => ({
    mutationFn: () => {
      const f = form();
      const locales = list(f.locales.toLowerCase());
      return unwrap(
        api.POST("/admin/v1/markets", {
          params: { header: idempotencyKey() },
          body: {
            code: f.code.trim().toLowerCase(),
            name: f.name.trim(),
            country_codes: list(f.countries.toUpperCase()),
            currency: f.currency.trim().toUpperCase(),
            default_locale: f.default_locale.trim().toLowerCase() || (locales[0] ?? ""),
            locales,
            tax_mode: f.tax_mode,
            is_default: f.is_default,
          },
        }),
      );
    },
    onSuccess: async () => {
      setOpen(false);
      await qc.invalidateQueries({ queryKey: tenantKey("markets") });
      showToast({ title: t("common.created"), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const newButton = () => (
    <Button
      variant="confirm"
      disabled={!can("admin")}
      onClick={() => {
        setForm(blank());
        setError(undefined);
        setOpen(true);
      }}
    >
      {t("markets.new")}
    </Button>
  );

  return (
    <>
      <PageHeader
        title={t("markets.title")}
        description={can("admin") ? undefined : t("markets.adminOnly")}
        actions={newButton()}
      />
      <QueryState query={markets}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={<EmptyState icon="earth" title={t("markets.emptyTitle")} />}
          >
            <Card padding="none">
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("markets.name")}</Th>
                      <Th>{t("markets.code")}</Th>
                      <Th>{t("markets.countries")}</Th>
                      <Th>{t("markets.currency")}</Th>
                      <Th>{t("markets.locales")}</Th>
                      <Th>{t("markets.taxMode")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={data.items}>
                      {(m) => (
                        <tr>
                          <td class={`${tdClass} font-semibold text-heading`}>
                            {m.name}{" "}
                            <Show when={m.is_default}>
                              <Badge tone="info">{t("markets.defaultBadge")}</Badge>
                            </Show>
                          </td>
                          <td class={`${tdClass} font-mono text-xs`}>{m.code}</td>
                          <td class={`${tdClass} figures text-xs`}>{m.country_codes.join(", ")}</td>
                          <td class={`${tdClass} font-mono text-xs`}>{m.currency}</td>
                          <td class={`${tdClass} figures text-xs`}>
                            {m.locales
                              .map((l) => (l === m.default_locale ? `${l}*` : l))
                              .join(", ")}
                          </td>
                          <td class={tdClass}>{t(`markets.${m.tax_mode}`)}</td>
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

      <Dialog open={open()} onOpenChange={setOpen} title={t("markets.new")}>
        <form
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate();
          }}
        >
          <div class="grid gap-4 sm:grid-cols-2">
            <TextField
              label={t("markets.name")}
              value={form().name}
              onChange={(name) => set({ name })}
              required
              maxLength={200}
            />
            <TextField
              label={t("markets.code")}
              description={t("markets.codeHint")}
              value={form().code}
              onChange={(code) => set({ code })}
              inputClass="figures"
              required
              maxLength={32}
            />
            <TextField
              label={t("markets.countries")}
              description={t("markets.countriesHint")}
              value={form().countries}
              onChange={(countries) => set({ countries })}
              inputClass="figures"
              required
            />
            <TextField
              label={t("markets.currency")}
              value={form().currency}
              onChange={(currency) => set({ currency })}
              inputClass="figures"
              required
              maxLength={3}
            />
            <TextField
              label={t("markets.locales")}
              description={t("markets.localesHint")}
              value={form().locales}
              onChange={(locales) => set({ locales })}
              inputClass="figures"
              required
            />
            <TextField
              label={t("markets.defaultLocale")}
              value={form().default_locale}
              placeholder={list(form().locales)[0] ?? ""}
              onChange={(default_locale) => set({ default_locale })}
              inputClass="figures"
            />
          </div>
          <SelectField
            label={t("markets.taxMode")}
            value={form().tax_mode}
            options={[
              { value: "gross", label: t("markets.gross") },
              { value: "net", label: t("markets.net") },
            ]}
            onChange={(v) => set({ tax_mode: v as TaxMode })}
          />
          <Checkbox
            label={t("markets.isDefault")}
            checked={form().is_default}
            onChange={(is_default) => set({ is_default })}
          />
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setOpen(false)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="confirm" loading={create.isPending}>
              {t("common.create")}
            </Button>
          </div>
        </form>
      </Dialog>
    </>
  );
}
