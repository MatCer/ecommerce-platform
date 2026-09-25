import { expect, test } from "vitest";
import { readConsent } from "./client.ts";
import { formatMoney, imageAttrs, imageUrl, lcpImage, pick, t, times } from "./format.ts";

const img = {
  alt: "Tričko",
  width: 1200,
  height: 1500,
  src: "/media/t/a/640.jpg",
  srcset: "/media/t/a/1080.avif 1080w, /media/t/a/360.avif 360w, /media/t/a/720.avif 720w",
  srcset_webp: "",
  srcset_fallback: "/media/t/a/640.jpg 640w",
};

test("money in minor units", () => {
  expect(formatMoney(12900, "CZK")).toBe("129,00 Kč");
  expect(formatMoney(1999, "EUR", "sk-SK")).toBe("19,99 €");
  expect(times({ amount_minor: 12900, currency: "CZK", formatted: "" }, 3).amount_minor).toBe(
    38700,
  );
});

test("image variants", () => {
  expect(imageUrl(img, 400)).toBe("/media/t/a/720.avif");
  expect(imageUrl(img, 5000)).toBe("/media/t/a/1080.avif");
  expect(imageUrl({ ...img, srcset: "" }, 100)).toBe("/media/t/a/640.jpg");
  expect(imageAttrs(img, { sizes: "100vw", priority: true, width: 720 })).toMatchObject({
    width: 720,
    height: 900,
    fetchpriority: "high",
    loading: "eager",
  });
});

test("the LCP preload and the img share one sizes value", () => {
  const { preload, img: attrs } = lcpImage(img, "50vw");
  expect(preload).toMatchObject({
    imagesrcset: img.srcset,
    imagesizes: "50vw",
    fetchpriority: "high",
  });
  expect(attrs).toMatchObject({ sizes: "50vw", srcset: img.srcset, loading: "eager" });
});

test("consent cookie parsing ignores unknown purposes", () => {
  expect(readConsent("a=1; consent=analytics%2Cevil%2Cads")).toEqual(["analytics", "ads"]);
  expect(readConsent("consent=")).toEqual([]);
  expect(readConsent("x=1")).toBeNull();
});

test("platform messages: placeholders, missing keys, island subsets", () => {
  const m = { "listing.count": "{count} produktů", "cart.add": "Přidat do košíku" };
  expect(t(m, "listing.count", { count: 3 })).toBe("3 produktů");
  expect(t(m, "nope")).toBe("nope");
  expect(pick(m, ["cart.add", "x"])).toEqual({ "cart.add": "Přidat do košíku", x: "x" });
});
