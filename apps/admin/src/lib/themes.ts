/**
 * The theme builder's check report (`RevisionDetail.checks`, WP23) as the admin shows it.
 * The API passes it through as JSON, so it is narrowed here instead of trusted.
 */
import type { Tone } from "@platform/ui";

export interface StepView {
  name: string;
  status: "passed" | "failed" | "skipped";
  ms: number | null;
  log: string;
}

export interface PageBudget {
  path: string;
  lcpMs: number | null;
  tbtMs: number | null;
  cls: number | null;
  jsKb: number | null;
  jsRumKb: number | null;
  calls: number | null;
  axe: string[];
  failures: string[];
}

export interface ReportView {
  pipeline: string;
  steps: StepView[];
  pages: PageBudget[];
  failures: string[];
}

const obj = (v: unknown): Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
const arr = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const num = (v: unknown): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const str = (v: unknown): string => (typeof v === "string" ? v : "");
const kb = (v: unknown) => {
  const n = num(v);
  return n === null ? null : Math.round((n / 1024) * 10) / 10;
};

export function parseReport(checks: unknown): ReportView {
  const c = obj(checks);
  const steps = arr(c.steps).map((s): StepView => {
    const o = obj(s);
    const status = o.status === "passed" || o.status === "failed" ? o.status : "skipped";
    return { name: str(o.name), status, ms: num(o.ms), log: str(o.log) };
  });
  const budget = arr(c.steps)
    .map(obj)
    .find((s) => s.name === "budget");
  const pages = arr(budget?.pages).map((p): PageBudget => {
    const o = obj(p);
    return {
      path: str(o.path),
      lcpMs: num(o.lcp_ms),
      tbtMs: num(o.tbt_ms),
      cls: num(o.cls),
      jsKb: kb(o.js_gzip),
      jsRumKb: kb(o.js_gzip_with_rum),
      calls: num(o.calls),
      axe: arr(o.axe).map((a) => str(obj(a).id)),
      failures: arr(o.failures).map(str),
    };
  });
  return {
    pipeline: str(c.pipeline),
    steps,
    pages,
    failures: arr(c.failures).map(str).filter(Boolean),
  };
}

/** Statuses that are still moving (the list polls while one exists). */
export const PENDING = new Set(["draft", "building", "checking"]);

export function statusTone(status: string): Tone {
  if (status === "published") return "success";
  if (status === "ready") return "info";
  if (status === "failed") return "error";
  if (PENDING.has(status)) return "warning";
  return "neutral";
}

/** Design tokens as edited in the admin (the same allowlist as `theme.tokens.json`, A6). */
export type TokenGroups = {
  colors: Record<string, string>;
  fonts: Record<string, string>;
  radius: Record<string, string>;
};

const COLOR = /^(#[0-9a-fA-F]{6}|oklch\(\s*[0-9.]+%?\s+[0-9.]+\s+[0-9.]+\s*\))$/;
const FONT = /^[A-Za-z0-9 ,'"-]{1,120}$/;
const LENGTH = /^(0|[0-9]{1,3}(\.[0-9]{1,3})?(rem|px))$/;

export function tokenGroups(v: unknown): TokenGroups {
  const o = obj(v);
  const group = (g: unknown) =>
    Object.fromEntries(
      Object.entries(obj(g)).filter((e): e is [string, string] => typeof e[1] === "string"),
    );
  return { colors: group(o.colors), fonts: group(o.fonts), radius: group(o.radius) };
}

/** Field-level problems (key → message key) before anything is sent. */
export function tokenErrors(t: TokenGroups): Record<string, "color" | "font" | "length"> {
  const out: Record<string, "color" | "font" | "length"> = {};
  for (const [k, v] of Object.entries(t.colors))
    if (!COLOR.test(v.trim())) out[`colors.${k}`] = "color";
  for (const [k, v] of Object.entries(t.fonts))
    if (!FONT.test(v.trim())) out[`fonts.${k}`] = "font";
  for (const [k, v] of Object.entries(t.radius))
    if (!LENGTH.test(v.trim())) out[`radius.${k}`] = "length";
  return out;
}

/** `#rrggbb` for the native colour picker, when the value is one. */
export const hexOf = (v: string) => (/^#[0-9a-fA-F]{6}$/.test(v.trim()) ? v.trim() : undefined);

/** AI theme edits (WP24): a run that is still working (poll it). */
export const RUN_ACTIVE = new Set(["queued", "running"]);

export function runTone(status: string): Tone {
  if (status === "succeeded" || status === "accepted") return "success";
  if (status === "failed") return "error";
  if (RUN_ACTIVE.has(status)) return "warning";
  return "neutral";
}

export type DiffKind = "file" | "hunk" | "add" | "del" | "ctx";

/** A unified diff split into lines tagged for colouring (no HTML is ever built from it). */
export function diffLines(diff: string): { kind: DiffKind; text: string }[] {
  return diff
    .split("\n")
    .filter((line, i, all) => line !== "" || i < all.length - 1)
    .map((text) => {
      const kind: DiffKind =
        text.startsWith("+++ ") || text.startsWith("--- ") || text.startsWith("Binary file ")
          ? "file"
          : text.startsWith("@@")
            ? "hunk"
            : text.startsWith("+")
              ? "add"
              : text.startsWith("-")
                ? "del"
                : "ctx";
      return { kind, text };
    });
}
