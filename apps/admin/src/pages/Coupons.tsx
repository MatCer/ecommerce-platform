import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  ConfirmDialog,
  Dialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, createUniqueId, For, Show } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
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

type Coupon = Schemas["Coupon"];
type Kind = Schemas["CouponDiscount"]["type"];

interface Form {
  code: string;
  type: Kind;
  percent: string;
  amount: string;
  currency: Currency | "";
  minSubtotal: string;
  usageLimit: string;
  perCustomer: string;
  startsAt: string;
  endsAt: string;
  published: boolean;
}

const blank = (): Form => ({
  code: "",
  type: "percent",
  percent: "",
  amount: "",
  currency: "",
  minSubtotal: "",
  usageLimit: "",
  perCustomer: "",
  startsAt: "",
  endsAt: "",
  published: false,
});

function fromCoupon(c: Coupon): Form {
  const d = c.discount;
  return {
    code: c.code,
    type: d.type,
    percent: d.type === "percent" ? String(d.basis_points / 100) : "",
    amount: d.type === "fixed" ? minorToInput(d.amount_minor) : "",
    currency: c.currency ?? "",
    minSubtotal: minorToInput(c.min_subtotal_minor),
    usageLimit: c.usage_limit == null ? "" : String(c.usage_limit),
    perCustomer: c.per_customer_limit == null ? "" : String(c.per_customer_limit),
    startsAt: toLocalInput(c.starts_at),
    endsAt: toLocalInput(c.ends_at),
    published: c.published,
  };
}

/** Blank -> no restriction (`null`); otherwise a positive 32-bit count, or `undefined` if invalid. */
export function limit(s: string): number | null | undefined {
  const v = s.trim();
  if (v === "") return null;
  if (!/^\d+$/.test(v)) return undefined;
  const n = Number(v);
  return n >= 1 && n <= 2_147_483_647 ? n : undefined;
}

/** Blank -> no minimum; otherwise minor units, or `undefined` if invalid. */
function minimum(s: string): number | null | undefined {
  return s.trim() === "" ? null : (parseMoney(s) ?? undefined);
}

/** A published coupon that has started only accepts a new end and new limits (WP4 rule). */
function isLocked(c: Coupon | "new" | null): boolean {
  return (
    c !== null &&
    c !== "new" &&
    c.published &&
    (!c.starts_at || new Date(c.starts_at).getTime() <= Date.now())
  );
}

export default function Coupons() {
  const qc = useQueryClient();
  const [editing, setEditing] = createSignal<Coupon | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Coupon | null>(null);
  const [form, setForm] = createSignal<Form>(blank());
  const [error, setError] = createSignal<string>();
  const set = (patch: Partial<Form>) => setForm({ ...form(), ...patch });

  const coupons = createInfiniteQuery(() => ({
    queryKey: tenantKey("coupons"),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/coupons", {
          params: { header: tenantHeader(), query: { cursor: pageParam, limit: 50 } },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));
  const rows = () => coupons.data?.pages.flatMap((p) => p.items) ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("coupons") });

  const discountText = (c: Coupon) => {
    const d = c.discount;
    if (d.type === "percent") return `−${d.basis_points / 100} %`;
    if (d.type === "fixed") return `−${formatMoney(d.amount_minor, c.currency ?? "EUR", locale())}`;
    return t("coupons.freeShipping");
  };

  const valid = () => {
    const f = form();
    const discountOk =
      f.type === "percent"
        ? parsePercent(f.percent) !== null
        : f.type === "fixed"
          ? parseMoney(f.amount) !== null
          : true;
    const needsCurrency = f.type === "fixed" || f.minSubtotal.trim() !== "";
    return (
      f.code.trim() !== "" &&
      discountOk &&
      (!needsCurrency || f.currency !== "") &&
      minimum(f.minSubtotal) !== undefined &&
      limit(f.usageLimit) !== undefined &&
      limit(f.perCustomer) !== undefined
    );
  };

  const create = submission();
  const save = createMutation(() => ({
    // `target` is the coupon being edited when Save was pressed (dialogs can change meanwhile).
    mutationFn: (target: Coupon | "new") => {
      const f = form();
      const discount =
        f.type === "percent"
          ? { type: "percent" as const, basis_points: parsePercent(f.percent) ?? 0 }
          : f.type === "fixed"
            ? { type: "fixed" as const, amount_minor: parseMoney(f.amount) ?? 0 }
            : { type: "free_shipping" as const };
      const body = {
        code: f.code.trim(),
        discount,
        currency: f.currency === "" ? null : f.currency,
        min_subtotal_minor: minimum(f.minSubtotal) ?? null,
        usage_limit: limit(f.usageLimit) ?? null,
        per_customer_limit: limit(f.perCustomer) ?? null,
        starts_at: instant(f.startsAt, target === "new" ? null : target.starts_at),
        ends_at: instant(f.endsAt, target === "new" ? null : target.ends_at),
        published: f.published,
      };
      return target === "new"
        ? unwrap(api.POST("/admin/v1/coupons", { params: { header: create.header(body) }, body }))
        : unwrap(
            api.PUT("/admin/v1/coupons/{id}", {
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
        api.DELETE("/admin/v1/coupons/{id}", { params: { header: tenantHeader(), path: { id } } }),
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

  const open = (c: Coupon | "new") => {
    setForm(c === "new" ? blank() : fromCoupon(c));
    setError(undefined);
    setEditing(c);
  };
  const formId = createUniqueId();
  const newButton = () => (
    <Button variant="confirm" onClick={() => open("new")}>
      {t("coupons.new")}
    </Button>
  );

  return (
    <>
      <PageHeader title={t("coupons.title")} actions={newButton()} />
      <QueryState query={coupons}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <EmptyState
                icon="tag"
                title={t("coupons.emptyTitle")}
                description={t("coupons.emptyDesc")}
                action={newButton()}
              />
            }
          >
            <Card
              padding="none"
              footer={
                coupons.hasNextPage ? (
                  <Button
                    loading={coupons.isFetchingNextPage}
                    onClick={() => void coupons.fetchNextPage()}
                  >
                    {t("common.loadMore")}
                  </Button>
                ) : undefined
              }
            >
              <div class="overflow-x-auto">
                <table class={tableClass}>
                  <thead>
                    <tr>
                      <Th>{t("coupons.code")}</Th>
                      <Th class="text-right">{t("coupons.discount")}</Th>
                      <Th>{t("coupons.validity")}</Th>
                      <Th class="text-right">{t("coupons.used")}</Th>
                      <Th>{t("sales.state")}</Th>
                      <Th srOnly>{t("common.actions")}</Th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(c) => (
                        <tr class="hover:bg-subtle">
                          <td class={`${tdClass} font-mono text-sm font-semibold text-heading`}>
                            {c.code}
                          </td>
                          <td class={`${tdClass} figures text-right`}>{discountText(c)}</td>
                          <td
                            class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                          >
                            {c.starts_at || c.ends_at
                              ? `${c.starts_at ? formatDateTime(c.starts_at) : "…"} – ${c.ends_at ? formatDateTime(c.ends_at) : "…"}`
                              : t("coupons.always")}
                          </td>
                          <td class={`${tdClass} figures text-right`}>
                            {c.used_count} / {c.usage_limit ?? "∞"}
                          </td>
                          <td class={tdClass}>
                            <Badge tone={c.published ? "success" : "neutral"}>
                              {c.published ? t("coupons.live") : t("coupons.private")}
                            </Badge>
                          </td>
                          <td class={`${tdClass} text-right whitespace-nowrap`}>
                            <Button category="tertiary" size="small" onClick={() => open(c)}>
                              {t("common.edit")}
                              <span class="sr-only">: {c.code}</span>
                            </Button>
                            <Button
                              category="tertiary"
                              size="small"
                              iconOnly
                              icon="remove"
                              aria-label={`${t("common.delete")}: ${c.code}`}
                              title={t("common.delete")}
                              onClick={() => setDeleting(c)}
                            />
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
        onOpenChange={(o) => !o && setEditing(null)}
        title={editing() === "new" ? t("coupons.new") : t("coupons.editTitle")}
        footer={
          <>
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              form={formId}
              variant="confirm"
              loading={save.isPending}
              disabled={!valid()}
            >
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </>
        }
      >
        <form
          id={formId}
          class="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            const target = editing();
            if (target) save.mutate(target);
          }}
        >
          <div class="grid grid-cols-2 gap-4">
            <TextField
              label={t("coupons.code")}
              disabled={isLocked(editing())}
              value={form().code}
              onChange={(code) => set({ code: code.toUpperCase() })}
              inputClass="figures"
              required
              maxLength={64}
            />
            <SelectField
              label={t("coupons.discount")}
              disabled={isLocked(editing())}
              value={form().type}
              options={[
                { value: "percent", label: t("sales.percent") },
                { value: "fixed", label: t("sales.fixed") },
                { value: "free_shipping", label: t("coupons.freeShipping") },
              ]}
              onChange={(v) => set({ type: v as Kind })}
            />
            <Show when={form().type === "percent"}>
              <TextField
                label={t("sales.percentValue")}
                disabled={isLocked(editing())}
                value={form().percent}
                onChange={(percent) => set({ percent })}
                inputMode="decimal"
                inputClass="figures text-right"
                required
              />
            </Show>
            <Show when={form().type === "fixed"}>
              <TextField
                label={t("sales.amount")}
                disabled={isLocked(editing())}
                value={form().amount}
                onChange={(amount) => set({ amount })}
                inputMode="decimal"
                inputClass="figures text-right"
                required
              />
            </Show>
            <SelectField
              label={t("sales.currency")}
              disabled={isLocked(editing())}
              description={t("coupons.currencyHint")}
              value={form().currency}
              options={[
                { value: "", label: "—" },
                ...CURRENCIES.map((c) => ({ value: c, label: c })),
              ]}
              onChange={(c) => set({ currency: c as Currency | "" })}
            />
            <TextField
              label={t("coupons.minSubtotal")}
              error={minimum(form().minSubtotal) === undefined && t("errors.invalid_min_subtotal")}
              disabled={isLocked(editing())}
              value={form().minSubtotal}
              onChange={(minSubtotal) => set({ minSubtotal })}
              inputMode="decimal"
              inputClass="figures text-right"
            />
            <TextField
              label={t("coupons.usageLimit")}
              error={limit(form().usageLimit) === undefined && t("errors.invalid_quantity")}
              description={t("coupons.limitHint")}
              value={form().usageLimit}
              onChange={(usageLimit) => set({ usageLimit })}
              inputMode="numeric"
              inputClass="figures text-right"
            />
            <TextField
              label={t("coupons.perCustomer")}
              error={limit(form().perCustomer) === undefined && t("errors.invalid_quantity")}
              description={t("coupons.limitHint")}
              value={form().perCustomer}
              onChange={(perCustomer) => set({ perCustomer })}
              inputMode="numeric"
              inputClass="figures text-right"
            />
            <DateTimeField
              label={t("sales.startsAt")}
              disabled={isLocked(editing())}
              value={form().startsAt}
              onChange={(startsAt) => set({ startsAt })}
            />
            <DateTimeField
              label={t("sales.endsAt")}
              value={form().endsAt}
              onChange={(endsAt) => set({ endsAt })}
            />
          </div>
          <Checkbox
            label={t("coupons.published")}
            disabled={isLocked(editing())}
            checked={form().published}
            onChange={(published) => set({ published })}
          />
          <Show when={isLocked(editing())}>
            <Alert tone="info">{t("errors.coupon_started")}</Alert>
          </Show>
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
        </form>
      </Dialog>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("coupons.deleteTitle", { code: deleting()?.code ?? "" })}
        description={t("coupons.deleteDesc")}
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
