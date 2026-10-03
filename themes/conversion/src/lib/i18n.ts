import { pick, pluralKeys, t, tn } from "@platform/storefront-sdk/format";
import type { ShopModel } from "@platform/storefront-sdk/types";

/**
 * Strings and links for one page. Every visible theme string comes from the platform catalog
 * (`shop.messages`, cs/sk/en), never from theme code, so a new market locale needs no theme
 * change. Page-model hrefs already carry the locale prefix; `href()` adds it to the few links
 * the theme builds itself.
 *
 * ```astro
 * const { t, href } = i18n(shop);
 * <a href={href("/search")}>{t("search.title")}</a>
 * ```
 */
export function i18n(shop: ShopModel) {
  const m = shop.messages;
  return {
    t: (key: string, args?: Record<string, string | number>) => t(m, key, args),
    /** Counted message (`listing.count.one|few|many|other`). */
    tn: (key: string, count: number, args?: Record<string, string | number>) =>
      tn(m, shop.locale, key, count, args),
    /** `/search` → `/cs/search` on a prefixed locale; `/` → `/cs`. */
    href: (path: string) => (shop.base_path ? shop.base_path + (path === "/" ? "" : path) : path),
    /** The subset of messages an island needs (islands serialize their props into the page). */
    pick: (keys: readonly string[]) => pick(m, keys),
    pluralKeys,
  };
}

/** Link to a product page (cards and search hits carry only the slug). */
export const productHref = (shop: ShopModel, slug: string) =>
  `${shop.base_path}/p/${encodeURIComponent(slug)}`;
