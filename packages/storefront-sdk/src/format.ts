import type { Image, Money } from "./types.ts";

const formatters = new Map<string, Intl.NumberFormat>();

/** Formats minor units in the currency's own precision (CZK/EUR: 2 decimals). */
export function formatMoney(amountMinor: number, currency: string, locale = "cs-CZ"): string {
  const key = `${locale}|${currency}`;
  let f = formatters.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, { style: "currency", currency });
    formatters.set(key, f);
  }
  const digits = f.resolvedOptions().maximumFractionDigits ?? 2;
  return f.format(amountMinor / 10 ** digits);
}

/** Multiplies a price by a quantity without floating point (for optimistic cart UI). */
export function times(m: Money, qty: number, locale?: string): Money {
  const amount_minor = m.amount_minor * qty;
  return {
    amount_minor,
    currency: m.currency,
    formatted: formatMoney(amount_minor, m.currency, locale),
  };
}

const KEY = /^[a-z0-9][a-z0-9/_-]{0,200}$/;

/** URL of a pre-generated AVIF variant: the closest available width not smaller than `width`. */
export function imageUrl(img: Image, width: number): string {
  if (!KEY.test(img.key)) throw new Error(`invalid image key ${img.key}`);
  const sorted = [...img.widths].sort((a, b) => a - b);
  const w = sorted.find((x) => x >= width) ?? sorted.at(-1) ?? img.width;
  return `/media/${img.key}/${w}.avif`;
}

/** `srcset` over every variant width. */
export function srcset(img: Image): string {
  return [...img.widths]
    .sort((a, b) => a - b)
    .map((w) => `${imageUrl(img, w)} ${w}w`)
    .join(", ");
}

/** Attributes for an `<img>` with explicit dimensions (no CLS), lazy unless it is the LCP. */
export function imageAttrs(
  img: Image,
  opts: { sizes: string; priority?: boolean; width?: number },
) {
  const width = opts.width ?? img.width;
  return {
    src: imageUrl(img, width),
    srcset: srcset(img),
    sizes: opts.sizes,
    width,
    height: Math.round((img.height / img.width) * width),
    alt: img.alt,
    loading: opts.priority ? ("eager" as const) : ("lazy" as const),
    decoding: "async" as const,
    fetchpriority: opts.priority ? ("high" as const) : ("auto" as const),
  };
}
