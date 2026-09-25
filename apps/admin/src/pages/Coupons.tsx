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
} from "@platform/ui";
import { createInfiniteQuery, createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, locale, t } from "../i18n/index.ts";
import { api, idempotencyKey, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import {
  CURRENCIES,
  type Currency,
  formatMoney,
  fromLocalInput,
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

const count = (s: string): number | null => (/^\d+$/.test(s.trim()) ? Number(s.trim()) : null);

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
    return f.code.trim() !== "" && discountOk && (!needsCurrency || f.currency !== "");
  };

  const save = createMutation(() => ({
    mutationFn: () => {
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
        min_subtotal_minor: f.minSubtotal.trim() === "" ? null : parseMoney(f.minSubtotal),
        usage_limit: count(f.usageLimit),
        per_customer_limit: count(f.perCustomer),
        starts_at: fromLocalInput(f.startsAt),
        ends_at: fromLocalInput(f.endsAt),
        published: f.published,
      };
      const current = editing();
      return current === "new" || current === null
        ? unwrap(api.POST("/admin/v1/coupons", { params: { header: idempotencyKey() }, body }))
        : unwrap(
            api.PUT("/admin/v1/coupons/{id}", {
              params: { header: tenantHeader(), path: { id: current.id } },
              body,
            }),
          );
    },
    onSuccess: async () => {
      const created = editing() === "new";
      setEditing(null);
      await refresh();
      showToast({
        title: created ? t("common.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
    },
    onError: (err) => setError(errorMessage(err)),
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
  const newButton = () => (
    <Button variant="primary" onClick={() => open("new")}>
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
                title={t("coupons.emptyTitle")}
                description={t("coupons.emptyDesc")}
                action={newButton()}
              />
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
                      <tr>
                        <td class={`${tdClass} figures font-medium`}>{c.code}</td>
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
                            {c.published ? t("coupons.live") : t("coupons.draft")}
                          </Badge>
                        </td>
                        <td class={`${tdClass} text-right whitespace-nowrap`}>
                          <Button variant="ghost" onClick={() => open(c)}>
                            {t("common.edit")}
                            <span class="sr-only">: {c.code}</span>
                          </Button>
                          <Button variant="ghost" onClick={() => setDeleting(c)}>
                            {t("common.delete")}
                            <span class="sr-only">: {c.code}</span>
                          </Button>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={coupons.hasNextPage}>
              <Button
                class="mt-3"
                loading={coupons.isFetchingNextPage}
                onClick={() => void coupons.fetchNextPage()}
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
        title={editing() === "new" ? t("coupons.new") : t("coupons.editTitle")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div class="grid grid-cols-2 gap-2">
            <TextField
              label={t("coupons.code")}
              value={form().code}
              onChange={(code) => set({ code: code.toUpperCase() })}
              inputClass="figures"
              required
              maxLength={64}
            />
            <SelectField
              label={t("coupons.discount")}
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
                value={form().amount}
                onChange={(amount) => set({ amount })}
                inputMode="decimal"
                inputClass="figures text-right"
                required
              />
            </Show>
            <SelectField
              label={t("sales.currency")}
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
              value={form().minSubtotal}
              onChange={(minSubtotal) => set({ minSubtotal })}
              inputMode="decimal"
              inputClass="figures text-right"
            />
            <TextField
              label={t("coupons.usageLimit")}
              description={t("coupons.limitHint")}
              value={form().usageLimit}
              onChange={(usageLimit) => set({ usageLimit })}
              inputMode="numeric"
              inputClass="figures text-right"
            />
            <TextField
              label={t("coupons.perCustomer")}
              description={t("coupons.limitHint")}
              value={form().perCustomer}
              onChange={(perCustomer) => set({ perCustomer })}
              inputMode="numeric"
              inputClass="figures text-right"
            />
            <DateTimeField
              label={t("sales.startsAt")}
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
            checked={form().published}
            onChange={(published) => set({ published })}
          />
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
