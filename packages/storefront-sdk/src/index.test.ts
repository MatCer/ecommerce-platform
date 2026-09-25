import { expect, test } from "vitest";
import { createStorefrontClient } from "./index";

test("calls the Storefront API with generated types", async () => {
  const seen: string[] = [];
  const client = createStorefrontClient({
    baseUrl: "http://api.test",
    fetch: async (req: Request) => {
      seen.push(`${req.method} ${req.url} ${req.headers.get("x-market")}`);
      return Response.json({ to_path: "/p/new", code: 301 });
    },
  });

  const { data, error } = await client.GET("/storefront/v1/redirects/resolve", {
    params: {
      query: { path: "/old" },
      header: { "X-Storefront-Token": "sf_x", "X-Market": "m-1" },
    },
  });

  expect(error).toBeUndefined();
  expect(data?.to_path).toBe("/p/new");
  expect(seen).toEqual(["GET http://api.test/storefront/v1/redirects/resolve?path=%2Fold m-1"]);
});
