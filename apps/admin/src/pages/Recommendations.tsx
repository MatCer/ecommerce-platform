import { Button, Checkbox, FieldGroup, SelectField, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Show } from "solid-js";
import { ExplainResult } from "../components/ExplainResult.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { ProductPicker } from "../components/ProductPicker.tsx";
import { contentLocales, errorMessage, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";
import { categoryOptions, useCategoryTree, useMarkets } from "../lib/queries.ts";

type Settings = Schemas["RecommendationSettings"];
type Toggle = "bestsellers" | "bought_together" | "seasonal" | "recently_viewed" | "personalized";
const TOGGLES: Toggle[] = [
  "bought_together",
  "bestsellers",
  "seasonal",
  "personalized",
  "recently_viewed",
];

type Context = "home" | "product" | "category" | "cart" | "collection";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * Recommendation settings (WP17): which strategies run and which products are never
 * recommended (admins), plus a "why recommended" view for staff that runs the real engine for
 * a market and context and explains every product it picked or left out.
 */
export default function Recommendations() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const markets = useMarkets();
  const categories = useCategoryTree();

  const settings = createQuery(() => ({
    queryKey: tenantKey("recommendation-settings"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/recommendations/settings", { params: { header: tenantHeader() } })),
  }));
  const [form, setForm] = createSignal<Settings>();
  createEffect(() => {
    if (settings.data && !form()) setForm({ ...settings.data });
  });
  const set = (patch: Partial<Settings>) => {
    const f = form();
    if (f) setForm({ ...f, ...patch });
  };
  const save = createMutation(() => ({
    mutationFn: (body: Settings) =>
      unwrap(
        api.PUT("/admin/v1/recommendations/settings", {
          params: { header: tenantHeader() },
          body,
        }),
      ),
    onSuccess: async (saved) => {
      setForm({ ...saved });
      await qc.invalidateQueries({ queryKey: tenantKey("recommendation-settings") });
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (err) =>
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") }),
  }));

  // --- why recommended -----------------------------------------------------------------------
  const [market, setMarket] = createSignal<string>();
  const [context, setContext] = createSignal<Context>("home");
  const [products, setProducts] = createSignal<string[]>([]);
  const [category, setCategory] = createSignal<string>("");
  const [collection, setCollection] = createSignal<string>("");
  const [customer, setCustomer] = createSignal("");
  const [asked, setAsked] = createSignal<Record<string, string | number> | null>(null);
  const collections = createQuery(() => ({
    queryKey: tenantKey("collections"),
    queryFn: () => unwrap(api.GET("/admin/v1/collections", { params: { header: tenantHeader() } })),
  }));
  const marketId = () => market() ?? markets.data?.items[0]?.id ?? "";
  const contextValue = (): string | null => {
    switch (context()) {
      case "home":
        return "home";
      case "cart":
        return products().length ? "cart" : null;
      case "product":
        return products()[0] ? `product:${products()[0]}` : null;
      case "category":
        return category() ? `category:${category()}` : null;
      case "collection":
        return collection() ? `collection:${collection()}` : null;
    }
  };
  const customerOk = () => customer().trim() === "" || UUID.test(customer().trim());
  const explain = createQuery(() => {
    const q = asked();
    return {
      queryKey: tenantKey("recommendation-explain", q),
      enabled: q !== null,
      queryFn: () =>
        unwrap(
          api.GET("/admin/v1/recommendations/explain", {
            params: {
              header: tenantHeader(),
              query: {
                market_id: String(q?.market_id ?? ""),
                context: String(q?.context ?? "home"),
                limit: 8,
                ids: q?.ids ? String(q.ids) : undefined,
                customer_id: q?.customer_id ? String(q.customer_id) : undefined,
              },
            },
          }),
        ),
    };
  });
  const run = () => {
    const c = contextValue();
    if (!c || !marketId()) return;
    setAsked({
      market_id: marketId(),
      context: c,
      ...(context() === "cart" ? { ids: products().join(",") } : {}),
      ...(customer().trim() ? { customer_id: customer().trim() } : {}),
    });
  };

  return (
    <>
      <PageHeader
        title={t("recommendations.title")}
        description={t("recommendations.description")}
      />
      <QueryState query={settings}>
        {() => (
          <Show when={form()}>
            {(f) => (
              <form
                class="flex max-w-2xl flex-col gap-4"
                onSubmit={(e) => {
                  e.preventDefault();
                  save.mutate(f());
                }}
              >
                <FieldGroup
                  legend={t("recommendations.strategies")}
                  description={t("recommendations.strategiesHint")}
                >
                  <For each={TOGGLES}>
                    {(k) => (
                      <Checkbox
                        label={t(`recommendations.strategy_${k}`)}
                        description={t(`recommendations.hint_${k}`)}
                        checked={f()[k]}
                        disabled={!can("admin")}
                        onChange={(on) => set({ [k]: on })}
                      />
                    )}
                  </For>
                </FieldGroup>
                <FieldGroup
                  legend={t("recommendations.excluded")}
                  description={t("recommendations.excludedHint")}
                >
                  <Show
                    when={can("admin")}
                    fallback={
                      <p class="text-sm">
                        {t("collections.nSelected", {
                          n: String(f().excluded_product_ids?.length ?? 0),
                        })}
                      </p>
                    }
                  >
                    <ProductPicker
                      value={f().excluded_product_ids ?? []}
                      onChange={(excluded_product_ids) => set({ excluded_product_ids })}
                    />
                  </Show>
                </FieldGroup>
                <Show
                  when={can("admin")}
                  fallback={
                    <p class="text-xs text-muted-foreground">{t("recommendations.adminOnly")}</p>
                  }
                >
                  <div>
                    <Button type="submit" variant="confirm" loading={save.isPending}>
                      {t("common.save")}
                    </Button>
                  </div>
                </Show>
              </form>
            )}
          </Show>
        )}
      </QueryState>

      <section
        aria-labelledby="why"
        class="mt-8 flex max-w-3xl flex-col gap-3 border-t border-border pt-6"
      >
        <h2 id="why" class="text-base font-semibold">
          {t("recommendations.whyTitle")}
        </h2>
        <p class="text-sm text-muted-foreground">{t("recommendations.whyDesc")}</p>
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            run();
          }}
        >
          <div class="grid gap-2 sm:grid-cols-2">
            <SelectField
              label={t("recommendations.market")}
              value={marketId()}
              options={(markets.data?.items ?? []).map((m) => ({ value: m.id, label: m.name }))}
              onChange={setMarket}
            />
            <SelectField
              label={t("recommendations.context")}
              value={context()}
              options={(["home", "product", "category", "cart", "collection"] as const).map(
                (c) => ({ value: c, label: t(`recommendations.context_${c}`) }),
              )}
              onChange={(v) => setContext(v as Context)}
            />
          </div>
          <Show when={context() === "product" || context() === "cart"}>
            <ProductPicker value={products()} onChange={setProducts} />
          </Show>
          <Show when={context() === "category"}>
            <SelectField
              label={t("recommendations.category")}
              value={category()}
              options={[
                { value: "", label: "—" },
                ...categoryOptions(categories.data?.items ?? [], contentLocales()),
              ]}
              onChange={setCategory}
            />
          </Show>
          <Show when={context() === "collection"}>
            <SelectField
              label={t("recommendations.collection")}
              value={collection()}
              options={[
                { value: "", label: "—" },
                ...(collections.data?.items ?? []).map((c) => ({ value: c.id, label: c.name })),
              ]}
              onChange={setCollection}
            />
          </Show>
          <TextField
            label={t("recommendations.customer")}
            description={t("recommendations.customerHint")}
            value={customer()}
            onChange={setCustomer}
            error={customerOk() ? undefined : t("recommendations.customerInvalid")}
          />
          <div>
            <Button
              type="submit"
              variant="confirm"
              disabled={contextValue() === null || !customerOk()}
              loading={explain.isFetching}
            >
              {t("recommendations.explain")}
            </Button>
          </div>
        </form>
        <Show when={asked()}>
          <QueryState query={explain}>{(data) => <ExplainResult data={data} />}</QueryState>
        </Show>
      </section>
    </>
  );
}
