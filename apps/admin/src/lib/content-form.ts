import type { Schemas } from "./api.ts";

export { slugify } from "./product-form.ts";
export type Block = Schemas["Block"];
export type MenuEntry = Schemas["MenuEntry"];
export const BLOCK_TYPES = [
  "heading",
  "rich_text",
  "image",
  "button",
  "product_grid",
  "faq",
] as const;
export function blankBlock(type: Block["type"]): Block {
  switch (type) {
    case "heading":
      return { type, text: "", level: 2 };
    case "rich_text":
      return { type, html: "" };
    case "image":
      return { type, asset_id: "", alt: "", caption: "" };
    case "button":
      return { type, label: "", href: "" };
    case "product_grid":
      return { type, title: "", product_ids: [] };
    case "faq":
      return { type, items: [] };
  }
}
export function moveItem<T>(items: readonly T[], index: number, delta: number): T[] {
  const next = [...items],
    target = index + delta;
  if (index < 0 || index >= items.length || target < 0 || target >= items.length) return next;
  const [item] = next.splice(index, 1);
  if (item !== undefined) next.splice(target, 0, item);
  return next;
}
export function removeItem<T>(items: readonly T[], index: number): T[] {
  return items.filter((_, i) => i !== index);
}
export function parseSynonyms(text: string): {
  groups: string[][];
  error?: "groups" | "terms" | "length";
  line?: number;
} {
  const lines = text
    .split(/\r?\n/)
    .map((raw, i) => ({ raw, line: i + 1 }))
    .filter((x) => x.raw.trim());
  const groups = lines.map((x) => x.raw.split(",").map((t) => t.trim()));
  if (groups.length > 500) return { groups, error: "groups" };
  for (let i = 0; i < groups.length; i++) {
    const g = groups[i] ?? [];
    if (g.length < 2 || g.length > 20 || new Set(g.map((t) => t.toLowerCase())).size !== g.length)
      return { groups, error: "terms", line: lines[i]?.line };
    if (g.some((t) => !t || [...t].length > 50) || /[\p{Cc}]/u.test(lines[i]?.raw ?? ""))
      return { groups, error: "length", line: lines[i]?.line };
  }
  return { groups };
}
export function synonymsText(groups: string[][]): string {
  return groups.map((g) => g.join(", ")).join("\n");
}
export function updateMenu(
  items: MenuEntry[],
  path: [number] | [number, number],
  change: (entry: MenuEntry) => MenuEntry,
): MenuEntry[] {
  return items.map((entry, i) =>
    i !== path[0]
      ? entry
      : path.length === 1
        ? change(entry)
        : {
            ...entry,
            children: (entry.children ?? []).map((child, j) =>
              j === path[1] ? change(child) : child,
            ),
          },
  );
}
export function countMenu(items: MenuEntry[]): number {
  return items.reduce((sum, e) => sum + 1 + (e.children?.length ?? 0), 0);
}
export function missingLabel(value: string, labels: Record<string, string>): string {
  const [type, locale] = value.split(":");
  if (type && locale && labels[type]) return `${labels[type]} (${locale})`;
  return labels[value] ?? value;
}
export function validContentHref(value: string): boolean {
  return (
    !/[\p{Cc}\s\\]/u.test(value) &&
    (/^\/(?!\/)/.test(value) || /^(https:\/\/[^/]+|mailto:[^@]+@.+|tel:[+\d][\d()-]*)/i.test(value))
  );
}

/** Explicit input shapes: API views contain fields rejected by deny_unknown_fields. */
export function pageInput(page: Schemas["PageInput"]): Schemas["PageInput"] {
  return {
    kind: page.kind,
    legal_type: page.legal_type ?? null,
    status: page.status ?? "draft",
    published_at: page.published_at ?? null,
    image_asset_id: page.image_asset_id ?? null,
    translations: page.translations.filter(
      (tr) =>
        tr.title.trim() ||
        tr.slug.trim() ||
        tr.excerpt?.trim() ||
        tr.seo_title?.trim() ||
        tr.seo_description?.trim() ||
        (tr.blocks?.length ?? 0) > 0,
    ),
  };
}
export function legalInput(entity: Schemas["LegalEntity"]): Schemas["LegalEntity"] {
  return {
    company_name: entity.company_name,
    company_id: entity.company_id,
    street: entity.street,
    city: entity.city,
    postal_code: entity.postal_code,
    country: entity.country.toUpperCase(),
    email: entity.email,
    phone: entity.phone,
    registry: entity.registry,
    returns_address: entity.returns_address,
  };
}
