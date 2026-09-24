import { expect, test } from "vitest";
import { readConsent } from "./client.ts";
import { formatMoney, imageAttrs, imageUrl, times } from "./format.ts";

const img = {
  key: "p/shirt-1",
  alt: "Tričko",
  width: 1200,
  height: 1500,
  widths: [360, 720, 1080],
};

test("money in minor units", () => {
  expect(formatMoney(12900, "CZK")).toBe("129,00 Kč");
  expect(formatMoney(1999, "EUR", "sk-SK")).toBe("19,99 €");
  expect(times({ amount_minor: 12900, currency: "CZK", formatted: "" }, 3).amount_minor).toBe(
    38700,
  );
});

test("image variants", () => {
  expect(imageUrl(img, 400)).toBe("/media/p/shirt-1/720.avif");
  expect(imageUrl(img, 5000)).toBe("/media/p/shirt-1/1080.avif");
  expect(() => imageUrl({ ...img, key: "../x" }, 100)).toThrow();
  expect(imageAttrs(img, { sizes: "100vw", priority: true, width: 720 })).toMatchObject({
    width: 720,
    height: 900,
    fetchpriority: "high",
    loading: "eager",
  });
});

test("consent cookie parsing ignores unknown purposes", () => {
  expect(readConsent("a=1; consent=analytics%2Cevil%2Cads")).toEqual(["analytics", "ads"]);
  expect(readConsent("consent=")).toEqual([]);
  expect(readConsent("x=1")).toBeNull();
});
