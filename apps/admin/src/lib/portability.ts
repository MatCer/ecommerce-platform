/**
 * CSV import helpers (WP13b): the fields each import kind understands (mirrors
 * `commerce::portability::import_*::FIELDS`), reading a file's header row in the browser for
 * the column mapping, and the default mapping (same name, case-insensitive).
 */
import type { Schemas } from "./api.ts";

export type ImportKind = Schemas["DataImportKind"];

export interface ImportField {
  name: string;
  required: boolean;
}

const f = (name: string, required = false): ImportField => ({ name, required });

export const IMPORT_FIELDS: Record<ImportKind, ImportField[]> = {
  customers: [
    f("email", true),
    f("name"),
    f("phone"),
    f("locale"),
    f("company"),
    f("street"),
    f("city"),
    f("postal_code"),
    f("country"),
  ],
  orders: [
    f("order_number", true),
    f("placed_at", true),
    f("email", true),
    f("currency", true),
    f("total", true),
    f("status"),
    f("name"),
    f("phone"),
    f("company"),
    f("street"),
    f("city"),
    f("postal_code"),
    f("country"),
    f("sku"),
    f("item_name"),
    f("quantity"),
    f("unit_price"),
  ],
  subscribers: [
    f("email", true),
    f("locale"),
    f("consent_at"),
    f("consent_source"),
    f("consent_ip"),
    f("consent_text_version"),
  ],
};

export const MAX_IMPORT_BYTES = 20 * 1024 * 1024;

/**
 * The header row of a CSV text: `;` when the first line has more semicolons than commas
 * (the server sniffs the same way), quotes honoured, BOM and whitespace stripped.
 */
export function headerRow(text: string): string[] {
  const body = text.startsWith("﻿") ? text.slice(1) : text;
  const firstLine = body.split(/\r?\n/, 1)[0] ?? "";
  const count = (c: string) => firstLine.split(c).length - 1;
  const sep = count(";") > count(",") ? ";" : ",";
  const cells: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (quoted) {
      if (ch === '"' && body[i + 1] === '"') {
        cell += '"';
        i++;
      } else if (ch === '"') quoted = false;
      else cell += ch;
    } else if (ch === '"') quoted = true;
    else if (ch === sep) {
      cells.push(cell.trim());
      cell = "";
    } else if (ch === "\n" || ch === "\r") break;
    else cell += ch;
  }
  cells.push(cell.trim());
  return cells.filter((c) => c !== "");
}

/** Field → header for every field whose name matches a header (case-insensitive). */
export function guessMapping(kind: ImportKind, headers: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const field of IMPORT_FIELDS[kind]) {
    const match = headers.find((h) => h.toLowerCase() === field.name);
    if (match) out[field.name] = match;
  }
  return out;
}

/** Required fields without a column. */
export function missingRequired(kind: ImportKind, mapping: Record<string, string>): string[] {
  return IMPORT_FIELDS[kind].filter((x) => x.required && !mapping[x.name]).map((x) => x.name);
}

/** Triggers a browser download of `data` as a pretty-printed JSON file. */
export function downloadJson(data: unknown, filename: string): void {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
