import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { fakeApi, site } from "./fixtures.ts";

const FRESH = "http://fresh.localhost:8280";
const DEMO = "http://demo.localhost:8280";
const SHOP_TOKEN = "carttoken_00000000000000000001";
const PRESENTED = "sameorigintoken_00000000000000000099";
const CHECKOUT_API = "http://api.test/storefront/v1/cart/checkout";
const HANDOFF_API = "http://api.test/storefront/v1/cart/handoff";

let gw: Gateway;
let api: ReturnType<typeof fakeApi>;
let checkoutAnswer: (() => Response) | undefined;

const navigation = (origin = FRESH): Record<string, string> => ({
  origin,
  "sec-fetch-site": "same-origin",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "content-type": "application/x-www-form-urlencoded",
  cookie: `cart=${SHOP_TOKEN}`,
});

const withHeader = (name: string, value: string) => {
  const headers = navigation();
  if (value === "missing") delete headers[name];
  else headers[name] = value;
  return headers;
};

const start = (
  headers: Record<string, string>,
  init: RequestInit = {},
  base = FRESH,
): Promise<Response> =>
  gw.fetch(
    new Request(`${base}/_p/checkout/start`, {
      method: "POST",
      body: "",
      ...init,
      headers: { host: new URL(base).host, ...headers },
    }),
  );

const callsTo = (url: string) => api.calls.filter((c) => c.url === url);

async function expectRefused(res: Response, status: number) {
  expect(res.status).toBe(status);
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(res.headers.getSetCookie()).toEqual([]);
  if (status === 403) expect(await res.text()).toBe("");
  expect(api.calls).toEqual([]);
}

function expectBackToShop(res: Response) {
  expect(res.status).toBe(303);
  expect(res.headers.get("location")).toBe("/");
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(res.headers.getSetCookie()).toEqual([]);
}

async function expectStarted(res: Response) {
  expect(res.status).toBe(303);
  expect(res.headers.get("location")).toBe("/checkout");
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(await res.text()).toBe("");
  const cookies = res.headers.getSetCookie();
  expect(cookies).toHaveLength(1);
  expect(cookies[0]).toMatch(
    /^__Host-Http-cart=sameorigintoken_\d{20}; Path=\/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400$/,
  );
  return cookies[0]?.split(";")[0]?.split("=")[1];
}

beforeAll(async () => {
  api = fakeApi();
  gw = createGateway({
    artifactRoot: await mkdtemp(path.join(tmpdir(), "shop118-artifacts-")),
    resolver: new StaticResolver({
      "demo.localhost": site(),
      "fresh.localhost": site({
        tenant_id: "t-fresh",
        shop_host: "fresh.localhost",
        checkout_origins: [
          {
            origin: FRESH,
            mode: "same_origin",
            mode_generation: 1,
            checkout_origin: FRESH,
          },
        ],
      }),
    }),
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: async (req) => {
      const res = await api.fn(req);
      return checkoutAnswer && req.url === CHECKOUT_API ? checkoutAnswer() : res;
    },
  });
});

beforeEach(() => {
  api.calls.length = 0;
  checkoutAnswer = undefined;
});

afterAll(async () => {
  await gw?.dispose();
});

describe("same-origin checkout start (C10)", () => {
  test.each(["GET", "HEAD"])("%s is not allowed", async (method) => {
    const res = await start(navigation(), { method, body: null });
    await expectRefused(res, 405);
    expect(res.headers.get("allow")).toBe("POST");
  });

  test.each([
    "missing",
    "null",
    "http://evil.localhost:8280",
    "https://fresh.localhost:8280",
    "http://fresh.localhost:8281",
    "http://checkout.fresh.localhost:8280",
  ])("refuses Origin %s", async (origin) => {
    await expectRefused(await start(withHeader("origin", origin)), 403);
  });

  test.each(["missing", "same-site", "cross-site", "none"])(
    "refuses Sec-Fetch-Site %s",
    async (value) => {
      await expectRefused(await start(withHeader("sec-fetch-site", value)), 403);
    },
  );

  test.each(["missing", "cors", "no-cors", "same-origin"])(
    "refuses Sec-Fetch-Mode %s",
    async (value) => {
      await expectRefused(await start(withHeader("sec-fetch-mode", value)), 403);
    },
  );

  test.each(["missing", "empty", "iframe", "frame", "embed", "object"])(
    "refuses Sec-Fetch-Dest %s",
    async (value) => {
      await expectRefused(await start(withHeader("sec-fetch-dest", value)), 403);
    },
  );

  test.each(["prefetch", "prefetch;prerender"])("refuses Sec-Purpose %s", async (value) => {
    await expectRefused(await start(withHeader("sec-purpose", value)), 403);
  });

  test.each(["missing", "application/json", "text/plain", "multipart/form-data; boundary=x"])(
    "refuses content type %s",
    async (value) => {
      await expectRefused(await start(withHeader("content-type", value)), 415);
    },
  );

  test.each([
    "application/x-www-form-urlencoded;charset=UTF-8",
    "APPLICATION/X-WWW-FORM-URLENCODED",
  ])("accepts content type %s", async (value) => {
    await expectStarted(await start(withHeader("content-type", value)));
  });

  test("refuses a form body over 16 KiB", async () => {
    await expectRefused(await start(navigation(), { body: "x".repeat(16 * 1024 + 1) }), 413);
  });

  test("accepts a form body of exactly 16 KiB", async () => {
    await expectStarted(await start(navigation(), { body: "x".repeat(16 * 1024) }));
  });

  test.each(["missing", "cart=short", "cart=not a token at all!"])(
    "without a valid shop cart (%s) it goes back to the shop",
    async (cookie) => {
      const res = await start(withHeader("cookie", cookie));
      expectBackToShop(res);
      expect(api.calls).toEqual([]);
    },
  );

  test.each([
    ["404", () => Response.json({ code: "not_found" }, { status: 404 })],
    ["409 cart_empty", () => Response.json({ code: "cart_empty" }, { status: 409 })],
    [
      "409 checkout_mode_changed",
      () => Response.json({ code: "checkout_mode_changed" }, { status: 409 }),
    ],
    ["500", () => new Response("boom", { status: 500 })],
    ["a non-JSON body", () => new Response("not json", { status: 200 })],
    ["a malformed cart_token", () => Response.json({ cart_token: "short" })],
  ])("an API answer of %s goes back to the shop", async (_, answer) => {
    checkoutAnswer = answer;
    const res = await start(navigation());
    expectBackToShop(res);
    expect(await res.text()).toBe("");
    expect(callsTo(CHECKOUT_API)).toHaveLength(1);
  });

  test("starts the checkout from the shop cart and supersedes the presented one", async () => {
    const res = await start({
      ...navigation(),
      cookie: `cart=${SHOP_TOKEN}; __Host-Http-cart=${PRESENTED}`,
    });
    const token = await expectStarted(res);
    expect(res.headers.get("location")).not.toContain(token);
    const [call] = callsTo(CHECKOUT_API);
    expect(call?.method).toBe("POST");
    expect(call?.headers["x-cart-token"]).toBe(SHOP_TOKEN);
    expect(call?.headers["x-shop-origin"]).toBe(FRESH);
    expect(call?.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(call?.body ?? "")).toEqual({ previous: PRESENTED });
    expect(callsTo(HANDOFF_API)).toEqual([]);
  });

  test("a malformed presented checkout cookie is not forwarded", async () => {
    await expectStarted(
      await start({ ...navigation(), cookie: `cart=${SHOP_TOKEN}; __Host-Http-cart=bad` }),
    );
    expect(JSON.parse(callsTo(CHECKOUT_API)[0]?.body ?? "")).toEqual({});
  });

  test("an isolated shop still hands the cart to the checkout host", async () => {
    const res = await start(navigation(DEMO), {}, DEMO);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toMatch(
      /^http:\/\/checkout\.demo\.localhost:8280\/start\?h=handofftoken_\d{20}$/,
    );
    expect(res.headers.getSetCookie()[0]).toMatch(/^cart=; Path=\/_p; .*Max-Age=0$/);
    expect(callsTo(HANDOFF_API)).toHaveLength(1);
    expect(callsTo(CHECKOUT_API)).toEqual([]);
  });
});
