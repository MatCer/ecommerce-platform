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

/** `srcset` candidates as `[url, width]`, narrowest first. */
function candidates(srcset: string): [string, number][] {
  return srcset
    .split(",")
    .map((c) => c.trim().split(/\s+/))
    .filter((p): p is [string, string] => p.length === 2 && /^\d+w$/.test(p[1] ?? ""))
    .map(([url, w]): [string, number] => [url, Number.parseInt(w, 10)])
    .sort((a, b) => a[1] - b[1]);
}

/**
 * URL of the narrowest AVIF variant at least `width` px wide (the widest if none is), e.g.
 * for thumbnails in islands. Falls back to `src` when the image has no AVIF variants.
 */
export function imageUrl(img: Image, width: number): string {
  const list = candidates(img.srcset);
  return (list.find(([, w]) => w >= width) ?? list.at(-1))?.[0] ?? img.src;
}

/**
 * Attributes for an `<img>`: AVIF `srcset` (every current browser decodes AVIF; `src` is the
 * JPEG/PNG fallback), explicit dimensions (no CLS), lazy unless it is the LCP image. `sizes`
 * must describe the real slot: a loose guess makes the browser pick a 2-4× heavier variant.
 */
export function imageAttrs(
  img: Image,
  opts: { sizes: string; priority?: boolean; width?: number },
) {
  const width = opts.width ?? img.width;
  return {
    src: img.src,
    srcset: img.srcset || img.srcset_fallback,
    sizes: opts.sizes,
    width,
    height: Math.round((img.height / img.width) * width),
    alt: img.alt,
    loading: opts.priority ? ("eager" as const) : ("lazy" as const),
    decoding: "async" as const,
    fetchpriority: opts.priority ? ("high" as const) : ("auto" as const),
  };
}

/**
 * The LCP image (spec §9.6): the `<link rel=preload>` attributes and the matching `<img>`
 * attributes from one `sizes` value, so the preload and the image can never disagree.
 */
export function lcpImage(img: Image, sizes: string, width?: number) {
  return {
    preload: {
      rel: "preload" as const,
      as: "image" as const,
      type: "image/avif",
      imagesrcset: img.srcset,
      imagesizes: sizes,
      fetchpriority: "high" as const,
    },
    img: imageAttrs(img, { sizes, priority: true, width }),
  };
}

/** Platform messages of the active locale (`ShopModel.messages`). */
export type Messages = Record<string, string>;

/** A message with `{name}` placeholders filled in; the key itself when it is missing. */
export function t(
  messages: Messages,
  key: string,
  args: Record<string, string | number> = {},
): string {
  let out = messages[key] ?? key;
  for (const [name, value] of Object.entries(args))
    out = out.replaceAll(`{${name}}`, String(value));
  return out;
}

/** The messages an island needs: islands serialize their props into the page, so send few. */
export function pick(messages: Messages, keys: readonly string[]): Messages {
  return Object.fromEntries(keys.map((k) => [k, messages[k] ?? k]));
}
