/**
 * The product editor's working copy and its mapping to the Admin API product document
 * (`ProductInput`). Pure functions: unit tested, no Solid or network.
 */
import type { components } from "@platform/admin-client";

type S = components["schemas"];
export type Product = S["Product"];
export type ProductInput = S["ProductInput"];
export type ProductOption = S["ProductOption"];
export type ProductMedia = S["ProductMedia"];
export type ParameterValue = S["ParameterValue"];
export type Gpsr = S["Gpsr"];
export type GpsrParty = S["GpsrParty"];
export type UnitMeasure = S["UnitMeasure"];
export type ProductStatus = S["ProductStatus"];
export type I18n = Record<string, string>;

/** Content locales the admin edits (spec: cs/sk/en). */
export const CONTENT_LOCALES = ["cs", "sk", "en"] as const;
export type ContentLocale = (typeof CONTENT_LOCALES)[number];

export interface TranslationDraft {
  name: string;
  slug: string;
  short_description: string;
  description_html: string;
  seo_title: string;
  seo_description: string;
}

export interface VariantDraft {
  id?: string;
  sku: string;
  ean: string;
  weight_g: string;
  option_values: Record<string, string>;
  is_default: boolean;
}

export interface PartyDraft {
  name: string;
  address: string;
  email: string;
  url: string;
  phone: string;
}

export interface ProductDraft {
  status: ProductStatus;
  brand: string;
  translations: Record<ContentLocale, TranslationDraft>;
  options: ProductOption[];
  variants: VariantDraft[];
  category_ids: string[];
  parameters: ParameterValue[];
  media: ProductMedia[];
  manufacturer: PartyDraft;
  eu_responsible_person: PartyDraft;
  safety_info: Record<ContentLocale, string>;
  warnings: Record<ContentLocale, string>;
  unit_measure: UnitMeasure | "";
  unit_quantity: string;
  /** Country (ISO alpha-2) -> tax category code; absent = standard. */
  tax_categories: Record<string, string>;
  google_category: string;
  heureka_category: string;
}

const emptyTranslation = (): TranslationDraft => ({
  name: "",
  slug: "",
  short_description: "",
  description_html: "",
  seo_title: "",
  seo_description: "",
});

const emptyParty = (): PartyDraft => ({ name: "", address: "", email: "", url: "", phone: "" });

const perLocale = <T>(make: (l: ContentLocale) => T): Record<ContentLocale, T> => ({
  cs: make("cs"),
  sk: make("sk"),
  en: make("en"),
});

export function emptyDraft(): ProductDraft {
  return {
    status: "draft",
    brand: "",
    translations: perLocale(emptyTranslation),
    options: [],
    variants: [],
    category_ids: [],
    parameters: [],
    media: [],
    manufacturer: emptyParty(),
    eu_responsible_person: emptyParty(),
    safety_info: perLocale(() => ""),
    warnings: perLocale(() => ""),
    unit_measure: "",
    unit_quantity: "",
    tax_categories: {},
    google_category: "",
    heureka_category: "",
  };
}

const party = (p: GpsrParty | null | undefined): PartyDraft => ({
  name: p?.name ?? "",
  address: p?.address ?? "",
  email: p?.email ?? "",
  url: p?.url ?? "",
  phone: p?.phone ?? "",
});

export function draftFromProduct(p: Product): ProductDraft {
  const translations = perLocale((l) => {
    const t = p.translations.find((x) => x.locale === l);
    return t
      ? {
          name: t.name,
          slug: t.slug,
          short_description: t.short_description ?? "",
          description_html: t.description_html ?? "",
          seo_title: t.seo_title ?? "",
          seo_description: t.seo_description ?? "",
        }
      : emptyTranslation();
  });
  return {
    status: p.status,
    brand: p.brand ?? "",
    translations,
    options: p.options,
    variants: p.variants.map((v) => ({
      id: v.id,
      sku: v.sku,
      ean: v.ean ?? "",
      weight_g: v.weight_g == null ? "" : String(v.weight_g),
      option_values: v.option_values ?? {},
      is_default: v.is_default,
    })),
    category_ids: p.category_ids,
    parameters: p.parameters,
    media: p.media,
    manufacturer: party(p.gpsr.manufacturer),
    eu_responsible_person: party(p.gpsr.eu_responsible_person),
    safety_info: perLocale((l) => p.gpsr.safety_info?.[l] ?? ""),
    warnings: perLocale((l) => p.gpsr.warnings?.[l] ?? ""),
    unit_measure: p.unit_measure ?? "",
    unit_quantity: p.unit_quantity == null ? "" : String(p.unit_quantity),
    tax_categories: p.tax_categories,
    google_category: p.google_category ?? "",
    heureka_category: p.heureka_category ?? "",
  };
}

const opt = (s: string): string | null => (s.trim() === "" ? null : s.trim());

/** Drops blank entries: the API rejects empty translations in i18n maps. */
export function compactI18n(map: Record<string, string>): I18n {
  return Object.fromEntries(
    Object.entries(map)
      .map(([k, v]) => [k, v.trim()] as const)
      .filter(([, v]) => v !== ""),
  );
}

function partyInput(p: PartyDraft): GpsrParty | null {
  if (Object.values(p).every((v) => v.trim() === "")) return null;
  return {
    name: p.name.trim(),
    address: p.address.trim(),
    email: opt(p.email),
    url: opt(p.url),
    phone: opt(p.phone),
  };
}

/** A form value the API would reject or misread; `code` is an `errors.*` message key. */
export class DraftError extends Error {
  readonly code: string;

  constructor(code: string) {
    super(code);
    this.code = code;
  }
}

/** Number from a form field (decimal comma allowed); throws instead of sending NaN (-> null). */
function num(s: string, code: string): number {
  const n = Number(s.trim().replace(",", "."));
  if (!Number.isFinite(n)) throw new DraftError(code);
  return n;
}

function weight(s: string): number | null {
  if (s.trim() === "") return null;
  const n = num(s, "invalid_weight");
  if (!Number.isInteger(n) || n < 0 || n > 10_000_000) throw new DraftError("invalid_weight");
  return n;
}

/** Option names and value names without blank locales (the API rejects empty entries). */
function compactOptions(options: readonly ProductOption[]): ProductOption[] {
  return options.map((o) => ({
    ...o,
    name_i18n: compactI18n(o.name_i18n),
    values: o.values.map((v) => ({ ...v, name_i18n: compactI18n(v.name_i18n) })),
  }));
}

/** The API document for a draft; throws `DraftError` for values it cannot represent. */
export function draftToInput(d: ProductDraft): ProductInput {
  const translations = CONTENT_LOCALES.filter((l) => d.translations[l].name.trim() !== "").map(
    (locale) => {
      const t = d.translations[locale];
      return {
        locale,
        name: t.name.trim(),
        slug: t.slug.trim(),
        short_description: t.short_description.trim(),
        description_html: t.description_html,
        seo_title: opt(t.seo_title),
        seo_description: opt(t.seo_description),
      };
    },
  );
  return {
    status: d.status,
    brand: opt(d.brand),
    translations,
    options: compactOptions(d.options),
    variants: d.variants.map((v) => ({
      ...(v.id ? { id: v.id } : {}),
      sku: v.sku.trim(),
      ean: opt(v.ean),
      weight_g: weight(v.weight_g),
      option_values: v.option_values,
      is_default: v.is_default,
    })),
    category_ids: d.category_ids,
    // Number inputs hold their text until saving.
    parameters: d.parameters.map((p) =>
      typeof p.value === "string" ? { ...p, value: num(p.value, "invalid_parameter_value") } : p,
    ),
    media: d.media.map((m) => ({ ...m, alt_i18n: compactI18n(m.alt_i18n ?? {}) })),
    gpsr: {
      manufacturer: partyInput(d.manufacturer),
      eu_responsible_person: partyInput(d.eu_responsible_person),
      safety_info: compactI18n(d.safety_info),
      warnings: compactI18n(d.warnings),
    },
    unit_measure: d.unit_measure === "" ? null : d.unit_measure,
    unit_quantity: d.unit_quantity.trim() === "" ? null : num(d.unit_quantity, "invalid_unit"),
    tax_categories: d.tax_categories,
    google_category: opt(d.google_category),
    heureka_category: opt(d.heureka_category),
  };
}

/** URL slug: ASCII lowercase words joined by hyphens (diacritics folded: "Tričko" -> "tricko"). */
export function slugify(text: string): string {
  return text
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 200)
    .replace(/-+$/, "");
}

/** Option/value code from a label: `[a-z0-9][a-z0-9_-]*`, at most 64 characters. */
export function codify(text: string): string {
  return slugify(text).slice(0, 64).replace(/-+$/, "");
}

/** The API's limit per product. */
export const MAX_VARIANTS = 500;

const comboKey = (values: Record<string, string>, codes: readonly string[]) =>
  codes.map((c) => `${c}=${values[c] ?? ""}`).join("&");

/**
 * Every combination of option values as a variant, in option order. Existing variants whose
 * combination still exists keep their id, SKU, EAN and weight; new combinations get a SKU
 * derived from `skuBase`. Without options there is exactly one variant.
 */
export function variantMatrix(
  options: readonly ProductOption[],
  existing: readonly VariantDraft[],
  skuBase: string,
): VariantDraft[] {
  // Count before materialising: a few options with many values explode combinatorially.
  const count = options.reduce((n, o) => n * o.values.length, 1);
  if (count > MAX_VARIANTS) throw new DraftError("too_many_variants");
  const codes = options.map((o) => o.code);
  let combos: Record<string, string>[] = [{}];
  for (const o of options) {
    combos = combos.flatMap((c) => o.values.map((v) => ({ ...c, [o.code]: v.code })));
  }
  const byKey = new Map(existing.map((v) => [comboKey(v.option_values, codes), v]));
  const base = skuBase.trim().toUpperCase().replace(/\s+/g, "-") || "SKU";
  const out = combos.map((values) => {
    const kept = byKey.get(comboKey(values, codes));
    if (kept) return { ...kept, option_values: values };
    const suffix = codes.map((c) => values[c]?.toUpperCase()).join("-");
    return {
      sku: suffix ? `${base}-${suffix}` : base,
      ean: "",
      weight_g: "",
      option_values: values,
      is_default: false,
    };
  });
  if (out.length > 0 && !out.some((v) => v.is_default)) {
    out[0] = { ...(out[0] as VariantDraft), is_default: true };
  }
  return out;
}

/** GTIN-8/12/13/14 check digit (mirrors the API's validation for inline feedback). */
export function eanValid(ean: string): boolean {
  if (!/^(\d{8}|\d{12}|\d{13}|\d{14})$/.test(ean)) return false;
  const digits = [...ean].map(Number);
  const check = digits.pop() as number;
  const sum = digits.reverse().reduce((acc, d, i) => acc + d * (i % 2 === 0 ? 3 : 1), 0);
  return (10 - (sum % 10)) % 10 === check;
}

/** Moves the item at `from` by `delta` positions (clamped); returns a new array. */
export function move<T>(items: readonly T[], from: number, delta: number): T[] {
  const to = Math.max(0, Math.min(items.length - 1, from + delta));
  const copy = [...items];
  const [item] = copy.splice(from, 1);
  if (item !== undefined) copy.splice(to, 0, item);
  return copy;
}
