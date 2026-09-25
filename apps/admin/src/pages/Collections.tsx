import {
  Badge,
  Button,
  ConfirmDialog,
  Dialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { ExplainResult } from "../components/ExplainResult.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { ProductPicker } from "../components/ProductPicker.tsx";
import { errorMessage, formatDateTime, LOCALES, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { instant, toLocalInput } from "../lib/money.ts";
import { useMarkets } from "../lib/queries.ts";

type Collection = Schemas["Collection"];
type Kind = Schemas["CollectionKind"];

interface Form {
  name: string;
  titles: Record<string, string>;
  kind: Kind;
  startsAt: string;
  endsAt: string;
  productIds: string[];
}

const blank = (): Form => ({
  name: "",
  titles: {},
  kind: "seasonal",
  startsAt: "",
  endsAt: "",
  productIds: [],
});

const fromCollection = (c: Collection): Form => ({
  name: c.name,
  titles: { ...c.title_i18n },
  kind: c.kind,
  startsAt: toLocalInput(c.starts_at),
  endsAt: toLocalInput(c.ends_at),
  productIds: [...c.product_ids],
});

/** Open now, not yet open, or closed for good. */
export function collectionState(c: Collection, now = Date.now()): "open" | "scheduled" | "ended" {
  if (c.ends_at && new Date(c.ends_at).getTime() <= now) return "ended";
  if (c.starts_at && new Date(c.starts_at).getTime() > now) return "scheduled";
  return "open";
}

const stateTone: Record<ReturnType<typeof collectionState>, Tone> = {
  open: "success",
  scheduled: "info",
  ended: "neutral",
};

/**
 * Collections (WP17): merchant-curated product lists. Seasonal ones fill the home page's
 * recommendations while their window is open; manual ones are shown where the theme asks for
 * them. Preview shows exactly what shoppers of a market would get (visibility filters applied).
 */
export default function Collections() {
  const qc = useQueryClient();
  const markets = useMarkets();
  const [editing, setEditing] = createSignal<Collection | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Collection | null>(null);
  const [previewing, setPreviewing] = createSignal<Collection | null>(null);
  const [previewMarket, setPreviewMarket] = createSignal<string>();
  const [form, setForm] = createSignal<Form>(blank());
  const [error, setError] = createSignal<string>();
  const set = (patch: Partial<Form>) => setForm({ ...form(), ...patch });

  const list = createQuery(() => ({
    queryKey: tenantKey("collections"),
    queryFn: () => unwrap(api.GET("/admin/v1/collections", { params: { header: tenantHeader() } })),
  }));
  const rows = () => list.data?.items ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("collections") });

  const market = () => previewMarket() ?? markets.data?.items[0]?.id;
  const preview = createQuery(() => {
    const c = previewing();
    const m = market();
    // A scheduled collection is previewed as of its start.
    const at = c && collectionState(c) === "scheduled" && c.starts_at ? c.starts_at : undefined;
    return {
      queryKey: tenantKey("collection-preview", c?.id, c?.updated_at, m),
      enabled: c !== null && m !== undefined,
      queryFn: () =>
        unwrap(
          api.GET("/admin/v1/recommendations/explain", {
            params: {
              header: tenantHeader(),
              query: {
                market_id: m ?? "",
                context: `collection:${c?.id ?? ""}`,
                limit: 24,
                at,
              },
            },
          }),
        ),
    };
  });

  const create = submission();
  const save = createMutation(() => ({
    mutationFn: (target: Collection | "new") => {
      const f = form();
      const titles: Record<string, string> = {};
      for (const [l, v] of Object.entries(f.titles)) if (v.trim()) titles[l] = v.trim();
      const body = {
        name: f.name.trim(),
        title_i18n: titles,
        kind: f.kind,
        starts_at: instant(f.startsAt, target === "new" ? null : target.starts_at),
        ends_at: instant(f.endsAt, target === "new" ? null : target.ends_at),
        product_ids: f.productIds,
      };
      return target === "new"
        ? unwrap(
            api.POST("/admin/v1/collections", { params: { header: create.header(body) }, body }),
          )
        : unwrap(
            api.PUT("/admin/v1/collections/{id}", {
              params: { header: tenantHeader(), path: { id: target.id } },
              body,
            }),
          );
    },
    onSuccess: async (_, target) => {
      const created = target === "new";
      if (created) create.done();
      if (editing() === target) setEditing(null);
      await refresh();
      showToast({
        title: created ? t("common.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
    },
    onError: (err, target) => {
      if (editing() === target) setError(errorMessage(err));
    },
  }));

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/collections/{id}", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      setDeleting(null);
      await refresh();
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      setDeleting(null);
      showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });
    },
  }));

  const open = (c: Collection | "new") => {
    setForm(c === "new" ? blank() : fromCollection(c));
    setError(undefined);
    setEditing(c);
  };
  const newButton = () => (
    <Button variant="primary" onClick={() => open("new")}>
      {t("collections.new")}
    </Button>
  );
  const valid = () => {
    const f = form();
    const scheduled = f.kind === "manual" || (f.startsAt !== "" && f.endsAt !== "");
    return f.name.trim() !== "" && scheduled && f.productIds.length > 0;
  };
  const schedule = (c: Collection) =>
    c.starts_at || c.ends_at
      ? `${c.starts_at ? formatDateTime(c.starts_at) : "…"} – ${c.ends_at ? formatDateTime(c.ends_at) : "…"}`
      : t("collections.always");

  return (
    <>
      <PageHeader
        title={t("collections.title")}
        description={t("collections.description")}
        actions={newButton()}
      />
      <QueryState query={list}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                title={t("collections.emptyTitle")}
                description={t("collections.emptyDesc")}
                action={newButton()}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("collections.name")}</Th>
                    <Th>{t("collections.kind")}</Th>
                    <Th>{t("collections.schedule")}</Th>
                    <Th class="text-right">{t("collections.products")}</Th>
                    <Th>{t("collections.state")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(c) => (
                      <tr>
                        <td class={`${tdClass} font-medium`}>{c.name}</td>
                        <td class={`${tdClass} text-xs`}>{t(`collections.kind_${c.kind}`)}</td>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {schedule(c)}
                        </td>
                        <td class={`${tdClass} figures text-right`}>{c.product_ids.length}</td>
                        <td class={tdClass}>
                          <Badge tone={stateTone[collectionState(c)]}>
                            {t(`collections.state_${collectionState(c)}`)}
                          </Badge>
                        </td>
                        <td class={`${tdClass} text-right whitespace-nowrap`}>
                          <Button variant="ghost" onClick={() => setPreviewing(c)}>
                            {t("collections.preview")}
                            <span class="sr-only">: {c.name}</span>
                          </Button>
                          <Button variant="ghost" onClick={() => open(c)}>
                            {t("common.edit")}
                            <span class="sr-only">: {c.name}</span>
                          </Button>
                          <Button variant="ghost" onClick={() => setDeleting(c)}>
                            {t("common.delete")}
                            <span class="sr-only">: {c.name}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={editing() !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        title={editing() === "new" ? t("collections.new") : t("collections.editTitle")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            const target = editing();
            if (target) save.mutate(target);
          }}
        >
          <TextField
            label={t("collections.name")}
            value={form().name}
            onChange={(name) => set({ name })}
            required
            maxLength={200}
          />
          <fieldset class="grid grid-cols-3 gap-2">
            <legend class="mb-1 text-xs font-medium text-muted-foreground">
              {t("collections.heading")}
            </legend>
            <For each={LOCALES}>
              {(l) => (
                <TextField
                  label={l.toUpperCase()}
                  value={form().titles[l] ?? ""}
                  onChange={(v) => set({ titles: { ...form().titles, [l]: v } })}
                  maxLength={200}
                />
              )}
            </For>
          </fieldset>
          <SelectField
            label={t("collections.kind")}
            value={form().kind}
            options={[
              { value: "seasonal", label: t("collections.kind_seasonal") },
              { value: "manual", label: t("collections.kind_manual") },
            ]}
            description={t(`collections.kindHint_${form().kind}`)}
            onChange={(v) => set({ kind: v as Kind })}
          />
          <div class="grid grid-cols-2 gap-2">
            <DateTimeField
              label={t("collections.startsAt")}
              value={form().startsAt}
              onChange={(startsAt) => set({ startsAt })}
            />
            <DateTimeField
              label={t("collections.endsAt")}
              value={form().endsAt}
              onChange={(endsAt) => set({ endsAt })}
            />
          </div>
          <ProductPicker value={form().productIds} onChange={(productIds) => set({ productIds })} />
          <p class="text-xs text-muted-foreground">
            {t("collections.nSelected", { n: String(form().productIds.length) })}
          </p>
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" loading={save.isPending} disabled={!valid()}>
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={previewing() !== null}
        onOpenChange={(o) => !o && setPreviewing(null)}
        title={t("collections.previewTitle", { name: previewing()?.name ?? "" })}
        description={t("collections.previewDesc")}
      >
        <div class="flex flex-col gap-3">
          <SelectField
            label={t("recommendations.market")}
            value={market() ?? ""}
            options={(markets.data?.items ?? []).map((m) => ({ value: m.id, label: m.name }))}
            onChange={setPreviewMarket}
          />
          <QueryState query={preview}>{(data) => <ExplainResult data={data} />}</QueryState>
        </div>
      </Dialog>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("collections.deleteTitle", { name: deleting()?.name ?? "" })}
        description={t("collections.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const c = deleting();
          if (c) remove.mutate(c.id);
        }}
      />
    </>
  );
}
