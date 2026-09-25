import {
  Badge,
  Button,
  buttonClass,
  EmptyState,
  SelectField,
  TextField,
  type Tone,
} from "@platform/ui";
import { A, useSearchParams } from "@solidjs/router";
import { createInfiniteQuery } from "@tanstack/solid-query";
import { createEffect, createSignal, For, on, onCleanup, Show } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { contentLocales, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey } from "../lib/me.ts";
import { categoryOptions, useCategoryTree } from "../lib/queries.ts";

type Status = Schemas["ProductStatus"];
const STATUSES: Status[] = ["draft", "active", "archived"];

export const statusTone: Record<Status, Tone> = {
  draft: "neutral",
  active: "success",
  archived: "warning",
};

function pickName(name: Record<string, string>): string {
  for (const l of contentLocales()) if (name[l]) return name[l];
  return Object.values(name)[0] ?? t("products.untitled");
}

const str = (v: string | string[] | undefined) => (Array.isArray(v) ? v[0] : v) ?? "";

export default function Products() {
  const [params, setParams] = useSearchParams();
  const [search, setSearch] = createSignal(str(params.q));
  const categories = useCategoryTree();

  // Debounce the search box into the URL (the URL drives the query).
  createEffect(
    on(
      search,
      (value) => {
        const timer = setTimeout(() => setParams({ q: value.trim() || undefined }), 300);
        onCleanup(() => clearTimeout(timer));
      },
      { defer: true },
    ),
  );

  const filters = () => ({
    q: str(params.q) || undefined,
    status: (STATUSES as string[]).includes(str(params.status))
      ? (str(params.status) as Status)
      : undefined,
    category_id: str(params.category) || undefined,
  });

  const products = createInfiniteQuery(() => ({
    queryKey: tenantKey("products", filters()),
    queryFn: ({ pageParam }) =>
      unwrap(
        api.GET("/admin/v1/products", {
          params: {
            header: tenantHeader(),
            query: { ...filters(), limit: 50, cursor: pageParam },
          },
        }),
      ),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  }));

  const hasFilters = () => Boolean(params.q || params.status || params.category);
  const rows = () => products.data?.pages.flatMap((p) => p.items) ?? [];

  return (
    <>
      <PageHeader
        title={t("products.title")}
        actions={
          <A href="/products/new" class={buttonClass("primary")}>
            {t("products.new")}
          </A>
        }
      />
      <div class="mb-3 flex flex-wrap items-end gap-2">
        <TextField
          class="w-full sm:w-72"
          type="search"
          label={t("products.search")}
          value={search()}
          onChange={setSearch}
        />
        <SelectField
          class="w-40"
          label={t("products.status")}
          value={str(params.status)}
          options={[
            { value: "", label: t("products.allStatuses") },
            ...STATUSES.map((s) => ({ value: s, label: t(`status.${s}`) })),
          ]}
          onChange={(v) => setParams({ status: v || undefined })}
        />
        <SelectField
          class="w-56"
          label={t("products.category")}
          value={str(params.category)}
          options={[
            { value: "", label: t("products.allCategories") },
            ...categoryOptions(categories.data?.items ?? [], contentLocales()),
          ]}
          onChange={(v) => setParams({ category: v || undefined })}
        />
        <Show when={hasFilters()}>
          <Button
            variant="ghost"
            onClick={() => {
              setSearch("");
              setParams({ q: undefined, status: undefined, category: undefined });
            }}
          >
            {t("products.clearFilters")}
          </Button>
        </Show>
      </div>
      <QueryState query={products}>
        {() => (
          <Show
            when={rows().length > 0}
            fallback={
              <Show
                when={hasFilters()}
                fallback={
                  <EmptyState
                    title={t("products.emptyTitle")}
                    description={t("products.emptyDesc")}
                    action={
                      <A href="/products/new" class={buttonClass("primary")}>
                        {t("products.new")}
                      </A>
                    }
                  />
                }
              >
                <EmptyState title={t("products.noMatches")} />
              </Show>
            }
          >
            <div class="overflow-x-auto">
              <table class={tableClass}>
                <thead>
                  <tr>
                    <Th>{t("products.colName")}</Th>
                    <Th>{t("products.colSku")}</Th>
                    <Th class="text-right">{t("products.colVariants")}</Th>
                    <Th>{t("products.colStatus")}</Th>
                    <Th class="text-right">{t("products.colUpdated")}</Th>
                  </tr>
                </thead>
                <tbody>
                  <For each={rows()}>
                    {(p) => (
                      <tr class="hover:bg-muted">
                        <td class={`${tdClass} max-w-md`}>
                          <A
                            href={`/products/${p.id}`}
                            class="block truncate font-medium text-accent-700 hover:underline"
                          >
                            {pickName(p.name)}
                          </A>
                          <Show when={p.brand}>
                            <span class="text-xs text-muted-foreground">{p.brand}</span>
                          </Show>
                        </td>
                        <td class={`${tdClass} figures text-xs`}>{p.default_sku ?? "—"}</td>
                        <td class={`${tdClass} figures text-right`}>{p.variant_count}</td>
                        <td class={tdClass}>
                          <Badge tone={statusTone[p.status]}>{t(`status.${p.status}`)}</Badge>
                        </td>
                        <td class={`${tdClass} figures text-right text-xs text-faint-foreground`}>
                          {formatDateTime(p.updated_at)}
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <Show when={products.hasNextPage}>
              <div class="mt-3">
                <Button
                  loading={products.isFetchingNextPage}
                  onClick={() => void products.fetchNextPage()}
                >
                  {t("common.loadMore")}
                </Button>
              </div>
            </Show>
          </Show>
        )}
      </QueryState>
    </>
  );
}
