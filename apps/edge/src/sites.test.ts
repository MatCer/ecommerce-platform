import { expect, test } from "vitest";
import {
  ApiResolver,
  CachedResolver,
  classifyHost,
  normalizeHost,
  type Site,
  StaticResolver,
  splitLocale,
} from "./sites.ts";

test("splitLocale strips only a non-default locale of the market (spec §9.1)", () => {
  const s = { locale: "sk", locales: ["sk", "cs"] };
  expect(splitLocale(s, "/cs/c/trika")).toEqual({ locale: "cs", path: "/c/trika" });
  expect(splitLocale(s, "/cs")).toEqual({ locale: "cs", path: "/" });
  expect(splitLocale(s, "/cs/")).toEqual({ locale: "cs", path: "/" });
  expect(splitLocale(s, "/cs/_p/public/shop")).toEqual({ locale: "cs", path: "/_p/public/shop" });
  expect(splitLocale(s, "/sk/c/x")).toBeNull(); // the default locale has no prefix
  expect(splitLocale(s, "/en/c/x")).toBeNull(); // not a locale of this market
  expect(splitLocale(s, "/csx/c")).toBeNull();
  expect(splitLocale(s, "/c/trika")).toBeNull();
});

test("the API resolver maps the resolve response and caches nothing on errors", async () => {
  const seen: string[] = [];
  const upstream = async (r: Request) => {
    seen.push(`${r.url} ${r.headers.get("authorization")}`);
    const host = new URL(r.url).searchParams.get("host");
    if (host === "down.localhost") return new Response("boom", { status: 500 });
    if (host !== "demo-sk.localhost") return Response.json({ code: "not_found" }, { status: 404 });
    return Response.json({
      hostname: "demo-sk.localhost",
      tenant_id: "t1",
      tenant_slug: "demo",
      market_id: "m-sk",
      market_code: "sk",
      currency: "EUR",
      default_locale: "sk",
      locales: ["sk", "cs"],
      country_codes: ["SK"],
      storefront_token: "sf_x",
      theme_artifact: "0123456789abcdef0123456789abcdef",
      retained_artifacts: [],
      checkout_artifact: null,
    });
  };
  const r = new ApiResolver("http://api:8000", "service-token", upstream);
  expect(await r.resolve("demo-sk.localhost")).toEqual({
    tenant_id: "t1",
    market_id: "m-sk",
    locale: "sk",
    locales: ["sk", "cs"],
    shop_host: "demo-sk.localhost",
    storefront_token: "sf_x",
    theme_artifact: "0123456789abcdef0123456789abcdef",
    retained_artifacts: [],
    checkout_artifact: null,
  });
  expect(seen[0]).toBe(
    "http://api:8000/internal/v1/resolve?host=demo-sk.localhost Bearer service-token",
  );
  expect(await r.resolve("nope.localhost")).toBeNull();
  await expect(r.resolve("down.localhost")).rejects.toThrow(/500/);
  // A failed lookup is not cached as "unknown shop".
  const cached = new CachedResolver(r);
  await expect(cached.resolve("down.localhost")).rejects.toThrow();
  expect(seen.filter((s) => s.includes("down.localhost")).length).toBe(2);
});

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
