import {
  Button,
  ConfirmDialog,
  Dialog,
  EmptyState,
  Menu,
  SelectField,
  showToast,
  TextField,
} from "@platform/ui";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { AiPanel } from "../components/AiPanel.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { contentLocales, errorMessage, t } from "../i18n/index.ts";
import { api, idempotencyKey, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import {
  type CategoryMove,
  type CategoryNode,
  categoryName,
  flatten,
  indent,
  moveDown,
  moveUp,
  outdent,
  type Row,
  subtreeIds,
} from "../lib/category-tree.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES, type ContentLocale, slugify } from "../lib/product-form.ts";
import { categoryOptions, useCategoryTree } from "../lib/queries.ts";

type Translation = Schemas["CategoryTranslation"];
type Names = Record<ContentLocale, { name: string; slug: string }>;

const emptyNames = (): Names => ({
  cs: { name: "", slug: "" },
  sk: { name: "", slug: "" },
  en: { name: "", slug: "" },
});

function namesOf(node: CategoryNode | undefined): Names {
  const names = emptyNames();
  for (const tr of node?.translations ?? []) {
    if (tr.locale in names) names[tr.locale as ContentLocale] = { name: tr.name, slug: tr.slug };
  }
  return names;
}

/** Filled locales as translations; keeps descriptions/SEO fields of existing translations. */
function translationsOf(names: Names, existing: readonly Translation[] = []): Translation[] {
  return CONTENT_LOCALES.filter((l) => names[l].name.trim()).map((l) => ({
    ...existing.find((e) => e.locale === l),
    locale: l,
    name: names[l].name.trim(),
    slug: names[l].slug.trim() || slugify(names[l].name),
  }));
}

function NamesFields(props: { names: Names; onChange: (n: Names) => void }) {
  return (
    <div class="flex flex-col gap-2">
      <p class="text-xs text-muted-foreground">{t("categories.nameHint")}</p>
      <For each={CONTENT_LOCALES}>
        {(l) => (
          <div class="grid grid-cols-2 gap-2">
            <TextField
              label={t("categories.name", { locale: l })}
              value={props.names[l].name}
              maxLength={200}
              onChange={(name) =>
                props.onChange({ ...props.names, [l]: { ...props.names[l], name } })
              }
            />
            <TextField
              label={t("categories.slug", { locale: l })}
              value={props.names[l].slug}
              inputClass="figures"
              placeholder={slugify(props.names[l].name)}
              maxLength={200}
              onChange={(slug) =>
                props.onChange({ ...props.names, [l]: { ...props.names[l], slug } })
              }
            />
          </div>
        )}
      </For>
    </div>
  );
}

type Editing =
  | { kind: "create"; parentId: string }
  | { kind: "edit"; node: CategoryNode }
  | { kind: "move"; row: Row }
  | { kind: "delete"; node: CategoryNode };

export default function Categories() {
  const qc = useQueryClient();
  const tree = useCategoryTree();
  const [editing, setEditing] = createSignal<Editing | null>(null);
  const [names, setNames] = createSignal<Names>(emptyNames());
  const [parent, setParent] = createSignal("");
  const [formError, setFormError] = createSignal<string>();

  const refresh = () => qc.invalidateQueries({ queryKey: tenantKey("categories") });
  const editedId = () => {
    const ed = editing();
    return ed?.kind === "edit" ? ed.node.id : undefined;
  };
  const close = () => {
    setEditing(null);
    setFormError(undefined);
  };
  const name = (n: CategoryNode) => categoryName(n, contentLocales());
  const toastError = (err: unknown) =>
    showToast({ title: errorMessage(err), tone: "error", closeLabel: t("common.close") });

  const create = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/categories", {
          params: { header: idempotencyKey() },
          body: { parent_id: parent() || null, translations: translationsOf(names()) },
        }),
      ),
    onSuccess: async () => {
      close();
      await refresh();
      showToast({ title: t("common.created"), closeLabel: t("common.close") });
    },
    onError: (err) => setFormError(errorMessage(err)),
  }));

  const update = createMutation(() => ({
    mutationFn: (node: CategoryNode) =>
      unwrap(
        api.PUT("/admin/v1/categories/{id}", {
          params: { header: tenantHeader(), path: { id: node.id } },
          body: {
            translations: translationsOf(names(), node.translations),
            image_asset_id: node.image_asset_id ?? null,
          },
        }),
      ),
    onSuccess: async () => {
      close();
      await refresh();
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (err) => setFormError(errorMessage(err)),
  }));

  const move = createMutation(() => ({
    mutationFn: (v: { id: string; body: CategoryMove; focus: string }) =>
      unwrap(
        api.POST("/admin/v1/categories/{id}/move", {
          params: { header: tenantHeader(), path: { id: v.id } },
          body: v.body,
        }),
      ),
    onSuccess: async (_, v) => {
      close();
      await refresh();
      // Rows are re-rendered: return focus to the control that was used, or the row.
      const el =
        document.querySelector<HTMLButtonElement>(`[data-focus="${v.focus}"]:not([disabled])`) ??
        document.querySelector<HTMLButtonElement>(
          `button[data-focus^="${v.id}:"]:not([disabled])`,
        ) ??
        document.querySelector<HTMLElement>(`[data-focus="${v.id}:row"]`);
      el?.focus();
      showToast({ title: t("categories.moved"), closeLabel: t("common.close") });
    },
    onError: toastError,
  }));

  const remove = createMutation(() => ({
    mutationFn: (id: string) =>
      unwrap(
        api.DELETE("/admin/v1/categories/{id}", {
          params: { header: tenantHeader(), path: { id } },
        }),
      ),
    onSuccess: async () => {
      close();
      await refresh();
      showToast({ title: t("common.deleted"), closeLabel: t("common.close") });
    },
    onError: (err) => {
      close();
      toastError(err);
    },
  }));

  const doMove = (row: Row, body: CategoryMove | null, action: string) => {
    // Ignore repeats while a move runs (buttons stay enabled so focus can return to them).
    if (body && !move.isPending)
      move.mutate({ id: row.node.id, body, focus: `${row.node.id}:${action}` });
  };

  const openCreate = (parentId = "") => {
    setNames(emptyNames());
    setParent(parentId);
    setFormError(undefined);
    setEditing({ kind: "create", parentId });
  };

  const iconButton = (
    row: Row,
    action: string,
    glyph: string,
    label: string,
    body: CategoryMove | null,
  ) => (
    <Button
      variant="ghost"
      class="w-8 px-0"
      data-focus={`${row.node.id}:${action}`}
      aria-label={`${label}: ${name(row.node)}`}
      title={label}
      disabled={!body}
      onClick={() => doMove(row, body, action)}
    >
      <span aria-hidden="true">{glyph}</span>
    </Button>
  );

  return (
    <>
      <PageHeader
        title={t("categories.title")}
        actions={
          <Button variant="primary" onClick={() => openCreate()}>
            {t("categories.new")}
          </Button>
        }
      />
      <QueryState query={tree}>
        {(data) => (
          <Show
            when={data.items.length > 0}
            fallback={
              <EmptyState
                title={t("categories.emptyTitle")}
                description={t("categories.emptyDesc")}
                action={
                  <Button variant="primary" onClick={() => openCreate()}>
                    {t("categories.new")}
                  </Button>
                }
              />
            }
          >
            {(() => {
              const rows = () => flatten(data.items);
              return (
                <ul aria-label={t("categories.treeLabel")} class="max-w-3xl border-t border-border">
                  <For each={rows()}>
                    {(row) => (
                      <li
                        class="flex h-row items-center gap-2 border-b border-border hover:bg-muted"
                        style={{ "padding-left": `${0.5 + row.depth * 1.5}rem` }}
                      >
                        <span
                          tabindex="-1"
                          data-focus={`${row.node.id}:row`}
                          class="min-w-0 flex-1 truncate rounded-sm"
                          classList={{ "font-medium": row.depth === 0 }}
                        >
                          <Show when={row.depth > 0}>
                            <span class="mr-1 text-faint-foreground" aria-hidden="true">
                              └
                            </span>
                          </Show>
                          {name(row.node)}
                          <span class="sr-only">
                            {` (${t("categories.level", { n: row.depth + 1 })})`}
                          </span>
                        </span>
                        <div class="flex shrink-0 items-center">
                          {iconButton(row, "up", "↑", t("common.moveUp"), moveUp(row))}
                          {iconButton(row, "down", "↓", t("common.moveDown"), moveDown(row))}
                          {iconButton(
                            row,
                            "outdent",
                            "←",
                            t("categories.outdent"),
                            outdent(rows(), row),
                          )}
                          {iconButton(
                            row,
                            "indent",
                            "→",
                            t("categories.indent"),
                            indent(rows(), row),
                          )}
                          <Menu
                            triggerLabel={`${t("common.actions")}: ${name(row.node)}`}
                            trigger={<span aria-hidden="true">⋯</span>}
                            items={[
                              {
                                label: t("common.edit"),
                                onSelect: () => {
                                  setNames(namesOf(row.node));
                                  setFormError(undefined);
                                  setEditing({ kind: "edit", node: row.node });
                                },
                              },
                              {
                                label: t("categories.moveTo"),
                                onSelect: () => {
                                  setParent(row.parentId ?? "");
                                  setEditing({ kind: "move", row });
                                },
                              },
                              {
                                label: t("categories.new"),
                                onSelect: () => openCreate(row.node.id),
                              },
                              {
                                label: t("common.delete"),
                                onSelect: () => setEditing({ kind: "delete", node: row.node }),
                              },
                            ]}
                          />
                        </div>
                      </li>
                    )}
                  </For>
                </ul>
              );
            })()}
          </Show>
        )}
      </QueryState>

      <Dialog
        open={editing()?.kind === "create" || editing()?.kind === "edit"}
        onOpenChange={(open) => !open && close()}
        title={editing()?.kind === "edit" ? t("categories.editTitle") : t("categories.new")}
      >
        <form
          class="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            const ed = editing();
            if (ed?.kind === "edit") update.mutate(ed.node);
            else create.mutate();
          }}
        >
          <Show when={editing()?.kind === "create"}>
            <SelectField
              label={t("categories.parent")}
              value={parent()}
              options={[
                { value: "", label: t("categories.topLevel") },
                ...categoryOptions(tree.data?.items ?? [], contentLocales()),
              ]}
              onChange={setParent}
            />
          </Show>
          <NamesFields names={names()} onChange={setNames} />
          <Show when={editedId()}>
            {(id) => (
              <AiPanel
                entityType="category"
                entityId={id()}
                onAccepted={() => {
                  void refresh();
                  close();
                }}
                acceptHint={t("ai.dialogHint")}
              />
            )}
          </Show>
          <Show when={formError()}>
            <p role="alert" class="text-xs font-medium text-error-700">
              {formError()}
            </p>
          </Show>
          <div class="flex justify-end gap-2">
            <Button onClick={close}>{t("common.cancel")}</Button>
            <Button
              type="submit"
              variant="primary"
              loading={create.isPending || update.isPending}
              disabled={!CONTENT_LOCALES.some((l) => names()[l].name.trim())}
            >
              {editing()?.kind === "edit" ? t("common.save") : t("common.create")}
            </Button>
          </div>
        </form>
      </Dialog>

      <Dialog
        open={editing()?.kind === "move"}
        onOpenChange={(open) => !open && close()}
        title={t("categories.moveTo")}
        size="sm"
      >
        {(() => {
          const ed = editing();
          if (ed?.kind !== "move") return null;
          return (
            <form
              class="flex flex-col gap-3"
              onSubmit={(e) => {
                e.preventDefault();
                // Larger positions append at the end of the new siblings.
                doMove(ed.row, { parent_id: parent() || null, position: 1_000_000 }, "row");
              }}
            >
              <SelectField
                label={t("categories.parent")}
                value={parent()}
                options={[
                  { value: "", label: t("categories.topLevel") },
                  ...categoryOptions(
                    tree.data?.items ?? [],
                    contentLocales(),
                    subtreeIds(ed.row.node),
                  ),
                ]}
                onChange={setParent}
              />
              <div class="flex justify-end gap-2">
                <Button onClick={close}>{t("common.cancel")}</Button>
                <Button type="submit" variant="primary" loading={move.isPending}>
                  {t("common.save")}
                </Button>
              </div>
            </form>
          );
        })()}
      </Dialog>

      <ConfirmDialog
        open={editing()?.kind === "delete"}
        onOpenChange={(open) => !open && close()}
        title={(() => {
          const ed = editing();
          return t("categories.deleteTitle", { name: ed?.kind === "delete" ? name(ed.node) : "" });
        })()}
        description={t("categories.deleteDesc")}
        confirmLabel={t("common.delete")}
        cancelLabel={t("common.cancel")}
        danger
        pending={remove.isPending}
        onConfirm={() => {
          const ed = editing();
          if (ed?.kind === "delete") remove.mutate(ed.node.id);
        }}
      />
    </>
  );
}
