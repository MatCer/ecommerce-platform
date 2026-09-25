import { expect, test } from "vitest";
import { createStorefront, StorefrontError } from "./server.ts";

const request = new Request("https://demo.localhost/p/x", {
  headers: { "x-platform-ctx": "ctx123" },
});

function binding(status = 200, body: unknown = { ok: true }) {
  const seen: { url: string; headers: Record<string, string> }[] = [];
  return {
    seen,
    fetch: async (url: string, init?: RequestInit) => {
      seen.push({ url, headers: Object.fromEntries(new Headers(init?.headers)) });
      return Response.json(body, { status });
    },
  };
}

test("forwards the request context and encodes slugs and queries", async () => {
  const b = binding();
  const sf = createStorefront({ binding: b, request });
  await sf.product("tričko?x=1");
  await sf.category("muzi/trika", { color: ["red", "blue"], sort: "price" });
  expect(b.seen.map((s) => s.url)).toEqual([
    "https://storefront/pages/product/tri%C4%8Dko%3Fx%3D1",
    "https://storefront/pages/category/muzi/trika?color=red&color=blue&sort=price",
  ]);
  expect(b.seen[0]?.headers["x-platform-ctx"]).toBe("ctx123");
});

test("404 → null for optional pages, error otherwise", async () => {
  expect(await createStorefront({ binding: binding(404), request }).product("x")).toBeNull();
  await expect(createStorefront({ binding: binding(404), request }).shop()).rejects.toBeInstanceOf(
    StorefrontError,
  );
  await expect(
    createStorefront({ binding: binding(500, { code: "boom" }), request }).product("x"),
  ).rejects.toMatchObject({
    status: 500,
    code: "boom",
  });
});
