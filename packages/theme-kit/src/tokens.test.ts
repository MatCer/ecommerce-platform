import { expect, test } from "vitest";
import { tokensToCss, validateTokens } from "./tokens.ts";

const ok = {
  colors: { brand: "#1f3a2e", bg: "oklch(0.98 0.01 90)" },
  fonts: { sans: '"Inter Variable", system-ui, sans-serif' },
  radius: { control: "0.375rem" },
};

test("valid tokens become unlayered CSS custom properties", () => {
  expect(tokensToCss(validateTokens(ok))).toBe(
    ':root{--color-brand:#1f3a2e;--color-bg:oklch(0.98 0.01 90);--font-sans:"Inter Variable", system-ui, sans-serif;--radius-control:0.375rem;}\n',
  );
});

test.each([
  ["CSS injection in a color", { ...ok, colors: { brand: "red;}body{display:none" } }],
  ["url() in a font", { ...ok, fonts: { sans: "url(https://evil.example/x)" } }],
  ["expression in a radius", { ...ok, radius: { control: "calc(1px + 1px)" } }],
  ["bad key", { ...ok, colors: { "Brand}": "#000000" } }],
  ["unknown group", { ...ok, scripts: {} }],
  ["missing group", { colors: ok.colors, fonts: ok.fonts }],
])("rejects %s", (_name, input) => {
  expect(() => validateTokens(input)).toThrow();
});
