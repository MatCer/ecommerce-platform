import {
  Badge,
  Button,
  Checkbox,
  ConfirmDialog,
  Dialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
  type Tone,
} from "@platform/ui";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { ProductPicker } from "../components/ProductPicker.tsx";
import { contentLocales, errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import {
  CURRENCIES,
  type Currency,
  formatMoney,
  instant,
  minorToInput,
  parseMoney,
  parsePercent,
  toLocalInput,
} from "../lib/money.ts";
import { categoryOptions, useCategoryTree } from "../lib/queries.ts";

type Sale = Schemas["Sale"];
/** `selected`: the chosen categories and/or products (the API accepts both together). */
type Target = "all" | "selected";

interface Form {
  name: string;
  type: "percent" | "fixed";
  percent: string;
  amount: string;
  currency: Currency;
  startsAt: string;
  endsAt: string;
  target: Target;
  categoryIds: string[];
  productIds: string[];
}

const blank = (): Form => ({
  name: "",
  type: "percent",
  percent: "",
  amount: "",
  currency: "CZK",
  startsAt: "",
  endsAt: "",
  target: "all",
  categoryIds: [],
  productIds: [],
});

function fromSale(s: Sale): Form {
  const d = s.discount;
  return {
    name: s.name,
    type: d.type,
    percent: d.type === "percent" ? String(d.basis_points / 100) : "",
    amount: d.type === "fixed" ? minorToInput(d.amount_minor) : "",
    currency: d.type === "fixed" ? d.currency : "CZK",
    startsAt: toLocalInput(s.starts_at),
    endsAt: toLocalInput(s.ends_at),
    target: s.targets.all ? "all" : "selected",
    categoryIds: s.targets.category_ids ?? [],
    productIds: s.targets.product_ids ?? [],
  };
}

export function saleState(s: Sale, now = Date.now()): "scheduled" | "running" | "ended" {
  if (new Date(s.starts_at).getTime() > now) return "scheduled";
  if (s.ends_at && new Date(s.ends_at).getTime() <= now) return "ended";
  return "running";
}

const stateTone: Record<ReturnType<typeof saleState>, Tone> = {
  scheduled: "info",
  running: "success",
  ended: "neutral",
};

export default function Sales() {
  const qc = useQueryClient();
  const categories = useCategoryTree();
  const [editing, setEditing] = createSignal<Sale | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Sale | null>(null);
  const [form, setForm] = createSignal<Form>(blank());
  const [error, setError] = createSignal<string>();
  const set = (patch: Partial<Form>) => setForm({ ...form(), ...patch });

  const sales = createInfiniteQuery(() => ({
    queryKey: tenantKey("sales"),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/sales", {
          params: { header: tenantHeader(), query: { cursor: pageParam, limit: 50 } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => sales.data?.pages.flatMap((p) => p.items) ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("sales") });

  const discountText = (s: Sale) =>
    s.discount.type === "percent"
      ? `−${s.discount.basis_points / 100} %`
      : `−${formatMoney(s.discount.amount_minor, s.discount.currency, locale())}`;
  const targetText = (s: Sale) =>
    s.targets.all
      ? t("sales.targetAll")
      : [
          (s.targets.category_ids?.length ?? 0) > 0 &&
            t("sales.nCategories", { n: String(s.targets.category_ids?.length ?? 0) }),
          (s.targets.product_ids?.length ?? 0) > 0 &&
            t("sales.nProducts", { n: String(s.targets.product_ids?.length ?? 0) }),
        ]
          .filter(Boolean)
          .join(", ");
  /** A running sale keeps its start and discount (WP4 rule), so those fields are locked. */
  const locked = () => {
    const e = editing();
    return e !== null && e !== "new" && saleState(e) === "running";
  };

  const create = submission();
  const save = createMutation(() => ({
    // `target` is the sale being edited when Save was pressed (dialogs can change meanwhile).
    mutationFn: (target: Sale | "new") => {
      const f = form();
      const discount =
        f.type === "percent"
          ? { type: "percent" as const, basis_points: parsePercent(f.percent) ?? 0 }
          : {
              type: "fixed" as const,
              amount_minor: parseMoney(f.amount) ?? 0,
              currency: f.currency,
            };
      const body = {
        name: f.name.trim(),
        discount,
        // Untouched times keep their exact instant (the API compares a running sale's start).
        starts_at: instant(f.startsAt, target === "new" ? null : target.starts_at),
        ends_at: instant(f.endsAt, target === "new" ? null : target.ends_at),
        targets:
          f.target === "all"
            ? { all: true }
            : { category_ids: f.categoryIds, product_ids: f.productIds },
      };
      return target === "new"
        ? unwrap(api.POST("/admin/v1/sales", { params: { header: create.header(body) }, body }))
        : unwrap(
            api.PUT("/admin/v1/sales/{id}", {
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
        api.DELETE("/admin/v1/sales/{id}", { params: { header: tenantHeader(), path: { id } } }),
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

  const open = (s: Sale | "new") => {
    setForm(s === "new" ? blank() : fromSale(s));
    setError(undefined);
    setEditing(s);
  };
  const newButton = () => (
    <Button variant="confirm" onClick={() => open("new")}>
      {t("sales.new")}
    </Button>
  );
  const valid = () => {
    const f = form();
    const discountOk =
      f.type === "percent" ? parsePercent(f.percent) !== null : parseMoney(f.amount) !== null;
    const targetsOk = f.target === "all" || f.categoryIds.length + f.productIds.length > 0;
    return f.name.trim() !== "" && discountOk && targetsOk;
  };

  return (
    <>
      <PageHeader title={t("sales.title")} actions={newButton()} />
      <QueryState query={sales}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                title={t("sales.emptyTitle")}
                description={t("sales.emptyDesc")}
                action={newButton()}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("sales.name")}</Th>
                    <Th class="text-right">{t("sales.discount")}</Th>
                    <Th>{t("sales.targets")}</Th>
                    <Th>{t("sales.schedule")}</Th>
                    <Th>{t("sales.state")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(s) => (
                      <tr>
                        <td class={`${tdClass} font-medium`}>{s.name}</td>
                        <td class={`${tdClass} figures text-right`}>{discountText(s)}</td>
                        <td class={`${tdClass} text-xs`}>{targetText(s)}</td>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {formatDateTime(s.starts_at)} –{" "}
                          {s.ends_at ? formatDateTime(s.ends_at) : t("sales.untilStopped")}
                        </td>
                        <td class={tdClass}>
                          <Badge tone={stateTone[saleState(s)]}>
                            {t(`sales.state_${saleState(s)}`)}
                          </Badge>
                        </td>
                        <td class={`${tdClass} text-right whitespace-nowrap`}>
                          <Button category="tertiary" onClick={() => open(s)}>
                            {t("common.edit")}
                            <span class="sr-only">: {s.name}</span>
                          </Button>
                          <Button category="tertiary" onClick={() => setDeleting(s)}>
                            {t("common.delete")}
                            <span class="sr-only">: {s.name}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={sales.hasNextPage}>
              <Button
                class="mt-3"
                loading={sales.isFetchingNextPage}
                onClick={() => void sales.fetchNextPage()}
              >
                {t("common.loadMore")}
              </Button>
            </Show>
          </Show>
        )}
      </QueryState>

      <Dialog
        open={editing() !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        title={editing() === "new" ? t("sales.new") : t("sales.editTitle")}
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
            label={t("sales.name")}
            value={form().name}
            onChange={(name) => set({ name })}
            required
            maxLength={200}
          />
          <div class="grid grid-cols-2 gap-2">
            <SelectField
              label={t("sales.discount")}
              disabled={locked()}
              value={form().type}
              options={[
                { value: "percent", label: t("sales.percent") },
                { value: "fixed", label: t("sales.fixed") },
              ]}
              onChange={(v) => set({ type: v as Form["type"] })}
            />
            <Show
              when={form().type === "fixed"}
              fallback={
                <TextField
                  label={t("sales.percentValue")}
                  disabled={locked()}
                  value={form().percent}
                  onChange={(percent) => set({ percent })}
                  inputMode="decimal"
                  inputClass="figures text-right"
                  required
                />
              }
            >
              <div class="grid grid-cols-[1fr_6rem] gap-2">
                <TextField
                  label={t("sales.amount")}
                  disabled={locked()}
                  value={form().amount}
                  onChange={(amount) => set({ amount })}
                  inputMode="decimal"
                  inputClass="figures text-right"
                  required
                />
                <SelectField
                  label={t("sales.currency")}
                  disabled={locked()}
                  value={form().currency}
                  options={CURRENCIES.map((c) => ({ value: c, label: c }))}
                  onChange={(c) => set({ currency: c as Currency })}
                />
              </div>
            </Show>
          </div>
          <div class="grid grid-cols-2 gap-2">
            <DateTimeField
              label={t("sales.startsAt")}
              disabled={locked()}
              hint={t("sales.startsHint")}
              value={form().startsAt}
              onChange={(startsAt) => set({ startsAt })}
            />
            <DateTimeField
              label={t("sales.endsAt")}
              hint={t("sales.endsHint")}
              value={form().endsAt}
              onChange={(endsAt) => set({ endsAt })}
            />
          </div>
          <SelectField
            label={t("sales.targets")}
            value={form().target}
            options={[
              { value: "all", label: t("sales.targetAll") },
              { value: "selected", label: t("sales.targetSelected") },
            ]}
            onChange={(v) => set({ target: v as Target })}
          />
          <Show when={form().target === "selected"}>
            <Show when={categories.isError}>
              <p role="alert" class="text-xs text-error-700">
                {errorMessage(categories.error)}
              </p>
            </Show>
            <fieldset class="flex max-h-48 flex-col gap-1 overflow-y-auto">
              <legend class="mb-1 text-xs font-medium text-muted-foreground">
                {t("sales.targetCategories")}
              </legend>
              <For each={categoryOptions(categories.data?.items ?? [], contentLocales())}>
                {(c) => (
                  <Checkbox
                    label={c.label}
                    checked={form().categoryIds.includes(c.value)}
                    onChange={(on) =>
                      set({
                        categoryIds: on
                          ? [...form().categoryIds, c.value]
                          : form().categoryIds.filter((x) => x !== c.value),
                      })
                    }
                  />
                )}
              </For>
            </fieldset>
          </Show>
          <Show when={form().target === "selected"}>
            <ProductPicker
              value={form().productIds}
              onChange={(productIds) => set({ productIds })}
            />
          </Show>
          <Show when={locked()}>
            <p class="text-xs text-muted-foreground">{t("errors.sale_started")}</p>
          </Show>
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button type="submit" variant="confirm" loading={save.isPending} disabled={!valid()}>
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("sales.deleteTitle", { name: deleting()?.name ?? "" })}
        description={t("sales.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const s = deleting();
          if (s) remove.mutate(s.id);
        }}
      />
    </>
  );
}
