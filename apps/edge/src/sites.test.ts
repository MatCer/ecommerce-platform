import { expect, test } from "vitest";
import { CachedResolver, classifyHost, normalizeHost, type Site, StaticResolver } from "./sites.ts";

test.each([
  ["Demo.Localhost:8280", "demo.localhost"],
  ["demo.localhost.", "demo.localhost"],
  ["checkout.demo.localhost", "checkout.demo.localhost"],
  ["evil.com/x", null],
  ["a b", null],
  ["", null],
  ["-bad.localhost", null],
])("normalizeHost(%s)", (raw, want) => expect(normalizeHost(raw)).toBe(want));

test("checkout origin maps to its shop", () => {
  expect(classifyHost("checkout.demo.localhost")).toEqual({
    kind: "checkout",
    shopHost: "demo.localhost",
  });
  expect(classifyHost("demo.localhost")).toEqual({ kind: "shop", shopHost: "demo.localhost" });
});

test("resolution is cached 60 s and purged per tenant", async () => {
  const site = { tenant_id: "t1", theme_artifact: "a" } as Site;
  const inner = new StaticResolver({ "demo.localhost": site });
  const cached = new CachedResolver(inner, 60_000);
  expect((await cached.resolve("demo.localhost", 0))?.theme_artifact).toBe("a");
  inner.set("demo.localhost", { ...site, theme_artifact: "b" });
  expect((await cached.resolve("demo.localhost", 59_000))?.theme_artifact).toBe("a");
  cached.purge("t1");
  expect((await cached.resolve("demo.localhost", 59_001))?.theme_artifact).toBe("b");
  expect(await cached.resolve("__proto__", 0)).toBeNull();
});
