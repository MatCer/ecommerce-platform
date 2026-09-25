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
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { contentLocales, errorMessage, t } from "../i18n/index.ts";
import { api, idempotencyKey, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES, codify, compactI18n } from "../lib/product-form.ts";
import { useParameters } from "../lib/queries.ts";

type Parameter = Schemas["Parameter"];
type Kind = Schemas["ParameterKind"];
type Input = Schemas["ParameterInput"];
const KINDS: Kind[] = ["text", "number", "bool"];

const blank = (): Input => ({
  key: "",
  kind: "text",
  name_i18n: {},
  unit: null,
  filterable: false,
});

function displayName(p: Parameter): string {
  for (const l of contentLocales()) if (p.name_i18n[l]) return p.name_i18n[l];
  return p.key;
}

export default function Parameters() {
  const qc = useQueryClient();
  const list = useParameters();
  const [form, setForm] = createSignal<Input>(blank());
  const [editing, setEditing] = createSignal<Parameter | "new" | null>(null);
  const [deleting, setDeleting] = createSignal<Parameter | null>(null);
  const [error, setError] = createSignal<string>();

  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("parameters") });
  const open = (p: Parameter | "new") => {
    setForm(
      p === "new"
        ? blank()
        : {
            key: p.key,
            kind: p.kind,
            name_i18n: p.name_i18n,
            unit: p.unit ?? null,
            filterable: p.filterable,
          },
    );
    setError(undefined);
    setEditing(p);
  };

  const save = createMutation(() => ({
    mutationFn: () => {
      const f = form();
      const body: Input = {
        ...f,
        key: f.key.trim() || codify(Object.values(f.name_i18n)[0] ?? ""),
        name_i18n: compactI18n(f.name_i18n),
        unit: f.unit?.trim() || null,
      };
      const current = editing();
      return current === "new" || current === null
        ? unwrap(api.POST("/admin/v1/parameters", { params: { header: idempotencyKey() }, body }))
        : unwrap(
            api.PUT("/admin/v1/parameters/{id}", {
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
        api.DELETE("/admin/v1/parameters/{id}", {
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

  const newButton = () => (
    <Button variant="primary" onClick={() => open("new")}>
      {t("parameters.new")}
    </Button>
  );

  return (
    <>
      <PageHeader title={t("parameters.title")} actions={newButton()} />
      <QueryState query={list}>
        {(page) => (
          <Show
            when={page.items.length > 0}
            fallback={
              <EmptyState
                title={t("parameters.emptyTitle")}
                description={t("parameters.emptyDesc")}
                action={newButton()}
              />
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("products.colName")}</Th>
                    <Th>{t("parameters.key")}</Th>
                    <Th>{t("parameters.kind")}</Th>
                    <Th>{t("parameters.unit")}</Th>
                    <Th>{t("parameters.filterable")}</Th>
                    <Th srOnly>{t("common.actions")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={page.items}>
                    {(p) => (
                      <tr class="hover:bg-muted">
                        <td class={`${tdClass} font-medium`}>{displayName(p)}</td>
                        <td class={`${tdClass} figures text-xs`}>{p.key}</td>
                        <td class={tdClass}>{t(`parameters.kind_${p.kind}`)}</td>
                        <td class={`${tdClass} figures text-xs`}>{p.unit ?? "—"}</td>
                        <td class={tdClass}>
                          <Show
                            when={p.filterable}
                            fallback={<span class="text-faint-foreground">{t("common.no")}</span>}
                          >
                            <Badge tone="info">{t("common.yes")}</Badge>
                          </Show>
                        </td>
                        <td class={`${tdClass} text-right whitespace-nowrap`}>
                          <Button variant="ghost" onClick={() => open(p)}>
                            {t("common.edit")}
                            <span class="sr-only">: {displayName(p)}</span>
                          </Button>
                          <Button variant="ghost" onClick={() => setDeleting(p)}>
                            {t("common.delete")}
                            <span class="sr-only">: {displayName(p)}</span>
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
        title={editing() === "new" ? t("parameters.new") : t("parameters.editTitle")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <For each={CONTENT_LOCALES}>
            {(l) => (
              <TextField
                label={t("parameters.name", { locale: l })}
                value={form().name_i18n[l] ?? ""}
                maxLength={100}
                onChange={(v) => setForm({ ...form(), name_i18n: { ...form().name_i18n, [l]: v } })}
              />
            )}
          </For>
          <div class="grid grid-cols-2 gap-2">
            <TextField
              label={t("parameters.key")}
              description={t("parameters.keyHint")}
              value={form().key}
              inputClass="figures"
              placeholder={codify(Object.values(compactI18n(form().name_i18n))[0] ?? "")}
              maxLength={64}
              onChange={(v) => setForm({ ...form(), key: v })}
            />
            <SelectField
              label={t("parameters.kind")}
              value={form().kind}
              options={KINDS.map((k) => ({ value: k, label: t(`parameters.kind_${k}`) }))}
              onChange={(v) => setForm({ ...form(), kind: v as Kind })}
            />
          </div>
          <Show when={form().kind === "number"}>
            <TextField
              label={t("parameters.unit")}
              description={t("parameters.unitHint")}
              value={form().unit ?? ""}
              maxLength={20}
              onChange={(v) => setForm({ ...form(), unit: v })}
            />
          </Show>
          <Checkbox
            label={t("parameters.filterable")}
            checked={form().filterable ?? false}
            onChange={(v) => setForm({ ...form(), filterable: v })}
          />
          <Show when={error()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {error()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={() => setEditing(null)}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              variant="primary"
              loading={save.isPending}
              disabled={Object.keys(compactI18n(form().name_i18n)).length === 0}
            >
              {editing() === "new" ? t("common.create") : t("common.save")}
            </Button>
          </div>
        </form>
      </Dialog>

      <ConfirmDialog
        open={deleting() !== null}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("parameters.deleteTitle", {
          name: deleting() ? displayName(deleting() as Parameter) : "",
        })}
        description={t("parameters.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const p = deleting();
          if (p) remove.mutate(p.id);
        }}
      />
    </>
  );
}
