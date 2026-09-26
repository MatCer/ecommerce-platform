import {
  Alert,
  Button,
  Card,
  Checkbox,
  ConfirmDialog,
  ErrorState,
  FieldGroup,
  linkClass,
  SelectField,
  showToast,
  Tabs,
  TextField,
} from "@platform/ui";
import { A, useNavigate, useParams } from "@solidjs/router";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createEffect, createSignal, For, type JSX, onCleanup, Show } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { AiPanel } from "../components/AiPanel.tsx";
import { MediaManager } from "../components/MediaManager.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { ParameterValues } from "../components/ParameterValues.tsx";
import { ProductPrices } from "../components/ProductPrices.tsx";
import { RichText } from "../components/RichText.tsx";
import { VariantsEditor } from "../components/VariantsEditor.tsx";
import { contentLocales, errorMessage, t } from "../i18n/index.ts";
import { ApiError, api, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { categoryName, flatten } from "../lib/category-tree.ts";
import { tenantKey } from "../lib/me.ts";
import {
  CONTENT_LOCALES,
  type ContentLocale,
  draftFromProduct,
  draftToInput,
  emptyDraft,
  type PartyDraft,
  type ProductDraft,
  type ProductOption,
  type ProductStatus,
  slugify,
  type UnitMeasure,
} from "../lib/product-form.ts";
import { useCategoryTree, useParameters, useTaxCategories } from "../lib/queries.ts";

/** "Red / M" from a variant's option values. */
function variantLabel(
  options: readonly ProductOption[],
  values: Record<string, string> | undefined,
): string {
  return options
    .map((o) => {
      const v = o.values.find((x) => x.code === values?.[o.code]);
      if (!v) return "";
      for (const l of contentLocales()) if (v.name_i18n[l]) return v.name_i18n[l];
      return v.code;
    })
    .filter(Boolean)
    .join(" / ");
}

const STATUSES: ProductStatus[] = ["draft", "active", "archived"];
const UNITS: UnitMeasure[] = ["kg", "l", "m", "m2", "pcs"];

/** A titled card with an anchor id; the wrapper keeps it a named region (landmark). */
function Section(props: {
  id: string;
  title: string;
  description?: string;
  children: JSX.Element;
}) {
  return (
    <section id={props.id} aria-label={props.title} class="min-w-0 scroll-mt-16">
      <Card title={props.title} description={props.description}>
        {props.children}
      </Card>
    </section>
  );
}

function PartyFields(props: {
  legend: string;
  description?: string;
  value: PartyDraft;
  onChange: (key: keyof PartyDraft, value: string) => void;
}) {
  return (
    <FieldGroup legend={props.legend} description={props.description ?? t("editor.contactHint")}>
      <div class="grid gap-4 sm:grid-cols-2">
        <TextField
          label={t("editor.partyName")}
          value={props.value.name}
          onChange={(v) => props.onChange("name", v)}
          maxLength={200}
        />
        <TextField
          label={t("editor.partyEmail")}
          type="email"
          value={props.value.email}
          onChange={(v) => props.onChange("email", v)}
          maxLength={254}
        />
        <TextField
          class="sm:col-span-2"
          label={t("editor.address")}
          value={props.value.address}
          onChange={(v) => props.onChange("address", v)}
          multiline
          rows={2}
          maxLength={500}
        />
        <TextField
          label={t("editor.url")}
          type="url"
          value={props.value.url}
          onChange={(v) => props.onChange("url", v)}
          maxLength={500}
        />
        <TextField
          label={t("editor.phone")}
          type="tel"
          value={props.value.phone}
          onChange={(v) => props.onChange("phone", v)}
          maxLength={50}
        />
      </div>
    </FieldGroup>
  );
}

export default function ProductEditor() {
  const params = useParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const isNew = () => !params.id;
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  const [draft, setDraft] = createStore<ProductDraft>(emptyDraft());
  const [loaded, setLoaded] = createSignal<string | null>(null);
  const [saveError, setSaveError] = createSignal<string>();
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [newCountry, setNewCountry] = createSignal("");

  const product = createQuery(() => ({
    queryKey: tenantKey("product", params.id),
    queryFn: () =>
      unwrap(
        api.GET("/admin/v1/products/{id}", {
          params: { header: tenantHeader(), path: { id: params.id as string } },
        }),
      ),
    enabled: !isNew(),
  }));
  const categories = useCategoryTree();
  const parameters = useParameters();
  const taxCategories = useTaxCategories();

  // Load the product into the working copy once per id (later refetches keep local edits).
  createEffect(() => {
    if (isNew() && loaded() !== "new") {
      setDraft(reconcile(emptyDraft()));
      setLoaded("new");
    } else if (product.data && loaded() !== product.data.id) {
      setDraft(reconcile(draftFromProduct(product.data)));
      setLoaded(product.data.id);
    }
  });

  /** After AI fields were saved: the stored product replaces the working copy. */
  const reloadProduct = async () => {
    const r = await product.refetch();
    if (r.data && alive) {
      setDraft(reconcile(draftFromProduct(r.data)));
      setLoaded(r.data.id);
    }
  };

  const skus = () => draft.variants.map((v) => v.sku).filter((s) => s.trim() !== "");
  const skuBase = () =>
    slugify(draft.translations.cs.name || draft.translations.sk.name || draft.translations.en.name)
      .slice(0, 20)
      .toUpperCase();

  const creation = submission();
  const save = createMutation(() => ({
    // The tenant is captured when the save starts: switching shops mid-request must neither
    // cache the answer under the new shop nor update this (by then unmounted) screen.
    mutationFn: async () => {
      const body = draftToInput(draft);
      const base = tenantKey();
      const route = params.id;
      const created = isNew();
      const header = created ? creation.header(body) : tenantHeader();
      const product = await (created
        ? unwrap(api.POST("/admin/v1/products", { params: { header }, body }))
        : unwrap(
            api.PUT("/admin/v1/products/{id}", {
              params: { header, path: { id: params.id as string } },
              body,
            }),
          ));
      return { product, base, created, route };
    },
    onSuccess: ({ product: p, base, created, route }) => {
      if (created) creation.done();
      qc.setQueryData([...base, "product", p.id], p);
      void qc.invalidateQueries({ queryKey: [...base, "products"] });
      // Only the screen that started the save reacts (same user, shop, product and still open).
      if (!alive || route !== params.id || JSON.stringify(base) !== JSON.stringify(tenantKey()))
        return;
      setSaveError(undefined);
      setDraft(reconcile(draftFromProduct(p)));
      setLoaded(p.id);
      showToast({
        title: created ? t("editor.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
      if (created) navigate(`/products/${p.id}`, { replace: true });
    },
    onError: (err) => {
      if (!alive) return;
      setSaveError(errorMessage(err));
      document.getElementById("save-error")?.focus();
    },
  }));

  const remove = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.DELETE("/admin/v1/products/{id}", {
          params: { header: tenantHeader(), path: { id: params.id as string } },
        }),
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: tenantKey("products") });
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
      navigate("/products");
    },
    onError: (err) =>
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") }),
  }));

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    save.mutate();
  };

  const countries = () =>
    [...new Set((taxCategories.data?.items ?? []).map((c) => c.country))].sort();
  const categoriesFor = (country: string) =>
    (taxCategories.data?.items ?? []).filter((c) => c.country === country);

  const form = () => (
    <form id="product-form" onSubmit={submit} class="flex flex-col gap-4" novalidate>
      <Show when={saveError()}>
        <div id="save-error" tabindex="-1" class="outline-none">
          <Alert tone="error">{saveError()}</Alert>
        </div>
      </Show>

      <div class="grid items-start gap-4 xl:grid-cols-[minmax(0,1fr)_20rem]">
        <div class="flex min-w-0 flex-col gap-4">
          <Section
            id="content"
            title={t("editor.content")}
            description={t("editor.translationHint")}
          >
            <Tabs
              label={t("editor.languages")}
              items={CONTENT_LOCALES.map((l: ContentLocale) => ({
                value: l,
                label: (
                  <>
                    {t(`common.locale_${l}`)}
                    <Show when={draft.translations[l].name.trim()}>
                      <span class="ml-1 text-success-700" aria-hidden="true">
                        •
                      </span>
                    </Show>
                  </>
                ),
                content: () => (
                  <div class="flex flex-col gap-4">
                    <div class="grid gap-4 sm:grid-cols-2">
                      <TextField
                        label={t("editor.name")}
                        value={draft.translations[l].name}
                        onChange={(v) => setDraft("translations", l, "name", v)}
                        maxLength={300}
                        required={l === "cs"}
                      />
                      <div
                        onFocusIn={() => {
                          if (!draft.translations[l].slug && draft.translations[l].name) {
                            setDraft(
                              "translations",
                              l,
                              "slug",
                              slugify(draft.translations[l].name),
                            );
                          }
                        }}
                      >
                        <TextField
                          label={t("editor.slug")}
                          description={t("editor.slugHint")}
                          value={draft.translations[l].slug}
                          onChange={(v) => setDraft("translations", l, "slug", v)}
                          inputClass="figures"
                          maxLength={200}
                        />
                      </div>
                    </div>
                    <TextField
                      label={t("editor.shortDescription")}
                      value={draft.translations[l].short_description}
                      onChange={(v) => setDraft("translations", l, "short_description", v)}
                      multiline
                      rows={2}
                      maxLength={1000}
                    />
                    <RichText
                      label={t("editor.description")}
                      value={draft.translations[l].description_html}
                      onChange={(v) => setDraft("translations", l, "description_html", v)}
                    />
                    <div class="grid gap-4 sm:grid-cols-2">
                      <TextField
                        label={t("editor.seoTitle")}
                        value={draft.translations[l].seo_title}
                        onChange={(v) => setDraft("translations", l, "seo_title", v)}
                        maxLength={200}
                      />
                      <TextField
                        label={t("editor.seoDescription")}
                        value={draft.translations[l].seo_description}
                        onChange={(v) => setDraft("translations", l, "seo_description", v)}
                        maxLength={500}
                      />
                    </div>
                  </div>
                ),
              }))}
            />
          </Section>

          <Show when={params.id}>
            {(id) => (
              <div id="ai" class="scroll-mt-16">
                <AiPanel
                  entityType="product"
                  entityId={id()}
                  onAccepted={() => void reloadProduct()}
                  acceptHint={t("ai.unsavedHint")}
                />
              </div>
            )}
          </Show>

          <Section id="media" title={t("editor.media")}>
            <MediaManager
              media={draft.media}
              skus={skus()}
              onChange={(m) => setDraft("media", reconcile(m))}
            />
          </Section>
        </div>

        <aside class="flex min-w-0 flex-col gap-4">
          <Section id="general" title={t("editor.general")}>
            <div class="flex flex-col gap-4">
              <SelectField
                label={t("editor.status")}
                value={draft.status}
                options={STATUSES.map((s) => ({ value: s, label: t(`status.${s}`) }))}
                onChange={(v) => setDraft("status", v as ProductStatus)}
              />
              <TextField
                label={t("editor.brand")}
                value={draft.brand}
                onChange={(v) => setDraft("brand", v)}
                maxLength={200}
              />
            </div>
          </Section>

          <Section id="categories" title={t("editor.categories")}>
            <QueryState query={categories}>
              {(tree) => (
                <Show
                  when={tree.items.length > 0}
                  fallback={<p class="text-sm text-muted-foreground">{t("editor.noCategories")}</p>}
                >
                  <fieldset>
                    <legend class="sr-only">{t("editor.assigned")}</legend>
                    <ul class="flex max-h-72 flex-col gap-1.5 overflow-y-auto">
                      <For each={flatten(tree.items)}>
                        {(row) => (
                          <li style={{ "padding-left": `${row.depth * 1.25}rem` }}>
                            <Checkbox
                              label={categoryName(row.node, contentLocales())}
                              checked={draft.category_ids.includes(row.node.id)}
                              onChange={(on) =>
                                setDraft(
                                  "category_ids",
                                  on
                                    ? [...draft.category_ids, row.node.id]
                                    : draft.category_ids.filter((id) => id !== row.node.id),
                                )
                              }
                            />
                          </li>
                        )}
                      </For>
                    </ul>
                  </fieldset>
                </Show>
              )}
            </QueryState>
          </Section>

          <Section id="feeds" title={t("editor.feeds")}>
            <div class="flex flex-col gap-4">
              <TextField
                label={t("editor.googleCategory")}
                value={draft.google_category}
                onChange={(v) => setDraft("google_category", v)}
                maxLength={500}
              />
              <TextField
                label={t("editor.heurekaCategory")}
                value={draft.heureka_category}
                onChange={(v) => setDraft("heureka_category", v)}
                maxLength={500}
              />
            </div>
          </Section>
        </aside>
      </div>

      {/* Wide tables and long forms span the full width (no sideways scrolling at 1280px). */}
      <Section id="variants" title={t("editor.variants")}>
        <VariantsEditor
          options={draft.options}
          variants={draft.variants}
          skuBase={skuBase()}
          onOptions={(o) => setDraft("options", reconcile(o))}
          onVariants={(v) => setDraft("variants", reconcile(v))}
        />
      </Section>

      <Section id="prices" title={t("prices.title")} description={t("prices.lead")}>
        <Show
          when={!isNew() && product.data}
          fallback={<p class="text-sm text-muted-foreground">{t("prices.saveFirst")}</p>}
        >
          {(p) => (
            <>
              <ProductPrices
                productId={p().id}
                variants={p().variants.map((v) => ({
                  id: v.id,
                  sku: v.sku,
                  label: variantLabel(p().options, v.option_values) || v.sku,
                }))}
              />
              <A
                href={`/inventory?product=${p().id}`}
                class={`${linkClass} mt-4 inline-block text-sm`}
              >
                {t("inventory.openInventory")}
              </A>
            </>
          )}
        </Show>
      </Section>

      <Section id="parameters" title={t("editor.parameters")}>
        <QueryState query={parameters}>
          {(page) => (
            <>
              <Show when={page.truncated}>
                <Alert tone="warning" class="mb-4">
                  {t("parameters.truncated", { count: page.items.length })}
                </Alert>
              </Show>
              <ParameterValues
                parameters={page.items}
                values={draft.parameters}
                skus={skus()}
                onChange={(v) => setDraft("parameters", reconcile(v))}
              />
            </>
          )}
        </QueryState>
      </Section>

      <Section id="pricing" title={t("editor.pricing")}>
        <div class="flex flex-col gap-6">
          <div class="flex flex-col gap-2">
            <div class="grid max-w-md gap-4 sm:grid-cols-2">
              <SelectField
                label={t("editor.unitMeasure")}
                value={draft.unit_measure}
                options={[
                  { value: "", label: t("common.none") },
                  ...UNITS.map((u) => ({ value: u, label: t(`units.${u}`) })),
                ]}
                onChange={(v) => setDraft("unit_measure", v as UnitMeasure | "")}
              />
              <TextField
                label={t("editor.unitQuantity")}
                inputMode="decimal"
                inputClass="figures text-right"
                value={draft.unit_quantity}
                onChange={(v) => setDraft("unit_quantity", v)}
                disabled={draft.unit_measure === ""}
              />
            </div>
            <p class="text-sm text-muted-foreground">{t("editor.unitHint")}</p>
          </div>

          <FieldGroup legend={t("editor.taxTitle")} description={t("editor.taxHint")}>
            <For each={Object.keys(draft.tax_categories).sort()}>
              {(country) => (
                <div class="flex flex-wrap items-end gap-2">
                  <span class="w-10 pb-1.5 font-mono text-sm font-semibold text-heading">
                    {country}
                  </span>
                  <SelectField
                    class="w-64"
                    label={`${t("editor.taxCategory")} (${country})`}
                    value={draft.tax_categories[country] ?? "standard"}
                    options={categoriesFor(country).map((c) => ({
                      value: c.code,
                      label: `${t(`tax.${c.code as "standard"}`)} · ${c.rate} %`,
                    }))}
                    onChange={(v) => setDraft("tax_categories", country, v)}
                  />
                  <Button
                    category="tertiary"
                    iconOnly
                    icon="remove"
                    aria-label={`${t("common.remove")}: ${country}`}
                    onClick={() => {
                      const rest = Object.entries(draft.tax_categories).filter(
                        ([c]) => c !== country,
                      );
                      setDraft("tax_categories", reconcile(Object.fromEntries(rest)));
                    }}
                  />
                </div>
              )}
            </For>
            <div class="flex items-end gap-2">
              <SelectField
                class="w-40"
                label={t("editor.country")}
                value={newCountry()}
                options={[
                  { value: "", label: "—" },
                  ...countries()
                    .filter((c) => !(c in draft.tax_categories))
                    .map((c) => ({ value: c, label: c })),
                ]}
                onChange={setNewCountry}
              />
              <Button
                disabled={!newCountry()}
                onClick={() => {
                  const c = newCountry();
                  const first = categoriesFor(c).find((x) => x.code !== "standard");
                  setDraft("tax_categories", c, first?.code ?? "standard");
                  setNewCountry("");
                }}
              >
                {t("editor.addCountry")}
              </Button>
            </div>
          </FieldGroup>
        </div>
      </Section>

      <Section id="compliance" title={t("editor.compliance")}>
        <div class="flex flex-col gap-6">
          <PartyFields
            legend={t("editor.manufacturer")}
            value={draft.manufacturer}
            onChange={(k, v) => setDraft("manufacturer", k, v)}
          />
          <PartyFields
            legend={t("editor.euResponsible")}
            description={`${t("editor.euResponsibleHint")} ${t("editor.contactHint")}`}
            value={draft.eu_responsible_person}
            onChange={(k, v) => setDraft("eu_responsible_person", k, v)}
          />
          <FieldGroup legend={t("editor.safetyInfo")}>
            <div class="grid gap-4 sm:grid-cols-3">
              <For each={CONTENT_LOCALES}>
                {(l) => (
                  <TextField
                    label={`${t("editor.safetyInfo")} (${l})`}
                    value={draft.safety_info[l]}
                    onChange={(v) => setDraft("safety_info", l, v)}
                    multiline
                    rows={3}
                  />
                )}
              </For>
            </div>
          </FieldGroup>
          <FieldGroup legend={t("editor.warnings")}>
            <div class="grid gap-4 sm:grid-cols-3">
              <For each={CONTENT_LOCALES}>
                {(l) => (
                  <TextField
                    label={`${t("editor.warnings")} (${l})`}
                    value={draft.warnings[l]}
                    onChange={(v) => setDraft("warnings", l, v)}
                    multiline
                    rows={3}
                  />
                )}
              </For>
            </div>
          </FieldGroup>
        </div>
      </Section>
    </form>
  );

  return (
    <>
      <PageHeader
        title={isNew() ? t("editor.titleNew") : t("editor.titleEdit")}
        back={{ href: "/products", label: t("nav.products") }}
        actions={
          <Show when={isNew() || product.data}>
            <Show when={!isNew()}>
              <Button variant="danger" category="secondary" onClick={() => setConfirmDelete(true)}>
                {t("editor.deleteProduct")}
              </Button>
            </Show>
            <Button type="submit" form="product-form" variant="confirm" loading={save.isPending}>
              {t("editor.save")}
            </Button>
          </Show>
        }
      />
      <Show when={!isNew()} fallback={form()}>
        <Show
          when={!(product.error instanceof ApiError && product.error.status === 404)}
          fallback={<ErrorState title={t("editor.notFound")} />}
        >
          <QueryState query={product}>{() => form()}</QueryState>
        </Show>
      </Show>
      <ConfirmDialog
        open={confirmDelete()}
        onOpenChange={setConfirmDelete}
        title={t("editor.deleteTitle")}
        description={t("editor.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => remove.mutate()}
      />
    </>
  );
}
