import { Button, SelectField, showToast, TextField } from "@platform/ui";
import { createMutation, createQuery } from "@tanstack/solid-query";
import { createEffect, createSignal, For, Index, Show } from "solid-js";
import { AiPanel } from "../components/AiPanel.tsx";
import { PageHeader, QueryState } from "../components/Page.tsx";
import { ProductPicker } from "../components/ProductPicker.tsx";
import { t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { contentError, firstContent, useContentPages } from "../lib/content-api.ts";
import {
  countMenu,
  type MenuEntry,
  moveItem,
  removeItem,
  updateMenu,
  validContentHref,
} from "../lib/content-form.ts";
import { tenantKey } from "../lib/me.ts";
import { CONTENT_LOCALES, compactI18n } from "../lib/product-form.ts";
import { categoryOptions, useCategoryTree } from "../lib/queries.ts";

const blank = (): MenuEntry => ({ link: { type: "url", url: "" }, label_i18n: {}, children: [] });
export default function ContentMenus() {
  const menus = createQuery(() => ({
    queryKey: tenantKey("menus"),
    queryFn: () => unwrap(api.GET("/admin/v1/menus", { params: { header: tenantHeader() } })),
  }));
  const categories = useCategoryTree(),
    pages = useContentPages();
  const [handle, setHandle] = createSignal("main"),
    [items, setItems] = createSignal<MenuEntry[]>([]),
    [loaded, setLoaded] = createSignal(""),
    [error, setError] = createSignal<string>();
  createEffect(() => {
    const key = JSON.stringify(tenantKey(handle()));
    if (menus.data && loaded() !== key) {
      setItems(menus.data.items.find((m) => m.handle === handle())?.items ?? []);
      setLoaded(key);
      setError(undefined);
    }
  });
  const clean = (e: MenuEntry): MenuEntry => ({
    ...e,
    label_i18n: compactI18n(e.label_i18n ?? {}),
    children: (e.children ?? []).map(clean),
  });
  const save = createMutation(() => ({
    mutationFn: () => {
      setError(undefined);
      return unwrap(
        api.PUT("/admin/v1/menus/{handle}", {
          params: { header: tenantHeader(), path: { handle: handle() } },
          body: { items: items().map(clean) },
        }),
      );
    },
    onSuccess: () => {
      void menus.refetch();
      showToast({ title: t("common.saved"), closeLabel: t("common.close") });
    },
    onError: (e: unknown) => setError(contentError(e)),
  }));
  const fields = (entry: () => MenuEntry, path: [number] | [number, number]) => {
    const change = (fn: (e: MenuEntry) => MenuEntry) =>
      setItems((list) => updateMenu(list, path, fn));
    const target = () => {
      const link = entry().link;
      return link.type === "url" ? link.url : link.id;
    };
    const setTarget = (value: string) =>
      change((e) => ({
        ...e,
        link:
          e.link.type === "url" ? { type: "url", url: value } : { type: e.link.type, id: value },
      }));
    return (
      <div class="grid gap-3 sm:grid-cols-2">
        <SelectField
          label={t("content.linkType")}
          value={entry().link.type}
          options={["category", "product", "page", "url"].map((type) => ({
            value: type,
            label: t(`content.${type as "category" | "product" | "page" | "url"}`),
          }))}
          onChange={(v) =>
            change((e) => ({
              ...e,
              link:
                v === "url"
                  ? { type: "url", url: "" }
                  : { type: v as "category" | "product" | "page", id: "" },
            }))
          }
        />
        <Show when={entry().link.type === "url"}>
          <TextField
            label={t("content.href")}
            value={target()}
            onChange={setTarget}
            description={t("content.hrefHint")}
            error={!!target() && !validContentHref(target()) && t("content.hrefHint")}
          />
        </Show>
        <Show when={entry().link.type === "category"}>
          <QueryState query={categories}>
            {(data) => (
              <SelectField
                label={t("content.target")}
                value={target()}
                onChange={setTarget}
                options={[
                  { value: "", label: t("common.none") },
                  ...categoryOptions(data.items, CONTENT_LOCALES),
                ]}
              />
            )}
          </QueryState>
        </Show>
        <Show when={entry().link.type === "page"}>
          <QueryState query={pages}>
            {(data) => (
              <SelectField
                label={t("content.target")}
                value={target()}
                onChange={setTarget}
                options={[
                  { value: "", label: t("common.none") },
                  ...data.items.map((p) => ({ value: p.id, label: firstContent(p.title) })),
                ]}
              />
            )}
          </QueryState>
        </Show>
        <Show when={entry().link.type === "product"}>
          <ProductPicker
            value={target() ? [target()] : []}
            onChange={(ids) => setTarget(ids.at(-1) ?? "")}
          />
        </Show>
        <For each={CONTENT_LOCALES}>
          {(l) => (
            <TextField
              label={`${t("content.label")} (${l})`}
              value={entry().label_i18n?.[l] ?? ""}
              onChange={(value) =>
                change((e) => ({ ...e, label_i18n: { ...e.label_i18n, [l]: value } }))
              }
            />
          )}
        </For>
      </div>
    );
  };
  const controls = (
    i: number,
    count: number,
    move: (d: number) => void,
    remove: () => void,
    label: string,
  ) => (
    <div class="my-2 flex flex-wrap gap-1">
      <Button
        disabled={i === 0}
        aria-label={`${t("common.moveUp")}: ${label}`}
        onClick={() => move(-1)}
      >
        {t("common.moveUp")}
      </Button>
      <Button
        disabled={i === count - 1}
        aria-label={`${t("common.moveDown")}: ${label}`}
        onClick={() => move(1)}
      >
        {t("common.moveDown")}
      </Button>
      <Button aria-label={`${t("common.remove")}: ${label}`} onClick={remove}>
        {t("common.remove")}
      </Button>
    </div>
  );
  return (
    <>
      <PageHeader title={t("content.menus")} />
      <QueryState query={menus}>
        {(data) => (
          <form
            class="flex max-w-4xl flex-col gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <SelectField
              label={t("content.handle")}
              value={handle()}
              disabled={save.isPending}
              onChange={setHandle}
              options={[...new Set(["main", "footer", ...data.items.map((m) => m.handle)])].map(
                (value) => ({
                  value,
                  label:
                    value === "main"
                      ? t("content.main")
                      : value === "footer"
                        ? t("content.footer")
                        : value,
                }),
              )}
            />
            <p role="status" class="text-sm">
              {t("content.entryCount", { count: countMenu(items()) })}
            </p>
            <Show when={error()}>
              <p role="alert" class="text-error-700">
                {error()}
              </p>
            </Show>
            <Index each={items()}>
              {(entry, i) => (
                <fieldset class="min-w-0 rounded-md border border-border p-3">
                  <legend>
                    {t("content.entry")} {i + 1}
                  </legend>
                  {controls(
                    i,
                    items().length,
                    (d) => setItems(moveItem(items(), i, d)),
                    () => setItems(removeItem(items(), i)),
                    String(i + 1),
                  )}
                  {fields(entry, [i])}
                  <div class="ml-2 mt-3 flex flex-col gap-3 sm:ml-6">
                    <Index each={entry().children ?? []}>
                      {(child, j) => (
                        <fieldset class="min-w-0 border-t border-border pt-2">
                          <legend>
                            {t("content.entry")} {i + 1}.{j + 1}
                          </legend>
                          {controls(
                            j,
                            entry().children?.length ?? 0,
                            (d) =>
                              setItems(
                                updateMenu(items(), [i], (e) => ({
                                  ...e,
                                  children: moveItem(e.children ?? [], j, d),
                                })),
                              ),
                            () =>
                              setItems(
                                updateMenu(items(), [i], (e) => ({
                                  ...e,
                                  children: removeItem(e.children ?? [], j),
                                })),
                              ),
                            `${i + 1}.${j + 1}`,
                          )}
                          {fields(child, [i, j])}
                        </fieldset>
                      )}
                    </Index>
                  </div>
                  <Button
                    class="mt-3"
                    onClick={() =>
                      setItems(
                        updateMenu(items(), [i], (e) => ({
                          ...e,
                          children: [...(e.children ?? []), blank()],
                        })),
                      )
                    }
                  >
                    {t("content.addChild")}
                  </Button>
                </fieldset>
              )}
            </Index>
            <div class="flex flex-wrap gap-2">
              <Button onClick={() => setItems([...items(), blank()])}>
                {t("content.addEntry")}
              </Button>
              <Button type="submit" variant="confirm" loading={save.isPending}>
                {t("common.save")}
              </Button>
            </div>
            <Show when={data.items.some((m) => m.handle === handle())}>
              <AiPanel
                entityType="menu"
                entityId={handle()}
                onAccepted={() => {
                  setLoaded("");
                  void menus.refetch();
                }}
                acceptHint={t("ai.unsavedHint")}
              />
            </Show>
          </form>
        )}
      </QueryState>
    </>
  );
}
