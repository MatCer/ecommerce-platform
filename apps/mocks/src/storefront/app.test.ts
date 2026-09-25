import { expect, test } from "vitest";
import { app } from "../app.ts";

const h = { "x-tenant": "t-demo", "x-market": "m-cz" };
const req = (path: string, init: RequestInit = {}) =>
  app.request(`/storefront/v1${path}`, {
    ...init,
    headers: { ...h, ...(init.headers as Record<string, string>) },
  });

test("category page: 24 per page, facets, filtered URLs are noindex", async () => {
  const page = (await (await req("/pages/category/trika")).json()) as {
    products: unknown[];
    facets: { key: string }[];
    pagination: { pages: number };
    seo: { robots?: string };
    cache: { public: boolean };
  };
  expect(page.products).toHaveLength(24);
  expect(page.facets.map((f) => f.key)).toEqual(["velikost", "barva"]);
  expect(page.seo.robots).toBeUndefined();
  expect(page.cache.public).toBe(true);
  const filtered = (await (await req("/pages/category/trika?velikost=M")).json()) as {
    seo: { robots?: string };
  };
  expect(filtered.seo.robots).toBe("noindex,follow");
});

test("product page in the SK market is priced in EUR", async () => {
  const res = await app.request("/storefront/v1/pages/product/tricko-basic", {
    headers: { ...h, "x-market": "m-sk" },
  });
  const p = (await res.json()) as { product: { variants: { price: { currency: string } }[] } };
  expect(p.product.variants[0]?.price.currency).toBe("EUR");
});

test("cart: capability token, tenant-bound, checkout token for the same cart", async () => {
  const created = await req("/cart", { method: "POST" });
  const token = created.headers.get("x-cart-token") ?? "";
  expect(token).toMatch(/^[A-Za-z0-9_-]{43}$/);
  const add = await req("/cart/lines", {
    method: "POST",
    headers: { "x-cart-token": token, "content-type": "application/json" },
    body: JSON.stringify({ variant_id: "prod-001-01", quantity: 2 }),
  });
  expect(((await add.json()) as { item_count: number }).item_count).toBe(2);
  const other = await app.request("/storefront/v1/cart", {
    headers: { "x-tenant": "t-other", "x-cart-token": token },
  });
  expect(other.status).toBe(404);
  const { token: ct } = (await (
    await req("/cart/checkout-token", { method: "POST", headers: { "x-cart-token": token } })
  ).json()) as { token: string };
  const viaCheckout = await req("/cart", { headers: { "x-cart-token": ct } });
  expect(((await viaCheckout.json()) as { item_count: number }).item_count).toBe(2);
  // Rotated (A4): the pre-handoff shop capability is dead; the checkout one cannot edit lines.
  expect((await req("/cart", { headers: { "x-cart-token": token } })).status).toBe(404);
  const edit = await req("/cart/lines", {
    method: "POST",
    headers: { "x-cart-token": ct, "content-type": "application/json" },
    body: JSON.stringify({ variant_id: "prod-001-01" }),
  });
  expect(edit.status).toBe(404);
});

test("media is served only for safe avif paths", async () => {
  expect((await app.request("/media/p/tee-sand/720.avif")).headers.get("content-type")).toBe(
    "image/avif",
  );
  expect((await app.request("/media/p/../../package.json")).status).toBe(404);
});
