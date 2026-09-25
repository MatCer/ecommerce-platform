/**
 * Email marketing (WP18) form logic: segment rules <-> the rule-builder form, campaign content
 * (blank blocks, client-side checks mirroring the API) and test recipients. The API validates
 * again and stays the trust boundary; these checks only give earlier, clearer feedback.
 */
import type { Tone } from "@platform/ui";
import type { Schemas } from "./api.ts";
import { validContentHref } from "./content-form.ts";
import { fromLocalInput, minorToInput, parseMoney, toLocalInput } from "./money.ts";

export type Rules = Schemas["Rules"];
export type Condition = Schemas["Condition"];
export type ConditionField = Condition["field"];
export type AffinityDim = Schemas["AffinityDim"];
export type EmailBlock = Schemas["EmailBlock"];
export type EmailBlockType = EmailBlock["type"];
export type LocaleContent = Schemas["LocaleContent"];
export type CampaignInput = Schemas["CampaignInput"];

export const CONDITION_FIELDS: readonly ConditionField[] = [
  "locale",
  "market",
  "subscribed",
  "purchased_category",
  "purchased_brand",
  "order_count",
  "total_spent",
  "last_order",
  "engaged",
  "affinity",
];

/** API limits (crates/commerce/src/marketing). */
export const MAX_CONDITIONS = 20;
export const MAX_VALUES = 50;
export const MAX_BLOCKS = 50;
export const MAX_GRID = 12;
export const MAX_TEST_RECIPIENTS = 5;

/**
 * One condition as the form edits it: text inputs stay strings until submitted (amounts in
 * major units, dates as `datetime-local` values), so half-typed input is never lost.
 */
export interface ConditionForm {
  field: ConditionField;
  /** Locales, market ids or category ids (also category affinity keys). */
  ids: string[];
  /** Comma-separated brands (purchased brand, brand affinity). */
  text: string;
  after: string;
  before: string;
  /** Order count or amount (major units). */
  min: string;
  max: string;
  currency: string;
  days: string;
  dim: AffinityDim;
}

export function blankCondition(field: ConditionField, currency = "CZK"): ConditionForm {
  return {
    field,
    ids: [],
    text: "",
    after: "",
    before: "",
    min: "",
    max: "",
    currency,
    days: field === "engaged" ? "30" : "",
    dim: "category",
  };
}

/** "a, b ,,c" -> ["a", "b", "c"]. */
export function splitList(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

function count(text: string): number | null | undefined {
  const s = text.trim();
  if (!s) return undefined;
  return /^\d{1,9}$/.test(s) ? Number(s) : null;
}

function money(text: string): number | null | undefined {
  return text.trim() ? parseMoney(text) : undefined;
}

function date(local: string): string | null | undefined {
  return local ? fromLocalInput(local) : undefined;
}

/** A bounded window: at least one bound, both valid, not reversed. */
function bounded<T extends number | string>(
  lo: T | null | undefined,
  hi: T | null | undefined,
): boolean {
  if (lo === null || hi === null || (lo === undefined && hi === undefined)) return false;
  return lo === undefined || hi === undefined || lo <= hi;
}

function values(list: string[]): string[] | null {
  return list.length > 0 && list.length <= MAX_VALUES ? list : null;
}

/** The API condition, or `null` while the form is incomplete or invalid. */
export function toCondition(f: ConditionForm): Condition | null {
  switch (f.field) {
    case "locale": {
      const locales = values(f.ids);
      return locales && { field: "locale", locales };
    }
    case "market": {
      const market_ids = values(f.ids);
      return market_ids && { field: "market", market_ids };
    }
    case "purchased_category": {
      const category_ids = values(f.ids);
      return category_ids && { field: "purchased_category", category_ids };
    }
    case "purchased_brand": {
      const brands = values(splitList(f.text));
      return brands?.every((b) => [...b].length <= 200)
        ? { field: "purchased_brand", brands }
        : null;
    }
    case "subscribed":
    case "last_order": {
      const after = date(f.after);
      const before = date(f.before);
      return bounded(after, before) ? { field: f.field, after, before } : null;
    }
    case "order_count": {
      const min = count(f.min);
      const max = count(f.max);
      return bounded(min, max) ? { field: "order_count", min, max } : null;
    }
    case "total_spent": {
      const min_minor = money(f.min);
      const max_minor = money(f.max);
      return /^[A-Z]{3}$/.test(f.currency) && bounded(min_minor, max_minor)
        ? { field: "total_spent", currency: f.currency, min_minor, max_minor }
        : null;
    }
    case "engaged": {
      const days = count(f.days);
      return days != null && days >= 1 && days <= 365 ? { field: "engaged", days } : null;
    }
    case "affinity": {
      const keys = values(f.dim === "category" ? f.ids : splitList(f.text));
      return keys?.every((k) => [...k].length <= 200)
        ? { field: "affinity", dim: f.dim, keys }
        : null;
    }
  }
}

export function fromCondition(c: Condition): ConditionForm {
  const f = blankCondition(c.field);
  switch (c.field) {
    case "locale":
      return { ...f, ids: [...c.locales] };
    case "market":
      return { ...f, ids: [...c.market_ids] };
    case "purchased_category":
      return { ...f, ids: [...c.category_ids] };
    case "purchased_brand":
      return { ...f, text: c.brands.join(", ") };
    case "subscribed":
    case "last_order":
      return { ...f, after: toLocalInput(c.after), before: toLocalInput(c.before) };
    case "order_count":
      return { ...f, min: c.min == null ? "" : String(c.min), max: c.max == null ? "" : String(c.max) };
    case "total_spent":
      return {
        ...f,
        currency: c.currency,
        min: minorToInput(c.min_minor),
        max: minorToInput(c.max_minor),
      };
    case "engaged":
      return { ...f, days: String(c.days) };
    case "affinity":
      return c.dim === "category"
        ? { ...f, dim: c.dim, ids: [...c.keys] }
        : { ...f, dim: c.dim, text: c.keys.join(", ") };
  }
}

/**
 * The API rules, or the (0-based) index of the first incomplete condition. No conditions =
 * every subscriber.
 */
export function toRules(
  match: "all" | "any",
  forms: readonly ConditionForm[],
): { rules: Rules } | { invalid: number } {
  const conditions: Condition[] = [];
  for (const [i, f] of forms.entries()) {
    const c = toCondition(f);
    if (!c) return { invalid: i };
    conditions.push(c);
  }
  return { rules: { match, conditions } };
}

// ---------------------------------------------------------------------------------------------
// Campaign content

export const campaignTone: Record<Schemas["CampaignStatus"], Tone> = {
  draft: "neutral",
  scheduled: "info",
  sending: "warning",
  sent: "success",
  cancelled: "neutral",
};

export const EMAIL_BLOCK_TYPES: readonly EmailBlockType[] = [
  "heading",
  "text",
  "image",
  "button",
  "product_grid",
  "personalized_products",
];

export function blankEmailBlock(type: EmailBlockType): EmailBlock {
  switch (type) {
    case "heading":
      return { type, text: "" };
    case "text":
      return { type, html: "" };
    case "image":
      return { type, asset_id: "", alt: "", href: "" };
    case "button":
      return { type, label: "", href: "" };
    case "product_grid":
      return { type, title: "", product_ids: [] };
    case "personalized_products":
      return { type, title: "", limit: 4 };
  }
}

export function blankContent(): LocaleContent {
  return { subject: "", preheader: "", blocks: [] };
}

const len = (s: string | undefined) => [...(s ?? "").trim()].length;

/** Visible text of rich-text HTML (an editor left with `<p><br></p>` counts as empty). */
function hasText(html: string): boolean {
  return html.replace(/<[^>]*>|&nbsp;/g, "").trim() !== "";
}

export function blockValid(b: EmailBlock): boolean {
  switch (b.type) {
    case "heading":
      return len(b.text) >= 1 && len(b.text) <= 200;
    case "text":
      return hasText(b.html) && b.html.length <= 20_000;
    case "image":
      return b.asset_id !== "" && len(b.alt) <= 300 && (!b.href || validContentHref(b.href));
    case "button":
      return len(b.label) >= 1 && len(b.label) <= 100 && validContentHref(b.href);
    case "product_grid":
      return (
        b.product_ids.length >= 1 && b.product_ids.length <= MAX_GRID && len(b.title) <= 200
      );
    case "personalized_products": {
      const limit = b.limit ?? 4;
      return Number.isInteger(limit) && limit >= 2 && limit <= 8 && len(b.title) <= 200;
    }
  }
}

/** A language the campaign leaves out: nothing entered at all. */
export function isEmptyContent(c: LocaleContent): boolean {
  return !c.subject.trim() && !c.preheader?.trim() && c.blocks.length === 0;
}

export type ContentProblem =
  | { kind: "no_content" }
  | { kind: "subject"; locale: string }
  | { kind: "blocks"; locale: string }
  | { kind: "block"; locale: string; index: number };

/** The first problem of the campaign's content, or `null` when it can be saved. */
export function contentProblem(content: Record<string, LocaleContent>): ContentProblem | null {
  const used = Object.entries(content).filter(([, c]) => !isEmptyContent(c));
  if (used.length === 0) return { kind: "no_content" };
  for (const [locale, c] of used) {
    if (len(c.subject) < 1 || len(c.subject) > 200 || len(c.preheader) > 200)
      return { kind: "subject", locale };
    if (c.blocks.length === 0 || c.blocks.length > MAX_BLOCKS) return { kind: "blocks", locale };
    const index = c.blocks.findIndex((b) => !blockValid(b));
    if (index >= 0) return { kind: "block", locale, index };
  }
  return null;
}

/**
 * The request body: explicit fields only (the API denies unknown ones), empty languages dropped,
 * languages sorted like the API returns them (so bodies compare equal to the saved campaign).
 */
export function campaignInput(
  name: string,
  segmentId: string,
  content: Record<string, LocaleContent>,
): CampaignInput {
  const out: Record<string, LocaleContent> = {};
  for (const locale of Object.keys(content).sort()) {
    const c = content[locale];
    if (!c || isEmptyContent(c)) continue;
    out[locale] = {
      subject: c.subject.trim(),
      preheader: c.preheader?.trim() ?? "",
      blocks: c.blocks.map((b) =>
        b.type === "image" && !b.href ? { type: b.type, asset_id: b.asset_id, alt: b.alt } : b,
      ),
    };
  }
  return { name: name.trim(), segment_id: segmentId || null, content: out };
}

/** Test recipients typed as a list (commas, spaces or lines): 1-5 plausible addresses or `null`. */
export function parseEmails(text: string): string[] | null {
  const list = [...new Set(text.split(/[\s,;]+/).filter(Boolean))];
  if (list.length < 1 || list.length > MAX_TEST_RECIPIENTS) return null;
  return list.every((e) => /^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(e) && e.length <= 254) ? list : null;
}
