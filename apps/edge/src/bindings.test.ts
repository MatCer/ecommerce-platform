import { expect, test } from "vitest";
import {
  CHECKOUT_OPERATIONS,
  ContextRegistry,
  CTX_HEADER,
  restrictedBinding,
  STOREFRONT_OPERATIONS,
} from "./bindings.ts";
import type { Site } from "./sites.ts";

const wp12Paths = [
  `/withdrawals/${"a".repeat(64)}`,
  "/customer/orders/12345678-1234-1234-1234-123456789abc/withdrawal",
  `/orders/${"a".repeat(64)}/documents`,
  "/customer/orders/12345678-1234-1234-1234-123456789abc/documents",
];
test.each(wp12Paths)("checkout binding permits only GET for %s", (path) => {
  expect(CHECKOUT_OPERATIONS.some((o) => o.method === "GET" && o.path.test(path))).toBe(true);
  expect(CHECKOUT_OPERATIONS.some((o) => o.method === "POST" && o.path.test(path))).toBe(false);
  const invalid = path.replace(/a{64}|12345678-1234-1234-1234-123456789abc/, "short");
  expect(CHECKOUT_OPERATIONS.some((o) => o.path.test(invalid))).toBe(false);
});

test("review links: the checkout reads the invitation, nobody submits through a binding", () => {
  const can = (ops: typeof CHECKOUT_OPERATIONS, method: string, path: string) =>
    ops.some((o) => o.method === method && o.path.test(path));
  expect(can(CHECKOUT_OPERATIONS, "GET", "/reviews/invitation")).toBe(true);
  expect(can(CHECKOUT_OPERATIONS, "POST", "/reviews")).toBe(false);
  expect(can(STOREFRONT_OPERATIONS, "GET", "/reviews/invitation")).toBe(false);
});

const site = (tenant: string) =>
  ({
    tenant_id: tenant,
    market_id: "m",
    locale: "cs",
    locales: ["cs"],
    storefront_token: `sf_${tenant}`,
  }) as Site;

test.each(wp12Paths)("checkout SSR forwards scoped credentials for %s", async (path) => {
  const registry = new ContextRegistry();
  const seen: Request[] = [];
  const binding = restrictedBinding({
    name: "CHECKOUT",
    operations: CHECKOUT_OPERATIONS,
    registry,
    artifactId: "checkout",
    scope: "t-a",
    apiOrigin: "http://api.test",
    upstream: async (request) => {
      seen.push(request);
      return Response.json({ items: [] });
    },
  });
  const ctx = registry.open(site("t-a"), "checkout", { sessionToken: "customer-session" });
  const response = await binding(
    new Request(`https://checkout${path}`, { headers: { [CTX_HEADER]: ctx } }),
  );
  expect(response.status).toBe(200);
  expect(seen[0]?.url).toBe(`http://api.test/storefront/v1${path}`);
  expect(seen[0]?.headers.get("x-customer-session")).toBe("customer-session");
  expect(seen[0]?.headers.get("x-tenant")).toBe("t-a");
  expect((await binding(new Request(`https://checkout${path}`))).status).toBe(403);
  expect(seen).toHaveLength(1);
});

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
