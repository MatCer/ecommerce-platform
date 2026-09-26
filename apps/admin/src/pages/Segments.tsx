import {
  Button,
  Checkbox,
  ConfirmDialog,
  Dialog,
  EmptyState,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Index, type JSX, Match, Show, Switch } from "solid-js";
import { DateTimeField } from "../components/DateTimeField.tsx";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { contentLocales, errorMessage, formatDateTime, LOCALES, t } from "../i18n/index.ts";
import { api, type Schemas, submission, tenantHeader, unwrap } from "../lib/api.ts";
import { removeItem } from "../lib/content-form.ts";
import {
  blankCondition,
  CONDITION_FIELDS,
  type ConditionField,
  type ConditionForm,
  fromCondition,
  MAX_CONDITIONS,
  toRules,
} from "../lib/marketing.ts";
import { tenantKey } from "../lib/me.ts";
import { CURRENCIES } from "../lib/money.ts";
import { categoryOptions, useCategoryTree, useMarkets, useSegments } from "../lib/queries.ts";

type Segment = Schemas["Segment"];
type Preview = Schemas["Preview"];

interface Form {
  name: string;
  match: "all" | "any";
  conditions: ConditionForm[];
}

/** Ticks a value in or out of a list. */
const toggle = (list: string[], value: string, on: boolean) =>
  on ? [...list, value] : list.filter((x) => x !== value);

function CheckList(props: {
  legend: string;
  options: { value: string; label: string }[];
  value: string[];
  onChange: (v: string[]) => void;
}) {
  return (
    <fieldset class="flex max-h-48 flex-col gap-1 overflow-y-auto">
      <legend class="mb-1 text-xs font-medium text-muted-foreground">{props.legend}</legend>
      <For each={props.options}>
        {(o) => (
          <Checkbox
            label={o.label}
            checked={props.value.includes(o.value)}
            onChange={(on) => props.onChange(toggle(props.value, o.value, on))}
          />
        )}
      </For>
    </fieldset>
  );
}

/** Inputs of one condition, by field. */
function ConditionFields(props: { c: ConditionForm; onChange: (c: ConditionForm) => void }) {
  const markets = useMarkets();
  const categories = useCategoryTree();
  const set = (patch: Partial<ConditionForm>) => props.onChange({ ...props.c, ...patch });
  const categoryList = () => (
    <CheckList
      legend={t("marketing.categories")}
      options={categoryOptions(categories.data?.items ?? [], contentLocales())}
      value={props.c.ids}
      onChange={(ids) => set({ ids })}
    />
  );
  const brands = () => (
    <TextField
      label={t("marketing.brands")}
      description={t("marketing.commaHint")}
      value={props.c.text}
      onChange={(text) => set({ text })}
    />
  );
  const range = (label: string, hint: string, money: boolean): JSX.Element => (
    <div class="grid grid-cols-2 gap-2">
      <TextField
        label={`${label}: ${t("marketing.min")}`}
        inputMode={money ? "decimal" : "numeric"}
        description={hint}
        value={props.c.min}
        onChange={(min) => set({ min })}
      />
      <TextField
        label={`${label}: ${t("marketing.max")}`}
        inputMode={money ? "decimal" : "numeric"}
        value={props.c.max}
        onChange={(max) => set({ max })}
      />
    </div>
  );
  return (
    <Switch>
      <Match when={props.c.field === "locale"}>
        <CheckList
          legend={t("marketing.languages")}
          options={LOCALES.map((l) => ({ value: l, label: t(`common.locale_${l}`) }))}
          value={props.c.ids}
          onChange={(ids) => set({ ids })}
        />
      </Match>
      <Match when={props.c.field === "market"}>
        <CheckList
          legend={t("marketing.markets")}
          options={(markets.data?.items ?? []).map((m) => ({ value: m.id, label: m.name }))}
          value={props.c.ids}
          onChange={(ids) => set({ ids })}
        />
      </Match>
      <Match when={props.c.field === "subscribed" || props.c.field === "last_order"}>
        <div class="grid grid-cols-2 gap-2">
          <DateTimeField
            label={t("marketing.after")}
            value={props.c.after}
            onChange={(after) => set({ after })}
          />
          <DateTimeField
            label={t("marketing.before")}
            value={props.c.before}
            onChange={(before) => set({ before })}
          />
        </div>
      </Match>
      <Match when={props.c.field === "purchased_category"}>{categoryList()}</Match>
      <Match when={props.c.field === "purchased_brand"}>{brands()}</Match>
      <Match when={props.c.field === "order_count"}>
        {range(t("marketing.orders"), t("marketing.boundHint"), false)}
      </Match>
      <Match when={props.c.field === "total_spent"}>
        <SelectField
          label={t("marketing.currency")}
          value={props.c.currency}
          options={CURRENCIES.map((c) => ({ value: c, label: c }))}
          onChange={(currency) => set({ currency })}
        />
        {range(t("marketing.amount"), t("marketing.boundHint"), true)}
      </Match>
      <Match when={props.c.field === "engaged"}>
        <TextField
          label={t("marketing.days")}
          description={t("marketing.engagedHint")}
          inputMode="numeric"
          value={props.c.days}
          onChange={(days) => set({ days })}
        />
      </Match>
      <Match when={props.c.field === "affinity"}>
        <SelectField
          label={t("marketing.interestIn")}
          value={props.c.dim}
          options={[
            { value: "category", label: t("marketing.categories") },
            { value: "brand", label: t("marketing.brands") },
          ]}
          onChange={(v) => set({ dim: v === "brand" ? "brand" : "category" })}
        />
        <Show when={props.c.dim === "category"} fallback={brands()}>
          {categoryList()}
        </Show>
        <p class="text-xs text-muted-foreground">{t("marketing.affinityHint")}</p>
      </Match>
    </Switch>
  );
}

/** Match all/any, the conditions, and adding a condition by field. */
export function RulesEditor(props: {
  match: "all" | "any";
  conditions: ConditionForm[];
  invalid?: number;
  onChange: (match: "all" | "any", conditions: ConditionForm[]) => void;
}) {
  const [adding, setAdding] = createSignal<ConditionField>("locale");
  const markets = useMarkets();
  return (
    <section class="flex flex-col gap-3" aria-label={t("marketing.conditions")}>
      <SelectField
        label={t("marketing.match")}
        value={props.match}
        options={[
          { value: "all", label: t("marketing.matchAll") },
          { value: "any", label: t("marketing.matchAny") },
        ]}
        onChange={(v) => props.onChange(v === "any" ? "any" : "all", props.conditions)}
      />
      <Show when={props.conditions.length === 0}>
        <p class="text-xs text-muted-foreground">{t("marketing.noConditions")}</p>
      </Show>
      <Index each={props.conditions}>
        {(c, i) => (
          <fieldset
            class="flex min-w-0 flex-col gap-2 rounded-md border p-3"
            classList={{
              "border-error-600": props.invalid === i,
              "border-border": props.invalid !== i,
            }}
          >
            <legend class="px-1 text-sm font-medium">
              {i + 1}. {t(`marketing.field_${c().field}`)}
            </legend>
            <ConditionFields
              c={c()}
              onChange={(next) =>
                props.onChange(
                  props.match,
                  props.conditions.map((x, j) => (i === j ? next : x)),
                )
              }
            />
            <Show when={props.invalid === i}>
              <p role="alert" class="text-xs font-medium text-error-700">
                {t("marketing.conditionIncomplete")}
              </p>
            </Show>
            <div>
              <Button
                aria-label={`${t("common.remove")}: ${i + 1}. ${t(`marketing.field_${c().field}`)}`}
                onClick={() => props.onChange(props.match, removeItem(props.conditions, i))}
              >
                {t("common.remove")}
              </Button>
            </div>
          </fieldset>
        )}
      </Index>
      <div class="flex flex-wrap items-end gap-2">
        <SelectField
          label={t("marketing.newCondition")}
          value={adding()}
          options={CONDITION_FIELDS.map((f) => ({ value: f, label: t(`marketing.field_${f}`) }))}
          onChange={(v) => setAdding(CONDITION_FIELDS.find((f) => f === v) ?? "locale")}
        />
        <Button
          disabled={props.conditions.length >= MAX_CONDITIONS}
          onClick={() =>
            props.onChange(props.match, [
              ...props.conditions,
              blankCondition(adding(), markets.data?.items[0]?.currency),
            ])
          }
        >
          {t("marketing.addCondition")}
        </Button>
      </div>
    </section>
  );
}

/** Count and a few members of the rules right now. */
export function PreviewResult(props: { data: Preview }) {
  return (
    <div class="flex flex-col gap-2">
      <p role="status" class="text-sm font-medium">
        {t("marketing.matching", { n: String(props.data.count) })}
      </p>
      <Show when={props.data.sample.length > 0}>
        <table class={tableClass} aria-label={t("marketing.sample")}>
          <thead>
            <tr>
              <Th>{t("marketing.email")}</Th>
              <Th>{t("marketing.language")}</Th>
            </tr>
          </thead>
          <tbody>
            <For each={props.data.sample}>
              {(m) => (
                <tr>
                  <td class={tdClass}>{m.email}</td>
                  <td class={`${tdClass} text-xs uppercase`}>{m.locale}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
}

const blank = (): Form => ({ name: "", match: "all", conditions: [] });

/** Segments (WP18): saved rules that pick campaign recipients among subscribed people. */
export default function Segments() {
  const qc = useQueryClient();
  const list = useSegments();
  const [editing, setEditing] = createSignal<Segment | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Segment | null>(null);
  const [form, setForm] = createSignal<Form>(blank());
  const [invalid, setInvalid] = createSignal<number>();
  const [error, setError] = createSignal<string>();
  const [preview, setPreview] = createSignal<Preview>();
  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("segments") });

  const rules = () => {
    const r = toRules(form().match, form().conditions);
    setInvalid("invalid" in r ? r.invalid : undefined);
    return "rules" in r ? r.rules : null;
  };

  const runPreview = createMutation(() => ({
    mutationFn: (body: Schemas["Rules"]) =>
      unwrap(api.POST("/admin/v1/segments/preview", { params: { header: tenantHeader() }, body })),
    onSuccess: (data) => setPreview(data),
    onError: (err) => setError(errorMessage(err)),
  }));

  const create = submission();
  const save = createMutation(() => ({
    mutationFn: ({ target, body }: { target: Segment | "new"; body: Schemas["SegmentInput"] }) =>
      target === "new"
        ? unwrap(api.POST("/admin/v1/segments", { params: { header: create.header(body) }, body }))
        : unwrap(
            api.PUT("/admin/v1/segments/{id}", {
              params: { header: tenantHeader(), path: { id: target.id } },
              body,
            }),
          ),
    onSuccess: async (_, { target }) => {
      if (target === "new") create.done();
      if (editing() === target) setEditing(null);
      await refresh();
      showToast({
        title: target === "new" ? t("common.created") : t("common.saved"),
        closeLabel: t("common.close"),
      });
    },
    onError: (err, { target }) => {
      if (editing() === target) setError(errorMessage(err));
    },
  }));

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/segments/{id}", { params: { header: tenantHeader(), path: { id } } }),
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

  const open = (s: Segment | "new") => {
    setForm(
      s === "new"
        ? blank()
        : {
            name: s.name,
            match: s.rules.match ?? "all",
            conditions: (s.rules.conditions ?? []).map(fromCondition),
          },
    );
    setError(undefined);
    setInvalid(undefined);
    setPreview(undefined);
    setEditing(s);
  };
  const newButton = () => (
    <Button variant="confirm" onClick={() => open("new")}>
      {t("marketing.newSegment")}
    </Button>
  );

  return (
    <>
      <PageHeader
        title={t("marketing.segments")}
        description={t("marketing.segmentsDesc")}
        actions={newButton()}
      />
      <QueryState query={list}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <EmptyState
                title={t("marketing.noSegments")}
                description={t("marketing.noSegmentsDesc")}
                action={newButton()}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("marketing.name")}</Th>
                    <Th class="text-right">{t("marketing.conditions")}</Th>
                    <Th>{t("marketing.updated")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data.items}>
                    {(s) => (
                      <tr>
                        <td class={`${tdClass} font-medium`}>{s.name}</td>
                        <td class={`${tdClass} figures text-right`}>
                          {s.rules.conditions?.length ?? 0}
                        </td>
                        <td
                          class={`${tdClass} figures text-xs whitespace-nowrap text-muted-foreground`}
                        >
                          {formatDateTime(s.updated_at)}
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
          </Show>
        )}
      </QueryState>

      <Dialog
        open={editing() !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        title={editing() === "new" ? t("marketing.newSegment") : t("marketing.editSegment")}
        size="md"
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            const target = editing();
            const r = rules();
            if (target && r) save.mutate({ target, body: { name: form().name.trim(), rules: r } });
          }}
        >
          <TextField
            label={t("marketing.name")}
            value={form().name}
            onChange={(name) => setForm({ ...form(), name })}
            required
            maxLength={200}
          />
          <RulesEditor
            match={form().match}
            conditions={form().conditions}
            invalid={invalid()}
            onChange={(match, conditions) => {
              setForm({ ...form(), match, conditions });
              setPreview(undefined);
              setInvalid(undefined);
            }}
          />
          <div class="flex flex-col gap-2 rounded-md bg-muted p-3">
            <div>
              <Button
                loading={runPreview.isPending}
                onClick={() => {
                  setError(undefined);
                  const r = rules();
                  if (r) runPreview.mutate(r);
                }}
              >
                {t("marketing.preview")}
              </Button>
            </div>
            <Show when={preview()}>{(p) => <PreviewResult data={p()} />}</Show>
          </div>
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              variant="confirm"
              loading={save.isPending}
              disabled={!form().name.trim()}
            >
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("marketing.deleteSegment", { name: deleting()?.name ?? "" })}
        description={t("marketing.deleteSegmentDesc")}
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
