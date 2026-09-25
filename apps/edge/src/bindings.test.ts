import { expect, test } from "vitest";
import {
  ContextRegistry,
  CTX_HEADER,
  restrictedBinding,
  STOREFRONT_OPERATIONS,
} from "./bindings.ts";
import type { Site } from "./sites.ts";

const site = (tenant: string) =>
  ({ tenant_id: tenant, market_id: "m", locale: "cs", storefront_token: `sf_${tenant}` }) as Site;

test("a tenant-scoped instance cannot use another tenant's request context", async () => {
  const registry = new ContextRegistry();
  const seen: string[] = [];
  const binding = (scope: string) =>
    restrictedBinding({
      name: "STOREFRONT",
      operations: STOREFRONT_OPERATIONS,
      registry,
      artifactId: "a1",
      scope,
      apiOrigin: "http://api.test",
      upstream: async (r) => {
        seen.push(r.headers.get("x-tenant") ?? "");
        return Response.json({});
      },
    });
  const ctxA = registry.open(site("t-a"), "a1");
  const call = (scope: string, ctx: string) =>
    binding(scope)(new Request("https://storefront/shop", { headers: { [CTX_HEADER]: ctx } }));

  expect((await call("t-b", ctxA)).status).toBe(403); // borrowed by tenant B's isolate
  expect((await call("t-a", ctxA)).status).toBe(200);
  expect(seen).toEqual(["t-a"]);
});
