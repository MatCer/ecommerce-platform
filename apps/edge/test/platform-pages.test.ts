import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway, type GatewayOptions } from "../src/gateway.ts";
import { type CheckoutPolicy, StaticResolver } from "../src/sites.ts";
import {
  buildArtifact,
  fakeApi,
  hostileTheme,
  platformCheckoutWorker,
  REVOKED_CAPABILITY,
  site,
} from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const FRESH = "http://fresh.localhost:8280";
const DEMO = "http://demo.localhost:8280";
const SECURE = "https://secure.localhost:8443";
const PREVIEW_TOKEN = `${"a".repeat(32)}.1900000000.${"b".repeat(64)}`;
const CAP = "sameorigintoken_00000000000000000042";
const COOKIE = `__Host-Http-cart=${CAP}`;
const ASSET = "/_checkout/0123456789abcdef/app.js";
const pageTokenOf = (host: string, capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|${host}|${capability}`).digest("base64url");
const TOKEN = pageTokenOf("fresh.localhost:8280", CAP);

const NAVIGATE = {
  "sec-fetch-site": "none",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "sec-fetch-user": "?1",
};
const PAGE_PATHS = [
  "/checkout",
  "/CHECKOUT",
  "/cs/checkout",
  "/cs/CHECKOUT",
  "/%63heckout",
  "//checkout",
  "/o/x",
  "/withdraw",
  "/checkout/x",
];
const REFUSED: [string, Record<string, string>][] = [
  ["prefetch", { ...NAVIGATE, "sec-purpose": "prefetch" }],
  ["prerender", { ...NAVIGATE, "sec-purpose": "prefetch;prerender" }],
  [
    "fetch or XHR",
    { "sec-fetch-site": "same-origin", "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" },
  ],
  [
    "same-origin fetch",
    { "sec-fetch-site": "same-origin", "sec-fetch-mode": "same-origin", "sec-fetch-dest": "empty" },
  ],
  [
    "no-cors",
    { "sec-fetch-site": "same-origin", "sec-fetch-mode": "no-cors", "sec-fetch-dest": "empty" },
  ],
  ...["iframe", "frame", "embed", "object"].map((dest): [string, Record<string, string>] => [
    dest,
    { ...NAVIGATE, "sec-fetch-site": "same-origin", "sec-fetch-dest": dest },
  ]),
];

/** Added by Node's fetch and Miniflare on the way into the worker, not by the edge. */
const TRANSPORT = new Set([
  "accept-encoding",
  "accept-language",
  "cf-connecting-ip",
  "connection",
  "host",
  "sec-fetch-mode",
  "user-agent",
]);
const workerHeaderNames = (html: string) =>
  (/<p id="headers">([^<]*)<\/p>/.exec(html)?.[1] ?? "")
    .split(",")
    .filter((name) => name && !TRANSPORT.has(name));

const sameOriginPolicy = (origin: string): CheckoutPolicy => ({
  origin,
  mode: "same_origin",
  mode_generation: 1,
  checkout_origin: origin,
});

let api: ReturnType<typeof fakeApi>;
let options: GatewayOptions;
let gw: Gateway;

const get = (
  url: string,
  headers: Record<string, string> = {},
  init: RequestInit = {},
  gateway = gw,
) => gateway.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

function expectPlatformHeaders(res: Response, label: string) {
  expect(res.headers.get("cache-control"), label).toBe("no-store");
  expect(res.headers.get("referrer-policy"), label).toBe("strict-origin");
  expect(res.headers.get("x-frame-options"), label).toBe("DENY");
  expect(res.headers.get("cross-origin-opener-policy"), label).toBe("same-origin");
  expect(res.headers.get("x-content-type-options"), label).toBe("nosniff");
  expect(res.headers.get("permissions-policy"), label).toBe(
    'camera=(), microphone=(), geolocation=(), usb=(), payment=(self "https://js.stripe.com")',
  );
  const csp = res.headers.get("content-security-policy") ?? "";
  expect(csp, label).toContain("frame-ancestors 'none'");
  expect(csp, label).toContain("worker-src 'none'");
  expect(res.headers.getSetCookie(), label).toEqual([]);
}

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop119-pages-"));
  const theme = await buildArtifact(root, "theme", hostileTheme("so"), { "_astro/app.so.js": "1" });
  const checkout = await buildArtifact(root, "checkout", platformCheckoutWorker, {
    [ASSET.slice(1)]: "console.log('checkout')",
  });
  api = fakeApi();
  const fresh = site({
    tenant_id: "t-fresh",
    shop_host: "fresh.localhost",
    theme_artifact: theme.id,
    checkout_origins: [sameOriginPolicy(FRESH)],
  });
  options = {
    artifactRoot: root,
    resolver: new StaticResolver({
      "fresh.localhost": fresh,
      "demo.localhost": site({ theme_artifact: theme.id }),
      "secure.localhost": site({
        tenant_id: "t-secure",
        shop_host: "secure.localhost",
        theme_artifact: theme.id,
        checkout_origins: [sameOriginPolicy(SECURE)],
      }),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
    renderTimeoutMs: 1500,
    log: () => {},
    packetaWidgetUrl: "http://mocks.localhost:8280/packeta/library.js",
    pageTokenSecret: SECRET,
    previews: {
      resolve: async (host, token) =>
        host === "preview-1--fresh.localhost" && token === PREVIEW_TOKEN
          ? { ...fresh, preview: { revision: 1, expiresAt: Date.now() + 60_000 } }
          : null,
    },
  };
  gw = createGateway(options);
});

beforeEach(() => {
  api.calls.length = 0;
});

afterAll(async () => {
  await gw?.dispose();
});

async function bodyAndHeaders(res: Response) {
  return `${[...res.headers].map(([k, v]) => `${k}: ${v}`).join("\n")}\n\n${await res.text()}`;
}

describe("same-origin platform pages answer top-level navigations only (C1)", () => {
  describe.each(PAGE_PATHS)("%s", (page) => {
    test.each(REFUSED)(
      "%s gets 403 with no body before any credential work",
      async (_, headers) => {
        const res = await get(`${FRESH}${page}`, { ...headers, cookie: COOKIE });
        expect(res.status).toBe(403);
        expect(await res.text()).toBe("");
        expectPlatformHeaders(res, page);
        expect(api.calls).toEqual([]);
      },
    );

    test("a browser without Fetch Metadata gets the static unsupported page", async () => {
      const res = await get(`${FRESH}${page}`, { cookie: COOKIE });
      expect(res.status).toBe(400);
      expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
      const html = await res.text();
      expect(html).toContain('lang="cs"');
      expect(html).toMatch(/Chrome.*Firefox.*Safari.*Edge/s);
      expect(html).toContain('<meta name="robots" content="noindex">');
      expect(html).not.toMatch(/<script|<style|page-token/);
      expectPlatformHeaders(res, page);
      expect(api.calls).toEqual([]);
    });

    test("a top-level document navigation renders /checkout and 404s the rest", async () => {
      const res = await get(`${FRESH}${page}`, { ...NAVIGATE, cookie: COOKIE });
      expectPlatformHeaders(res, page);
      if (page === "/checkout") {
        expect(res.status).toBe(200);
        expect(api.calls.map((c) => c.url)).toEqual(["http://api.test/storefront/v1/checkout"]);
      } else {
        expect(res.status).toBe(404);
        expect(await res.text()).toBe("Not found");
        expect(api.calls).toEqual([]);
      }
    });
  });

  test("POST to /checkout is 405 with the platform headers", async () => {
    const res = await get(
      `${FRESH}/checkout`,
      { ...NAVIGATE, "sec-fetch-site": "same-origin", origin: FRESH, cookie: COOKIE },
      { method: "POST", body: "x=1" },
    );
    expect(res.status).toBe(405);
    expect(res.headers.get("allow")).toBe("GET, HEAD");
    expectPlatformHeaders(res, "POST /checkout");
    expect(api.calls).toEqual([]);
  });
});

describe("the same-origin render gets the request's own capability and its page token (C4)", () => {
  const checkoutRead = () =>
    api.calls.find((c) => c.url === "http://api.test/storefront/v1/checkout");

  test("only __Host-Http-cart reaches the API; the worker request carries only edge headers", async () => {
    const res = await get(`${FRESH}/checkout`, {
      ...NAVIGATE,
      cookie: [
        COOKIE,
        "__Host-cart=checkouttoken_000000000001",
        "__Host-sid=sessiontoken_000000000001",
        `__Secure-consent_id=${"a".repeat(32)}`,
        "__Secure-consent_exp=1",
        "consent=analytics",
      ].join("; "),
      authorization: "Bearer stolen",
    });
    expect(res.status).toBe(200);
    const read = checkoutRead();
    expect(read?.headers["x-cart-token"]).toBe(CAP);
    expect(read?.headers["x-shop-origin"]).toBe(FRESH);
    expect(read?.headers["x-customer-session"]).toBeUndefined();
    expect(read?.headers["x-consent-subject"]).toBeUndefined();
    expect(workerHeaderNames(await res.text())).toEqual([
      "accept",
      "x-platform-ctx",
      "x-platform-page-token",
    ]);
  });

  test("the page's meta holds the token for fresh.localhost:8280 and that capability", async () => {
    const html = await (await get(`${FRESH}/checkout`, { ...NAVIGATE, cookie: COOKIE })).text();
    expect(html).toContain(`<meta name="page-token" content="${TOKEN}">`);
  });

  test.each([
    ["no cookie", {}],
    ["a malformed cookie", { cookie: "__Host-Http-cart=not a capability" }],
  ])("%s: no token for the worker and no capability for the API", async (_, extra) => {
    const res = await get(`${FRESH}/checkout`, { ...NAVIGATE, ...extra });
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(workerHeaderNames(html)).toEqual(["accept", "x-platform-ctx"]);
    expect(html).not.toContain('<meta name="page-token"');
    expect(checkoutRead()?.headers["x-cart-token"]).toBeUndefined();
  });

  test("a capability the API no longer opens gets the restart notice and no token", async () => {
    const res = await get(`${FRESH}/checkout`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cart=${REVOKED_CAPABILITY}`,
    });
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain('<p role="status">restarted</p>');
    expect(html).not.toContain('<meta name="page-token"');
    expect(html).not.toContain(pageTokenOf("fresh.localhost:8280", REVOKED_CAPABILITY));
  });

  test("the rendered page has the checkout CSP with Stripe and the pickup-point widget", async () => {
    const res = await get(`${FRESH}/checkout`, { ...NAVIGATE, cookie: COOKIE });
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp).toMatch(
      /script-src 'self' 'sha256-[^;]* https:\/\/js\.stripe\.com [^;]*http:\/\/mocks\.localhost:8280/,
    );
    expect(csp).toContain("connect-src 'self' https://api.stripe.com");
    expect(csp).toMatch(/frame-src https:\/\/js\.stripe\.com [^;]*http:\/\/mocks\.localhost:8280/);
    expect(csp).toContain("form-action 'self'");
    expect(csp).toContain("frame-ancestors 'none'");
    expect(csp).toContain("worker-src 'none'");
  });
});

describe("the page token reaches nothing but the admitted /checkout HTML (C4)", () => {
  test("no other route, refusal or API call carries it", async () => {
    const cookie = `${COOKIE}; cart=carttoken_00000000000000000001`;
    const start = {
      origin: FRESH,
      "sec-fetch-site": "same-origin",
      "sec-fetch-mode": "navigate",
      "sec-fetch-dest": "document",
      "content-type": "application/x-www-form-urlencoded",
    };
    const requests: [string, Record<string, string>, RequestInit?][] = [
      ["/_p/cart", {}],
      ["/_p/consent", {}],
      ["/_p/checkout/start", start, { method: "POST", body: "" }],
      ["/_p/checkout", { "x-page-token": TOKEN }],
      ["/_p/public/shop", {}],
      ["/_p/speculation-rules.json", {}],
      ["/_p/tokens.css", {}],
      [ASSET, {}],
      ["/", NAVIGATE],
      ["/checkout", { ...NAVIGATE, "sec-purpose": "prefetch" }],
      ["/checkout", { "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" }],
      ["/checkout", { ...NAVIGATE, "sec-fetch-dest": "iframe" }],
      ["/checkout", {}],
      ["/CHECKOUT", NAVIGATE],
      ["/checkout", { ...NAVIGATE, origin: FRESH }, { method: "POST", body: "" }],
    ];
    for (const [route, headers, init] of requests) {
      const res = await get(`${FRESH}${route}`, { ...headers, cookie }, init);
      expect(await bodyAndHeaders(res), `${init?.method ?? "GET"} ${route}`).not.toContain(TOKEN);
    }
    const admitted = await get(`${FRESH}/checkout`, { ...NAVIGATE, cookie });
    expect(await admitted.text(), "positive control").toContain(TOKEN);
    expect(JSON.stringify(api.calls), "the API never receives it").not.toContain(TOKEN);
  });

  test("a theme page on the same origin sees neither the token nor the capability", async () => {
    const res = await get(`${FRESH}/pages/headers`, { ...NAVIGATE, cookie: COOKIE });
    expect(res.status).toBe(200);
    const seen = await res.text();
    expect(seen).not.toContain(TOKEN);
    expect(seen).not.toContain(CAP);
    expect(seen).not.toContain("x-platform-page-token");
    const probe = await get(`${FRESH}/probe`, { ...NAVIGATE, cookie: COOKIE });
    expect(((await probe.json()) as { cartViaBinding: unknown }).cartViaBinding).toBe(404);
  });
});

describe("checkout assets and tokens CSS are served on the shop, never navigation gated (C8)", () => {
  test.each([
    ["script metadata", { "sec-fetch-mode": "no-cors", "sec-fetch-dest": "script" }],
    ["no metadata", {}],
    ["a prefetch", { "sec-purpose": "prefetch", "sec-fetch-mode": "no-cors" }],
  ])("a checkout asset with %s is immutable and sandboxed", async (_, headers) => {
    const res = await get(`${FRESH}${ASSET}`, headers);
    expect(res.status).toBe(200);
    expect(await res.text()).toBe("console.log('checkout')");
    expect(res.headers.get("content-type")).toBe("text/javascript; charset=utf-8");
    expect(res.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    expect(res.headers.get("content-security-policy")).toContain("default-src 'none'");
    expect(res.headers.get("content-security-policy")).toContain("sandbox");
  });

  test("an unknown checkout asset is 404; writes are 405", async () => {
    expect((await get(`${FRESH}/_checkout/0123456789abcdef/missing.js`)).status).toBe(404);
    const write = await get(`${FRESH}${ASSET}`, {}, { method: "POST", body: "x" });
    expect(write.status).toBe(405);
    expect(write.headers.get("allow")).toBe("GET, HEAD");
  });

  test("/_p/tokens.css serves the shop theme's tokens", async () => {
    const res = await get(`${FRESH}/_p/tokens.css`, { "sec-fetch-mode": "no-cors" });
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/css; charset=utf-8");
    expect(await res.text()).toContain("--color-brand:#123456;");
  });
});

describe("isolated shops, previews and the checkout origin keep today's answers (C8)", () => {
  test("an isolated shop 404s platform pages, checkout assets and the checkout API, ungated", async () => {
    const page = await get(`${DEMO}/checkout`, {
      ...NAVIGATE,
      "sec-purpose": "prefetch",
      cookie: COOKIE,
    });
    expect(page.status).toBe(404);
    expect(await page.text()).toBe("Not found");
    expect(page.headers.get("x-frame-options")).toBeNull();
    expect((await get(`${DEMO}${ASSET}`)).status).toBe(404);
    const route = await get(`${DEMO}/_p/checkout`, {
      cookie: COOKIE,
      "x-page-token": pageTokenOf("demo.localhost:8280", CAP),
    });
    expect(route.status).toBe(404);
    expect(await route.json()).toMatchObject({ code: "not_found" });
    expect(api.calls).toEqual([]);
  });

  test("a preview of a same-origin shop is not gated", async () => {
    const res = await get("http://preview-1--fresh.localhost:8280/checkout", {
      "sec-purpose": "prefetch",
      cookie: `__Host-preview=${PREVIEW_TOKEN}`,
    });
    expect(res.status).toBe(404);
    expect(await res.text()).toBe("Not found");
    expect(res.headers.get("x-frame-options")).toBeNull();
  });

  test.each(["checkout.demo.localhost:8280", "checkout.fresh.localhost:8280"])(
    "%s does not serve the same-origin page and serves /_checkout/ immutable",
    async (host) => {
      for (const p of ["/checkout", "/checkout/", "/%63heckout"]) {
        const res = await get(`http://${host}${p}`, { ...NAVIGATE, cookie: COOKIE });
        expect(res.status, p).toBe(404);
        expect(await res.text(), p).toBe("Not found");
      }
      const asset = await get(`http://${host}${ASSET}`);
      expect(asset.status).toBe(200);
      expect(asset.headers.get("cache-control")).toBe("public, max-age=31536000, immutable");
    },
  );
});

describe("a local TLS request's shop origin follows Caddy's X-Forwarded-Proto", () => {
  const atSecure = (headers: Record<string, string>, gateway = gw) =>
    get(
      "http://secure.localhost:8443/checkout",
      { ...NAVIGATE, cookie: COOKIE, ...headers },
      {},
      gateway,
    );
  const shopOriginSent = () =>
    api.calls.find((c) => c.url === "http://api.test/storefront/v1/checkout")?.headers[
      "x-shop-origin"
    ];

  test("scheme http plus x-forwarded-proto https is the https origin", async () => {
    const res = await atSecure({ "x-forwarded-proto": "https" });
    expect(res.status).toBe(200);
    expect(await res.text()).toContain(
      `<meta name="page-token" content="${pageTokenOf("secure.localhost:8443", CAP)}">`,
    );
    expect(shopOriginSent()).toBe(SECURE);
  });

  test("without it the origin is http://secure.localhost:8443, an isolated one", async () => {
    const res = await atSecure({});
    expect(res.status).toBe(404);
    expect(await res.text()).toBe("Not found");
    expect(res.headers.get("x-frame-options")).toBeNull();
    expect(api.calls).toEqual([]);
  });

  test("a gateway with scheme https ignores x-forwarded-proto http", async () => {
    const https = createGateway({ ...options, scheme: "https" });
    try {
      const res = await atSecure({ "x-forwarded-proto": "http" }, https);
      expect(res.status).toBe(200);
      expect(shopOriginSent()).toBe(SECURE);
    } finally {
      await https.dispose();
    }
  });
});

test("the withdrawal referrer rule keeps a same-origin platform page's strict-origin", async () => {
  for (const headers of [NAVIGATE, { "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" }]) {
    const res = await get(`${FRESH}/withdraw`, headers);
    expect(res.headers.get("referrer-policy")).toBe("strict-origin");
    expect(res.headers.get("cache-control")).toBe("no-store");
  }
  for (const p of ["/withdraw", "/_p/withdraw"]) {
    const res = await get(`http://checkout.fresh.localhost:8280${p}`);
    expect(res.headers.get("referrer-policy"), p).toBe("no-referrer");
    expect(res.headers.get("cache-control"), p).toBe("no-store");
  }
});

describe("failure paths keep the platform headers and carry no token (C1, C4)", () => {
  test("a worker exception's 500 keeps them", async () => {
    const res = await get(`${FRESH}/checkout?throw=1`, { ...NAVIGATE, cookie: COOKIE });
    expect(res.status).toBe(500);
    expectPlatformHeaders(res, "500");
    expect(await bodyAndHeaders(res)).not.toContain(TOKEN);
  });

  test("a render the edge cannot complete is a 502", async () => {
    const res = await get(`${FRESH}/checkout?huge=1`, { ...NAVIGATE, cookie: COOKIE });
    expect(res.status).toBe(502);
    expectPlatformHeaders(res, "502");
    expect(await bodyAndHeaders(res)).not.toContain(TOKEN);
  });

  test.each([
    ["the page-token secret", { pageTokenSecret: undefined }],
    ["a checkout artifact", { checkoutArtifact: undefined }],
  ])("without %s the page is a 503 and nothing renders", async (_, missing) => {
    const gateway = createGateway({ ...options, ...missing });
    try {
      const res = await get(`${FRESH}/checkout`, { ...NAVIGATE, cookie: COOKIE }, {}, gateway);
      expect(res.status).toBe(503);
      expectPlatformHeaders(res, "503");
      expect(await bodyAndHeaders(res)).not.toContain(TOKEN);
      expect(api.calls).toEqual([]);
    } finally {
      await gateway.dispose();
    }
  });

  test("a render past the deadline is a 504", async () => {
    const res = await get(`${FRESH}/checkout?slow=1`, { ...NAVIGATE, cookie: COOKIE });
    expect(res.status).toBe(504);
    expectPlatformHeaders(res, "504");
    expect(await bodyAndHeaders(res)).not.toContain(TOKEN);
  });
});
