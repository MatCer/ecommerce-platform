import { spawnSync } from "node:child_process";
import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { pageToken, verifyPageToken } from "../src/page-token.ts";
import { StaticResolver } from "../src/sites.ts";
import { fakeApi, site } from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const CAP = "sameorigintoken_00000000000000000042";
const OTHER_CAP = "sameorigintoken_00000000000000000043";
const reference = (secret: string, host: string, capability: string) =>
  createHmac("sha256", secret).update(`pt1|${host}|${capability}`).digest("base64url");

describe("page token (C4)", () => {
  test("is base64url(HMAC-SHA256(secret, pt1|host|capability)), 43 characters", () => {
    const token = pageToken(SECRET, "https://shop.test:8443", CAP);
    expect(token).toBe(reference(SECRET, "shop.test:8443", CAP));
    expect(token).toMatch(/^[A-Za-z0-9_-]{43}$/);
  });

  test("the host is the canonical authority: :443 is no port, :8443 is another host", () => {
    expect(pageToken(SECRET, "https://shop.test:443", CAP)).toBe(
      reference(SECRET, "shop.test", CAP),
    );
    expect(pageToken(SECRET, "https://shop.test", CAP)).toBe(reference(SECRET, "shop.test", CAP));
    expect(pageToken(SECRET, "https://shop.test:8443", CAP)).not.toBe(
      reference(SECRET, "shop.test", CAP),
    );
  });

  test("no capability or no secret gives no token", () => {
    expect(pageToken(SECRET, "https://shop.test", undefined)).toBeUndefined();
    expect(pageToken(SECRET, "https://shop.test", "")).toBeUndefined();
    expect(pageToken(undefined, "https://shop.test", CAP)).toBeUndefined();
    expect(pageToken("", "https://shop.test", CAP)).toBeUndefined();
  });

  test("verify accepts the right token and rejects anything else without throwing", () => {
    const origin = "https://shop.test:8443";
    const good = reference(SECRET, "shop.test:8443", CAP);
    expect(verifyPageToken(SECRET, origin, CAP, good)).toBe(true);
    const rejected: [string, string | undefined, string, string | undefined, string | null][] = [
      ["wrong capability", SECRET, origin, `${CAP}x`, good],
      ["another host", SECRET, "https://other.test:8443", CAP, good],
      ["another port", SECRET, "https://shop.test", CAP, good],
      ["another secret", "another-page-token-secret-0123456789", origin, CAP, good],
      ["no secret", undefined, origin, CAP, good],
      ["no capability", SECRET, origin, undefined, reference(SECRET, "shop.test:8443", "")],
      ["truncated", SECRET, origin, CAP, good.slice(0, -1)],
      ["extended", SECRET, origin, CAP, `${good}A`],
      ["empty", SECRET, origin, CAP, ""],
      ["null", SECRET, origin, CAP, null],
      ["non-ASCII, same byte length", SECRET, origin, CAP, `${good.slice(0, -2)}é`],
      ["non-ASCII, same character length", SECRET, origin, CAP, `${good.slice(0, -1)}é`],
    ];
    for (const [label, secret, at, capability, presented] of rejected)
      expect(verifyPageToken(secret, at, capability, presented), label).toBe(false);
  });

  test("the edge does not start without a page-token secret of at least 32 characters", () => {
    const server = fileURLToPath(new URL("../src/server.ts", import.meta.url));
    const start = (secret: string) =>
      spawnSync(process.execPath, [server], {
        encoding: "utf8",
        timeout: 5000,
        env: {
          ...process.env,
          APP_ENV: "dev",
          E2E_RATE_SECRET: "",
          ARTIFACT_ROOT: "/tmp/unused-artifacts",
          API_ORIGIN: "http://127.0.0.1:1",
          INTERNAL_API_TOKEN: "test-token",
          MEDIA_ORIGIN: "",
          EDGE_PAGE_TOKEN_SECRET: secret,
        },
      });
    const missing = start("");
    expect(missing.status).not.toBe(0);
    expect(missing.stderr).toContain("missing env EDGE_PAGE_TOKEN_SECRET");
    const short = start("x".repeat(31));
    expect(short.status).not.toBe(0);
    expect(short.stderr).toContain("EDGE_PAGE_TOKEN_SECRET must be at least 32 characters");
  });
});

const FRESH = "http://fresh.localhost:8280";
const TOKEN = reference(SECRET, "fresh.localhost:8280", CAP);
const COOKIE = `__Host-Http-cart=${CAP}`;
const WRITE = { origin: FRESH, "content-type": "application/json" };

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;
let noSecret: Gateway;

const call = (
  gateway: Gateway,
  route: string,
  headers: Record<string, string>,
  init: RequestInit = {},
) =>
  gateway.fetch(
    new Request(`${FRESH}${route}`, {
      ...init,
      headers: { host: "fresh.localhost:8280", ...headers },
    }),
  );

beforeAll(async () => {
  api = fakeApi();
  const options = {
    artifactRoot: await mkdtemp(path.join(tmpdir(), "shop119-token-")),
    resolver: new StaticResolver({
      "fresh.localhost": site({
        tenant_id: "t-fresh",
        shop_host: "fresh.localhost",
        checkout_origins: [
          { origin: FRESH, mode: "same_origin", mode_generation: 1, checkout_origin: FRESH },
        ],
      }),
    }),
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http" as const,
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
  };
  gw = createGateway({ ...options, pageTokenSecret: SECRET });
  noSecret = createGateway(options);
});

beforeEach(() => {
  api.calls.length = 0;
});

afterAll(async () => {
  await gw?.dispose();
  await noSecret?.dispose();
});

describe("the same-origin checkout API takes the page token of the request's own capability (C4)", () => {
  const refusals: [string, () => Gateway, Record<string, string>][] = [
    ["no cookie", () => gw, { "x-page-token": TOKEN }],
    [
      "a malformed cookie",
      () => gw,
      {
        cookie: "__Host-Http-cart=bad",
        "x-page-token": reference(SECRET, "fresh.localhost:8280", "bad"),
      },
    ],
    [
      "the isolated checkout cookie",
      () => gw,
      { cookie: `__Host-cart=${CAP}`, "x-page-token": TOKEN },
    ],
    ["no token", () => gw, { cookie: COOKIE }],
    [
      "a token for another capability",
      () => gw,
      { cookie: COOKIE, "x-page-token": reference(SECRET, "fresh.localhost:8280", OTHER_CAP) },
    ],
    [
      "a token for another host",
      () => gw,
      { cookie: COOKIE, "x-page-token": reference(SECRET, "other.localhost:8280", CAP) },
    ],
    ["a gateway without a secret", () => noSecret, { cookie: COOKIE, "x-page-token": TOKEN }],
  ];

  test.each(refusals)("%s: 403 with no body and no API call", async (_, gateway, headers) => {
    const reads = await call(gateway(), "/_p/checkout", headers);
    const writes = await call(
      gateway(),
      "/_p/checkout/shipping",
      { ...headers, origin: FRESH, "content-type": "text/plain" },
      { method: "PUT", body: "x".repeat(17 * 1024) },
    );
    for (const res of [reads, writes]) {
      expect(res.status, "refused before the body is read").toBe(403);
      expect(await res.text()).toBe("");
      expect(res.headers.get("cache-control")).toBe("no-store");
      expect(res.headers.getSetCookie()).toEqual([]);
    }
    expect(api.calls).toEqual([]);
  });

  test("the right token is proxied with that capability and the shop origin only", async () => {
    const res = await call(gw, "/_p/checkout", {
      cookie: `${COOKIE}; __Host-cart=${OTHER_CAP}; __Host-sid=sessiontoken_000000000001`,
      "x-page-token": TOKEN,
    });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ path: "/checkout", cart_seen: CAP });
    const [read] = api.calls;
    expect(read?.method).toBe("GET");
    expect(read?.url).toBe("http://api.test/storefront/v1/checkout");
    expect(read?.headers["x-cart-token"]).toBe(CAP);
    expect(read?.headers["x-shop-origin"]).toBe(FRESH);
    expect(read?.headers["x-customer-session"]).toBeUndefined();

    for (const op of ["shipping", "payment"]) {
      const body = JSON.stringify({ op });
      const put = await call(
        gw,
        `/_p/checkout/${op}`,
        { ...WRITE, cookie: COOKIE, "x-page-token": TOKEN },
        { method: "PUT", body },
      );
      expect(put.status, op).toBe(200);
      expect(api.calls.at(-1)).toMatchObject({
        method: "PUT",
        url: `http://api.test/storefront/v1/checkout/${op}`,
        body,
        headers: { "x-cart-token": CAP, "x-shop-origin": FRESH },
      });
    }
    expect(JSON.stringify(api.calls), "the API never receives the token").not.toContain(TOKEN);
  });

  test("a write with a valid token still needs the shop's own Origin", async () => {
    const res = await call(
      gw,
      "/_p/checkout/shipping",
      { ...WRITE, origin: "http://evil.localhost", cookie: COOKIE, "x-page-token": TOKEN },
      { method: "PUT", body: "{}" },
    );
    expect(res.status).toBe(403);
    expect(await res.json()).toMatchObject({ code: "cross_origin" });
    expect(api.calls).toEqual([]);
  });

  test.each([
    ["PUT", "/contact"],
    ["PUT", "/addresses"],
    ["POST", "/place-order"],
    ["GET", "/order"],
  ])("%s %s is not offered on the shop, even with a valid token", async (method, op) => {
    const res = await call(
      gw,
      `/_p/checkout${op}`,
      { ...WRITE, cookie: COOKIE, "x-page-token": TOKEN },
      { method, ...(method === "GET" ? {} : { body: "{}" }) },
    );
    expect(res.status).toBe(404);
    expect(await res.json()).toMatchObject({ code: "not_found" });
    expect(api.calls).toEqual([]);
  });
});
