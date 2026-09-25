import { Button, Checkbox, SelectField, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Profile = Schemas["TaxProfile"];
type Input = Schemas["TaxProfileInput"];
type Mode = Schemas["DistanceSalesMode"];

/** EU-27 (the platform sells within the EU). */
export const EU_COUNTRIES = [
  "AT",
  "BE",
  "BG",
  "CY",
  "CZ",
  "DE",
  "DK",
  "EE",
  "ES",
  "FI",
  "FR",
  "GR",
  "HR",
  "HU",
  "IE",
  "IT",
  "LT",
  "LU",
  "LV",
  "MT",
  "NL",
  "PL",
  "PT",
  "RO",
  "SE",
  "SI",
  "SK",
] as const;

export function countryName(code: string): string {
  try {
    return new Intl.DisplayNames([locale()], { type: "region" }).of(code) ?? code;
  } catch {
    return code;
  }
}

const blank = (): Input => ({
  vat_payer: true,
  establishment_country: "CZ",
  vat_id: "",
  sk_ic_dph: "",
  distance_sales_mode: "destination",
  confirm_origin_threshold: false,
  cash_rounding_in_vat_base: false,
});

/** The tenant's VAT setup (spec A3); saving is a sensitive operation (A9 fresh auth). */
export default function TaxProfile() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const [form, setForm] = createSignal<Input>(blank());
  const [error, setError] = createSignal<string>();
  const set = (patch: Partial<Input>) => setForm({ ...form(), ...patch });

  const profile = createQuery(() => ({
    queryKey: tenantKey("tax-profile"),
    // `404` means "not configured yet": an empty form, not an error.
    queryFn: async (): Promise<Profile | null> => {
      try {
        return await unwrap(
          api.GET("/admin/v1/tax-profile", { params: { header: tenantHeader() } }),
        );
      } catch (err) {
        if (err instanceof ApiError && err.status === 404) return null;
        throw err;
      }
    },
  }));

  createEffect(() => {
    const p = profile.data;
    if (p === undefined) return;
    setForm(
      p === null
        ? blank()
        : {
            vat_payer: p.vat_payer,
            establishment_country: p.establishment_country,
            vat_id: p.vat_id ?? "",
            sk_ic_dph: p.sk_ic_dph ?? "",
            distance_sales_mode: p.distance_sales_mode,
            confirm_origin_threshold: Boolean(p.origin_threshold_confirmed_at),
            cash_rounding_in_vat_base: p.cash_rounding_in_vat_base,
          },
    );
  });

  const save = createMutation(() => ({
    mutationFn: () => {
      const f = form();
      return unwrap(
        api.PUT("/admin/v1/tax-profile", {
          params: { header: tenantHeader() },
          body: {
            ...f,
            vat_id: f.vat_id?.trim() || null,
            sk_ic_dph: f.sk_ic_dph?.trim() || null,
            confirm_origin_threshold:
              f.distance_sales_mode === "origin_threshold" && f.confirm_origin_threshold,
          },
        }),
      );
    },
    onSuccess: (p) => {
      setError(undefined);
      qc.setQueryData(tenantKey("tax-profile"), p);
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const readOnly = () => !can("admin");

  return (
    <>
      <PageHeader title={t("taxProfile.title")} description={t("taxProfile.lead")} />
      <QueryState query={profile}>
        {(p) => (
          <form
            class="flex max-w-xl flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <Show when={p === null}>
              <p role="status" class="rounded-md bg-warning-50 px-3 py-2 text-sm text-warning-700">
                {t("taxProfile.notSet")}
              </p>
            </Show>
            <Show when={readOnly()}>
              <p class="text-sm text-muted-foreground">{t("taxProfile.adminOnly")}</p>
            </Show>
            <fieldset disabled={readOnly()} class="flex flex-col gap-3">
              <Checkbox
                label={t("taxProfile.vatPayer")}
                checked={form().vat_payer}
                disabled={readOnly()}
                onChange={(vat_payer) => set({ vat_payer })}
              />
              <SelectField
                label={t("taxProfile.establishment")}
                value={form().establishment_country}
                options={EU_COUNTRIES.map((c) => ({ value: c, label: `${countryName(c)} (${c})` }))}
                onChange={(establishment_country) => set({ establishment_country })}
                disabled={readOnly()}
              />
              <div class="grid gap-3 sm:grid-cols-2">
                <TextField
                  label={t("taxProfile.vatId")}
                  value={form().vat_id ?? ""}
                  onChange={(vat_id) => set({ vat_id })}
                  inputClass="figures"
                  maxLength={20}
                  disabled={readOnly()}
                />
                <Show when={form().establishment_country === "SK"}>
                  <TextField
                    label={t("taxProfile.skIcDph")}
                    value={form().sk_ic_dph ?? ""}
                    onChange={(sk_ic_dph) => set({ sk_ic_dph })}
                    inputClass="figures"
                    maxLength={20}
                    disabled={readOnly()}
                  />
                </Show>
              </div>
              <SelectField
                label={t("taxProfile.distanceMode")}
                value={form().distance_sales_mode}
                options={[
                  { value: "destination", label: t("taxProfile.destination") },
                  { value: "origin_threshold", label: t("taxProfile.origin") },
                ]}
                onChange={(v) => set({ distance_sales_mode: v as Mode })}
                disabled={readOnly()}
              />
              <Show when={form().distance_sales_mode === "origin_threshold"}>
                <Checkbox
                  label={t("taxProfile.confirmOrigin")}
                  description={
                    p?.origin_threshold_confirmed_at
                      ? t("taxProfile.confirmedAt", {
                          at: formatDateTime(p.origin_threshold_confirmed_at),
                        })
                      : undefined
                  }
                  checked={form().confirm_origin_threshold ?? false}
                  disabled={readOnly()}
                  onChange={(confirm_origin_threshold) => set({ confirm_origin_threshold })}
                />
              </Show>
              <Checkbox
                label={t("taxProfile.cashRounding")}
                checked={form().cash_rounding_in_vat_base ?? false}
                disabled={readOnly()}
                onChange={(cash_rounding_in_vat_base) => set({ cash_rounding_in_vat_base })}
              />
            </fieldset>
            <Show when={error()}>
              <p role="alert" class="text-xs font-medium text-error-700">
                {error()}
              </p>
            </Show>
            <div>
              <Button
                type="submit"
                variant="primary"
                loading={save.isPending}
                disabled={readOnly()}
              >
                {t("common.save")}
              </Button>
            </div>
          </form>
        )}
      </QueryState>
    </>
  );
}
