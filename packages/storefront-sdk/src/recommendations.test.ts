import { afterEach, expect, test, vi } from "vitest";
import { recentlyViewed, recommendations } from "./recommendations.ts";

const calls: { url: string; init?: RequestInit }[] = [];
vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
  calls.push({ url, init });
  return Response.json({ strategy: null, title: null, products: [], cache: {} });
});
afterEach(() => {
  calls.length = 0;
});

test("private reads go unprefixed with the cookies; the locale is a parameter", async () => {
  await recommendations({ context: "cart", limit: 4 }, { locale: "cs" });
  expect(calls[0]?.url).toBe("/_p/recommendations?context=cart&limit=4&locale=cs");
  expect(calls[0]?.init?.credentials).toBe("same-origin");
});

test("recently viewed is rehydrated on the public route without cookies (A20)", async () => {
  await recentlyViewed(["a", "b"], { base: "/cs" });
  expect(calls[0]?.url).toBe("/cs/_p/public/recommendations?context=recent&ids=a%2Cb&limit=2");
  expect(calls[0]?.init?.credentials).toBe("omit");
});
