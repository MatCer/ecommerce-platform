import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, checkoutWorker, fakeApi, site } from "./fixtures.ts";

const catchAllTheme = `
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const ctx = request.headers.get("x-platform-ctx");
    await env.STOREFRONT.fetch("https://storefront/shop", { headers: { "x-platform-ctx": ctx } });
    return new Response("<!doctype html>theme:" + url.pathname, { headers: { "content-type": "text/html" } });
  },
};
`;

let gw: Gateway;
let api: ReturnType<typeof fakeApi>;

const get = (url: string, headers: Record<string, string> = {}) =>
  gw.fetch(new Request(url, { headers: { host: new URL(url).host, ...headers } }));

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop115-artifacts-"));
  const theme = await buildArtifact(root, "theme", catchAllTheme, {
    "_astro/app.js": "console.log(1)",
    "favicon.svg": "<svg/>",
  });
  const checkout = await buildArtifact(root, "checkout", checkoutWorker, {
    "_astro/checkout.js": "1",
  });
  api = fakeApi();
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "demo.localhost": site({ theme_artifact: theme.id, locales: ["cs", "en"] }),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
    renderTimeoutMs: 1500,
  });
});

afterAll(async () => {
  await gw?.dispose();
});

describe("service workers (C3)", () => {
  test("worker_denied_before_asset_and_cache", async () => {
    expect((await get("http://demo.localhost/")).headers.get("x-edge-cache")).toBe("MISS");
    expect((await get("http://demo.localhost/")).headers.get("x-edge-cache")).toBe("HIT");
    const sw = { "service-worker": "script" };
    for (const url of [
      "http://demo.localhost/",
      "http://demo.localhost/_astro/app.js",
      "http://demo.localhost/favicon.svg",
      "http://demo.localhost/_p/speculation-rules.json",
      "http://demo.localhost/en/",
      "http://checkout.demo.localhost/",
      "http://checkout.demo.localhost/_astro/checkout.js",
      "http://preview-1--demo.localhost/",
      "http://unknown.localhost/sw.js",
    ]) {
      const res = await get(url, sw);
      expect(res.status, url).toBe(403);
      expect(res.headers.get("cache-control"), url).toBe("no-store");
      expect(res.headers.get("x-edge-cache"), url).toBeNull();
      expect(await res.text(), url).toBe("");
    }
  });
});

describe("reserved platform namespace (C8)", () => {
  const expectPlatform404 = async (url: string) => {
    const res = await get(url);
    expect(res.status, url).toBe(404);
    expect(res.headers.get("cache-control"), url).toBe("no-store");
    expect(res.headers.get("x-edge-cache"), url).toBeNull();
    expect(await res.text(), url).not.toContain("theme:");
  };

  test("all_reserved_routes_precede_locale_theme", async () => {
    for (const p of [
      "/checkout",
      "/checkout/identify",
      "/o/abc",
      "/withdraw",
      "/review",
      "/restore-cart",
      "/consent",
      "/newsletter",
      "/newsletter/confirm",
      "/watch/confirm",
      "/flows/unsubscribe",
      "/account",
      "/account/orders/1",
      "/_checkout/b1/app.js",
      "/_p",
      "/_edge",
    ])
      await expectPlatform404(`http://demo.localhost${p}`);
    for (const p of ["/en/checkout", "/en/account", "/de/withdraw", "/en-GB/o/abc", "/cs/review"])
      await expectPlatform404(`http://demo.localhost${p}`);
  });

  test("encoded_and_double_slash_paths_cannot_shadow", async () => {
    for (const p of [
      "//checkout",
      "/%63heckout",
      "/%2563heckout",
      "/CHECKOUT",
      "/Account/",
      "/x/../checkout",
      "/%2e%2e/checkout",
      "/%2Fcheckout",
      "/%5Ccheckout",
      "/en//checkout",
      "/%5Fp/cart",
    ])
      await expectPlatform404(`http://demo.localhost${p}`);
  });

  test("other theme paths still render", async () => {
    for (const p of ["/", "/p/checkout", "/c/account", "/pages/consent", "/checkouts", "/en/p/x"]) {
      const res = await get(`http://demo.localhost${p}`);
      expect(res.status, p).toBe(200);
      expect(await res.text(), p).toContain("theme:");
    }
    expect((await get("http://demo.localhost/favicon.svg")).status).toBe(200);
    expect((await get("http://demo.localhost/_p/speculation-rules.json")).status).toBe(200);
  });
});

test("speculation rules never prerender a platform path or its localized variant", async () => {
  const rules = (await (await get("http://demo.localhost/_p/speculation-rules.json")).json()) as {
    prerender: { where: { and: { not?: { href_matches?: string[] } }[] } }[];
  };
  const excluded = rules.prerender[0]?.where.and.flatMap((c) => c.not?.href_matches ?? []);
  expect(excluded).toEqual(
    expect.arrayContaining([
      "/_p/*",
      "/_checkout/*",
      "/checkout",
      "/checkout/*",
      "/*/checkout",
      "/o/*",
      "/account",
      "/*/account/*",
      "/newsletter/*",
      "/consent",
    ]),
  );
  expect(excluded).not.toContain("/p/*");
});

describe("response headers (C2, C3)", () => {
  test("theme pages: worker-src none, COOP same-origin-allow-popups, handoff form-action kept", async () => {
    const res = await get("http://demo.localhost:8280/p/x");
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp).toContain("worker-src 'none'");
    expect(csp).toContain("form-action 'self' http://checkout.demo.localhost:8280");
    expect(res.headers.get("cross-origin-opener-policy")).toBe("same-origin-allow-popups");
  });

  test("checkout origin: worker-src none, COOP same-origin", async () => {
    const res = await get("http://checkout.demo.localhost/");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-security-policy")).toContain("worker-src 'none'");
    expect(res.headers.get("cross-origin-opener-policy")).toBe("same-origin");
  });
});
