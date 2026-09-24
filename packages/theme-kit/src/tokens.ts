/**
 * Design tokens (`theme.tokens.json`, spec §9.1 + A6). Schema-validated data, not code, so the
 * platform can safely turn them into CSS for the checkout origin.
 */
export interface ThemeTokens {
  colors: Record<string, string>;
  fonts: Record<string, string>;
  radius: Record<string, string>;
}

const KEY = /^[a-z][a-z0-9-]{0,31}$/;
const COLOR = /^(#[0-9a-fA-F]{6}|oklch\(\s*[0-9.]+%?\s+[0-9.]+\s+[0-9.]+\s*\))$/;
const FONT = /^[A-Za-z0-9 ,'"-]{1,120}$/;
const LENGTH = /^(0|[0-9]{1,3}(\.[0-9]{1,3})?(rem|px))$/;

function group(input: unknown, name: string, value: RegExp): Record<string, string> {
  if (typeof input !== "object" || input === null || Array.isArray(input)) {
    throw new Error(`tokens.${name}: expected an object`);
  }
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(input)) {
    if (!KEY.test(k)) throw new Error(`tokens.${name}: invalid key ${JSON.stringify(k)}`);
    if (typeof v !== "string" || !value.test(v)) {
      throw new Error(`tokens.${name}.${k}: invalid value ${JSON.stringify(v)}`);
    }
    out[k] = v;
  }
  return out;
}

/** Throws on anything outside the allowlisted shapes (keys and values). */
export function validateTokens(input: unknown): ThemeTokens {
  if (typeof input !== "object" || input === null) throw new Error("tokens: expected an object");
  const t = input as Record<string, unknown>;
  const extra = Object.keys(t).filter((k) => !["colors", "fonts", "radius", "$schema"].includes(k));
  if (extra.length) throw new Error(`tokens: unknown keys ${extra.join(", ")}`);
  return {
    colors: group(t.colors, "colors", COLOR),
    fonts: group(t.fonts, "fonts", FONT),
    radius: group(t.radius, "radius", LENGTH),
  };
}

/**
 * Unlayered `:root` custom properties. They override the defaults a stylesheet declares in
 * Tailwind's `@theme` (which live in `@layer theme`), so the same tokens restyle any app.
 */
export function tokensToCss(tokens: ThemeTokens): string {
  const lines = [
    ...Object.entries(tokens.colors).map(([k, v]) => `--color-${k}:${v};`),
    ...Object.entries(tokens.fonts).map(([k, v]) => `--font-${k}:${v};`),
    ...Object.entries(tokens.radius).map(([k, v]) => `--radius-${k}:${v};`),
  ];
  return `:root{${lines.join("")}}\n`;
}
