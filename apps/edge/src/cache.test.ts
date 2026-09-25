import { describe, expect, test } from "vitest";
import {
  type CacheEntry,
  HtmlCache,
  normalizeUrl,
  type RequestFacts,
  requestVerdict,
  responseVerdict,
} from "./cache.ts";

const req = (over: Partial<RequestFacts> & { path?: string } = {}): RequestFacts => ({
  method: "GET",
  url: new URL(`https://demo.localhost${over.path ?? "/p/shirt"}`),
  origin: "shop",
  preview: false,
  headers: new Headers(),
  ...over,
});

const res = (
  headers: Record<string, string> = {},
  over: Partial<{ status: number; anyPrivate: boolean; minMaxAge: number | null }> = {},
) => ({
  status: over.status ?? 200,
  headers: new Headers(headers),
  pageModels: { anyPrivate: over.anyPrivate ?? false, minMaxAge: over.minMaxAge ?? null },
});

describe("A2 request allowlist", () => {
  test.each([
    "/",
    "/c/shirts",
    "/c/men/shirts",
    "/p/shirt",
    "/pages/about",
    "/blog",
    "/blog/post",
    "/search",
    "/sk/p/shirt",
    "/en/",
  ])("caches %s", (path) => expect(requestVerdict(req({ path }))).toEqual({ cache: true }));

  test.each([
    ["POST", req({ method: "POST" }), "method"],
    ["checkout origin", req({ origin: "checkout" }), "checkout origin"],
    ["preview", req({ preview: true }), "preview"],
    ["/_p/*", req({ path: "/_p/cart" }), "platform route"],
    [
      "Authorization",
      req({ headers: new Headers({ authorization: "Bearer x" }) }),
      "authorization",
    ],
    ["?token=", req({ path: "/p/x?token=1" }), "capability token in URL"],
    ["?h=", req({ path: "/p/x?h=1" }), "capability token in URL"],
    ["?sig=", req({ path: "/p/x?sig=1" }), "capability token in URL"],
    ["account path", req({ path: "/account" }), "not allowlisted"],
    ["nested product path", req({ path: "/p/a/b" }), "not allowlisted"],
    ["api-looking path", req({ path: "/storefront/v1/cart" }), "not allowlisted"],
  ])("never caches: %s", (_n, r, reason) => {
    expect(requestVerdict(r)).toEqual({ cache: false, reason });
  });
});

describe("A2 response rules", () => {
  test("default TTL 60 s; themes and page models can only lower it", () => {
    expect(responseVerdict(res())).toEqual({ ttl: 60 });
    expect(responseVerdict(res({ "cache-control": "public, max-age=31536000" }))).toEqual({
      ttl: 60,
    });
    expect(responseVerdict(res({ "cache-control": "public, s-maxage=10, max-age=600" }))).toEqual({
      ttl: 10,
    });
    expect(responseVerdict(res({}, { minMaxAge: 5 }))).toEqual({ ttl: 5 });
  });

  test.each([
    ["Set-Cookie", res({ "set-cookie": "a=1" }), "set-cookie"],
    ["Cache-Control: private", res({ "cache-control": "private, max-age=60" }), "cache-control"],
    ["Cache-Control: no-store", res({ "cache-control": "no-store" }), "cache-control"],
    ["Cache-Control: no-cache", res({ "cache-control": "no-cache" }), "cache-control"],
    ["private page model", res({}, { anyPrivate: true }), "private page model"],
    ["non-200", res({}, { status: 404 }), "status"],
    ["max-age=0", res({ "cache-control": "max-age=0" }), "ttl 0"],
  ])("never caches: %s", (_n, r, reason) => {
    expect(responseVerdict(r)).toEqual({ cache: false, reason });
  });
});

test("tracking params are dropped and the rest sorted", () => {
  expect(
    normalizeUrl(new URL("https://d.localhost/c/x?utm_source=a&b=2&gclid=z&a=1&fbclid=q")).search,
  ).toBe("?a=1&b=2");
});

const entry = (over: Partial<CacheEntry> = {}): CacheEntry => ({
  status: 200,
  headers: [],
  body: new Uint8Array(10),
  storedAt: 0,
  ttlMs: 60_000,
  tags: ["product:1"],
  tenantId: "t1",
  ...over,
});

describe("HtmlCache", () => {
  test("fresh, then stale within SWR, then gone", () => {
    const c = new HtmlCache();
    c.set("k", entry());
    expect(c.get("k", 59_999)?.fresh).toBe(true);
    expect(c.get("k", 60_000)?.fresh).toBe(false);
    expect(c.get("k", 360_000)).toBeNull();
  });

  test("purge by tag is scoped to the tenant when both are given", () => {
    const c = new HtmlCache();
    c.set("a", entry());
    c.set("b", entry({ tenantId: "t2" }));
    expect(c.purge({ tenantId: "t2", tags: ["product:1"] })).toBe(1);
    expect(c.get("a", 1)).not.toBeNull();
    expect(c.purge({})).toBe(0);
    expect(c.purge({ all: true })).toBe(1);
  });

  test("evicts least recently used entries beyond the byte budget (metadata counts)", () => {
    const c = new HtmlCache(16 * 400); // each entry costs ≥ 256 B of overhead + body
    c.set("a", entry());
    c.set("b", entry());
    c.get("a", 1);
    for (let i = 0; i < 30; i++) c.set(`x${i}`, entry());
    expect(c.get("b", 1)).toBeNull();
    expect(c.size).toBeLessThan(24);
  });

  test("a render started before a purge cannot write its stale result", () => {
    const c = new HtmlCache();
    const gen = c.generation;
    c.purge({ tags: ["product:1"] });
    c.set("k", entry(), gen);
    expect(c.get("k", 1)).toBeNull();
    c.set("k", entry(), c.generation);
    expect(c.get("k", 1)).not.toBeNull();
  });
});
