import { createQuery } from "@tanstack/solid-query";
import { errorMessage } from "../i18n/index.ts";
import { ApiError, api, type Schemas, tenantHeader, unwrap } from "./api.ts";
import { tenantKey } from "./me.ts";
import { CONTENT_LOCALES } from "./product-form.ts";

export function contentError(error: unknown): string {
  return error instanceof ApiError && error.detail ? error.detail : errorMessage(error);
}
export function firstContent(map: Record<string, string>): string {
  return CONTENT_LOCALES.map((l) => map[l]).find(Boolean) ?? Object.values(map)[0] ?? "—";
}
/** Fetch all pages for menu targets and lists; follow cursors rather than dropping legal pages. */
export function useContentPages() {
  return createQuery(() => ({
    queryKey: tenantKey("content-pages"),
    queryFn: async () => {
      const header = tenantHeader();
      const items: Schemas["PageSummary"][] = [];
      let cursor: string | undefined;
      do {
        const page = await unwrap(
          api.GET("/admin/v1/pages", { params: { header, query: { cursor, limit: 100 } } }),
        );
        items.push(...page.items);
        if (page.next_cursor && page.next_cursor === cursor)
          throw new Error("Repeated page cursor");
        cursor = page.next_cursor ?? undefined;
      } while (cursor);
      return { items };
    },
  }));
}
