/** Shared tenant-scoped queries used by several screens. */
import { createQuery } from "@tanstack/solid-query";
import { api, type Schemas, tenantHeader, unwrap } from "./api.ts";
import { type CategoryNode, categoryName, flatten } from "./category-tree.ts";
import { tenantKey } from "./me.ts";

export function useCategoryTree() {
  return createQuery(() => ({
    queryKey: tenantKey("categories"),
    queryFn: () => unwrap(api.GET("/admin/v1/categories", { params: { header: tenantHeader() } })),
  }));
}

/**
 * Every parameter definition (all pages): the product editor needs the full set to edit the
 * values it already has. ponytail: bounded at 5 000 definitions; add server-side search if
 * tenants get near that.
 */
export function useParameters() {
  return createQuery(() => ({
    queryKey: tenantKey("parameters"),
    queryFn: async () => {
      const header = tenantHeader();
      const items: Schemas["Parameter"][] = [];
      let cursor: string | undefined;
      for (let page = 0; page < 50; page++) {
        const res = await unwrap(
          api.GET("/admin/v1/parameters", { params: { header, query: { limit: 100, cursor } } }),
        );
        items.push(...res.items);
        cursor = res.next_cursor ?? undefined;
        if (!cursor) break;
      }
      return { items };
    },
  }));
}

export function useTaxCategories() {
  return createQuery(() => ({
    queryKey: ["tax-categories"],
    queryFn: () =>
      unwrap(api.GET("/admin/v1/tax-categories", { params: { header: tenantHeader() } })),
    staleTime: 3_600_000,
  }));
}

export function useMarkets() {
  return createQuery(() => ({
    queryKey: tenantKey("markets"),
    queryFn: () => unwrap(api.GET("/admin/v1/markets", { params: { header: tenantHeader() } })),
  }));
}

/** Indented options for selecting a category ("Clothing / T-shirts" depth shown with dashes). */
export function categoryOptions(
  tree: readonly CategoryNode[],
  locales: readonly string[],
  exclude: ReadonlySet<string> = new Set(),
): { value: string; label: string }[] {
  return flatten(tree)
    .filter((r) => !exclude.has(r.node.id))
    .map((r) => ({
      value: r.node.id,
      label: `${"— ".repeat(r.depth)}${categoryName(r.node, locales)}`,
    }));
}
