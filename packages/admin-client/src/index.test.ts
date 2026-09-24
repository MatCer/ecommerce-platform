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
