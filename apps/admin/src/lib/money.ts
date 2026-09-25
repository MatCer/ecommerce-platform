/**
 * Money in the admin: the API speaks integer minor units + ISO currency (spec D25); people type
 * decimals ("129,90" or "129.90"). Every supported currency (EU) has two decimals.
 */
import type { components } from "@platform/admin-client";

export type Currency = components["schemas"]["Currency"];
export const CURRENCIES: readonly Currency[] = [
  "CZK",
  "EUR",
  "PLN",
  "HUF",
  "RON",
  "BGN",
  "DKK",
  "SEK",
];

/** "1 299,50" -> 129950. `null` for anything that is not a non-negative amount with <= 2 decimals. */
export function parseMoney(text: string): number | null {
  const s = text.replace(/[\s  ]/g, "").replace(",", ".");
  if (!/^\d+(\.\d{1,2})?$/.test(s)) return null;
  const [whole = "0", frac = ""] = s.split(".");
  const minor = Number(whole) * 100 + Number(frac.padEnd(2, "0"));
  return Number.isSafeInteger(minor) ? minor : null;
}

/** 129950 -> "1299.50" for an input field (no grouping, so it parses back). */
export function minorToInput(minor: number | null | undefined): string {
  if (minor == null) return "";
  return (minor / 100).toFixed(2);
}

export function formatMoney(minor: number, currency: string, locale: string): string {
  return new Intl.NumberFormat(locale, { style: "currency", currency }).format(minor / 100);
}

/** Percent text ("12.5") <-> basis points (1250). */
export function parsePercent(text: string): number | null {
  const s = text.trim().replace(",", ".");
  if (!/^\d+(\.\d{1,2})?$/.test(s)) return null;
  const bp = Math.round(Number(s) * 100);
  return bp > 0 && bp <= 10_000 ? bp : null;
}

/** ISO instant <-> the value of an `<input type="datetime-local">` in the browser's zone. */
export function toLocalInput(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * The instant to send for a datetime field: the untouched original (full precision: the API
 * compares running-sale starts exactly) or the edited local value.
 */
export function instant(local: string, original: string | null | undefined): string | null {
  if (original && toLocalInput(original) === local) return original;
  return fromLocalInput(local);
}

export function fromLocalInput(value: string): string | null {
  if (!value) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}
