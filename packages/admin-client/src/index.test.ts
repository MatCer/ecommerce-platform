import { expect, test } from "vitest";
import { createAdminClient } from "./index";

test("calls the API with generated types", async () => {
  const seen: string[] = [];
  const client = createAdminClient({
    baseUrl: "http://api.test",
    fetch: async (req: Request) => {
      seen.push(`${req.method} ${req.url}`);
      return Response.json({ status: "ok" });
    },
  });

  const { data, error } = await client.GET("/healthz");

  expect(error).toBeUndefined();
  expect(data?.status).toBe("ok");
  expect(seen).toEqual(["GET http://api.test/healthz"]);
});

test("admin endpoints are typed with the tenant header", async () => {
  const seen: Array<[string, string | null]> = [];
  const client = createAdminClient({
    baseUrl: "http://api.test",
    fetch: async (req: Request) => {
      seen.push([req.url, req.headers.get("x-tenant-id")]);
      return Response.json({ items: [] });
    },
  });

  const tenant = "0190f1f0-0000-7000-8000-000000000000";
  const { data } = await client.GET("/admin/v1/markets", {
    params: { header: { "X-Tenant-Id": tenant } },
  });

  expect(data?.items).toEqual([]);
  expect(seen).toEqual([["http://api.test/admin/v1/markets", tenant]]);
});
