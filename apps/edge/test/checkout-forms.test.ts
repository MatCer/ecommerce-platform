import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, platformCheckoutWorker, site } from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const FRESH = "http://fresh.localhost:8280";
const DEMO = "http://demo.localhost:8280";
const CAP = "sameorigintoken_00000000000000000042";
const NEW_CAP = "sameorigintoken_00000000000000000077";
const DETAILS_API = "http://api.test/storefront/v1/checkout/details";
const pageTokenOf = (host: string, capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|${host}|${capability}`).digest("base64url");
const TOKEN = pageTokenOf("fresh.localhost:8280", CAP);
const ROTATED_COOKIE = `__Host-Http-cart=${NEW_CAP}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400`;
const CONTACT = `_pt=${TOKEN}&email=jana%40example.cz&phone=&evil=1`;

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;
let noSecret: Gateway;
let answers: Record<string, () => Response>;

const navigation = (over: Record<string, string> = {}): Record<string, string> => {
  const headers: Record<string, string> = {
    origin: FRESH,
    "sec-fetch-site": "same-origin",
    "sec-fetch-mode": "navigate",
    "sec-fetch-dest": "document",
    "content-type": "application/x-www-form-urlencoded",
    cookie: `__Host-Http-cart=${CAP}`,
    ...over,
  };
  for (const [name, value] of Object.entries(headers))
    if (value === "missing") delete headers[name];
  return headers;
};

const post = (
  route: string,
  body: string | null = CONTACT,
  headers = navigation(),
  { method = "POST", base = FRESH, gateway = gw } = {},
) =>
  gateway.fetch(
    new Request(`${base}${route}`, {
      method,
      body,
      headers: { host: new URL(base).host, ...headers },
    }),
  );

const detailCalls = () => api.calls.filter((c) => c.url === DETAILS_API);

function expectPlatformHeaders(res: Response, label: string) {
  expect(res.headers.get("cache-control"), label).toBe("no-store");
  expect(res.headers.get("referrer-policy"), label).toBe("strict-origin");
  expect(res.headers.get("x-frame-options"), label).toBe("DENY");
  expect(res.headers.get("cross-origin-opener-policy"), label).toBe("same-origin");
  expect(res.headers.get("x-content-type-options"), label).toBe("nosniff");
  expect(res.headers.get("content-security-policy") ?? "", label).toContain(
    "frame-ancestors 'none'",
  );
}

async function expectRefused(res: Response, status: number) {
  expect(res.status).toBe(status);
  expect(res.headers.getSetCookie(), "no cookie on a refusal").toEqual([]);
  if (status === 403) expect(await res.text()).toBe("");
  expect(api.calls, "no API call on a refusal").toEqual([]);
}

async function expectStale(res: Response) {
  expect(res.status).toBe(403);
  expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
  const html = await res.text();
  expect(html, "one link back to the checkout").toContain('href="/checkout"');
  for (const secret of [TOKEN, CAP, NEW_CAP]) expect(html).not.toContain(secret);
  expect(html, "reloads itself by navigation").toContain(
    '<meta http-equiv="refresh" content="0;url=/checkout?error=stale_page">',
  );
  expect(res.headers.getSetCookie(), "no cookie on a stale page").toEqual([]);
  expect(api.calls, "no API call on a stale page").toEqual([]);
  expectPlatformHeaders(res, "stale page");
}

async function expectRotated(res: Response) {
  expect(res.status).toBe(303);
  expect(res.headers.get("location")).toBe("/checkout");
  expect(await res.text()).toBe("");
  expect(res.headers.getSetCookie(), "exactly one HttpOnly capability cookie").toEqual([
    ROTATED_COOKIE,
  ]);
  expectPlatformHeaders(res, "success");
}

beforeAll(async () => {
  api = fakeApi();
  const artifactRoot = await mkdtemp(path.join(tmpdir(), "shop120-forms-"));
  const checkout = await buildArtifact(artifactRoot, "checkout", platformCheckoutWorker, {
    "_checkout/0123456789abcdef/app.js": "",
  });
  const options = {
    artifactRoot,
    checkoutArtifact: checkout.id,
    resolver: new StaticResolver({
      "demo.localhost": site(),
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
    upstream: async (req: Request) => {
      const res = await api.fn(req);
      return answers[req.url]?.() ?? res;
    },
  };
  gw = createGateway({ ...options, pageTokenSecret: SECRET });
  noSecret = createGateway(options);
});

beforeEach(() => {
  api.calls.length = 0;
  answers = {
    [DETAILS_API]: () => new Response(null, { status: 204, headers: { "x-cart-token": NEW_CAP } }),
  };
});

afterAll(async () => {
  await gw?.dispose();
  await noSecret?.dispose();
});

describe("checkout detail forms rotate the capability (C4, C6)", () => {
  test("the contact form writes only its own fields and sets the new capability", async () => {
    await expectRotated(await post("/checkout/contact"));
    expect(api.calls, "one API call").toHaveLength(1);
    const [call] = detailCalls();
    expect(call?.method).toBe("POST");
    expect(call?.headers["x-cart-token"]).toBe(CAP);
    expect(call?.headers["x-shop-origin"]).toBe(FRESH);
    expect(call?.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(call?.body ?? ""), "_pt and unknown fields stay at the edge").toEqual({
      contact: { email: "jana@example.cz", phone: "" },
    });
    expect(JSON.stringify(api.calls), "the API never receives the page token").not.toContain(TOKEN);
  });

  const billing = {
    name: "Jana Nováková",
    company: "",
    street: "Dlouhá 1",
    city: "Praha",
    postal_code: "11000",
    country: "CZ",
    phone: "+420601234567",
  };
  const shipping = {
    name: "Petr Novák",
    company: "Firma s.r.o.",
    street: "Krátká 2",
    city: "Brno",
    postal_code: "60200",
    country: "CZ",
    phone: "",
  };
  const fields = (prefix: string, address: Record<string, string>) =>
    Object.entries(address).map(([k, v]) => [`${prefix}_${k}`, v]);

  test.each([
    ["absent", [], null],
    ["not exactly 1", [["ship_elsewhere", "on"]], null],
    ["1", [["ship_elsewhere", "1"]], shipping],
  ])("the addresses form with ship_elsewhere %s", async (_, flag, expected) => {
    const body = new URLSearchParams([
      ["_pt", TOKEN],
      ...fields("billing", billing),
      ...flag,
      ...fields("shipping", shipping),
      ["billing_vat_id", "CZ123"],
    ]).toString();
    await expectRotated(await post("/checkout/addresses", body));
    expect(api.calls, "one API call").toHaveLength(1);
    const [call] = detailCalls();
    expect(call?.headers["x-cart-token"]).toBe(CAP);
    expect(JSON.parse(call?.body ?? "")).toEqual({ addresses: { billing, shipping: expected } });
  });

  test("a missing field is sent empty for the API to judge", async () => {
    await expectRotated(await post("/checkout/addresses", `_pt=${TOKEN}&billing_name=Jana`));
    expect(JSON.parse(detailCalls()[0]?.body ?? "")).toEqual({
      addresses: {
        billing: {
          name: "Jana",
          company: "",
          street: "",
          city: "",
          postal_code: "",
          country: "",
          phone: "",
        },
        shipping: null,
      },
    });
  });
});

describe("a detail form is a top-level form POST from the shop's own page", () => {
  test.each([
    ["GET", null],
    ["HEAD", null],
    ["PUT", CONTACT],
  ])("%s is not allowed", async (method, body) => {
    const res = await post("/checkout/contact", body, navigation(), { method });
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
    const res = await post("/checkout/contact", CONTACT, navigation({ origin }));
    await expectRefused(res, 403);
    expectPlatformHeaders(res, "refusal");
  });

  test.each([
    ["sec-fetch-site", "missing"],
    ["sec-fetch-site", "same-site"],
    ["sec-fetch-site", "cross-site"],
    ["sec-fetch-site", "none"],
    ["sec-fetch-mode", "cors"],
    ["sec-fetch-mode", "no-cors"],
    ["sec-fetch-mode", "same-origin"],
    ["sec-fetch-dest", "missing"],
    ["sec-fetch-dest", "empty"],
    ["sec-fetch-dest", "iframe"],
    ["sec-fetch-dest", "frame"],
    ["sec-fetch-dest", "embed"],
    ["sec-fetch-dest", "object"],
    ["sec-purpose", "prefetch"],
    ["sec-purpose", "prefetch;prerender"],
  ])("refuses %s %s", async (name, value) => {
    await expectRefused(
      await post("/checkout/addresses", CONTACT, navigation({ [name]: value })),
      403,
    );
  });

  test("a browser without Sec-Fetch-Mode gets the unsupported-browser page", async () => {
    const res = await post(
      "/checkout/contact",
      CONTACT,
      navigation({ "sec-fetch-mode": "missing" }),
    );
    await expectRefused(res, 400);
    expect(await res.text()).toContain("Nepodporovaný prohlížeč");
  });

  test.each(["missing", "application/json", "text/plain", "multipart/form-data; boundary=x"])(
    "refuses content type %s",
    async (value) => {
      const res = await post("/checkout/contact", CONTACT, navigation({ "content-type": value }));
      await expectRefused(res, 415);
      expect(await res.json()).toMatchObject({ code: "unsupported_media_type" });
    },
  );

  test.each([
    "application/x-www-form-urlencoded;charset=UTF-8",
    "APPLICATION/X-WWW-FORM-URLENCODED",
  ])("accepts content type %s", async (value) => {
    await expectRotated(
      await post("/checkout/contact", CONTACT, navigation({ "content-type": value })),
    );
  });

  const padded = (bytes: number) => `${CONTACT}&pad=${"x".repeat(bytes - CONTACT.length - 5)}`;

  test("refuses a form body over 16 KiB", async () => {
    const res = await post("/checkout/contact", padded(16 * 1024 + 1));
    await expectRefused(res, 413);
    expect(await res.json()).toMatchObject({ code: "payload_too_large" });
  });

  test("accepts a form body of exactly 16 KiB", async () => {
    expect(padded(16 * 1024)).toHaveLength(16 * 1024);
    await expectRotated(await post("/checkout/contact", padded(16 * 1024)));
  });
});

describe("a form whose page token does not match the request's capability is stale", () => {
  const email = "email=jana%40example.cz";
  const cookie = (value: string) => navigation({ cookie: value });
  const cases: [string, string, Record<string, string>, () => Gateway][] = [
    ["no _pt", email, navigation(), () => gw],
    ["an empty _pt", `_pt=&${email}`, navigation(), () => gw],
    ["a duplicate _pt", `_pt=${TOKEN}&_pt=${TOKEN}&${email}`, navigation(), () => gw],
    ["an oversized _pt", `_pt=${TOKEN}${"A".repeat(8000)}&${email}`, navigation(), () => gw],
    ["a truncated _pt", `_pt=${TOKEN.slice(0, -1)}&${email}`, navigation(), () => gw],
    [
      "the stale tab's _pt",
      `_pt=${TOKEN}&${email}`,
      cookie(`__Host-Http-cart=${NEW_CAP}`),
      () => gw,
    ],
    [
      "another host's _pt",
      `_pt=${pageTokenOf("other.localhost:8280", CAP)}&${email}`,
      navigation(),
      () => gw,
    ],
    ["no cookie", `_pt=${TOKEN}&${email}`, navigation({ cookie: "missing" }), () => gw],
    [
      "a malformed cookie",
      `_pt=${pageTokenOf("fresh.localhost:8280", "bad")}&${email}`,
      cookie("__Host-Http-cart=bad"),
      () => gw,
    ],
    ["the isolated cookie only", `_pt=${TOKEN}&${email}`, cookie(`__Host-cart=${CAP}`), () => gw],
    ["a duplicate field", `_pt=${TOKEN}&${email}&email=eva%40example.cz`, navigation(), () => gw],
    ["a gateway without a secret", `_pt=${TOKEN}&${email}`, navigation(), () => noSecret],
  ];

  test.each(cases)("%s: 403 stale page", async (_, body, headers, gateway) => {
    await expectStale(await post("/checkout/contact", body, headers, { gateway: gateway() }));
  });
});

const problem = (status: number, code: unknown) =>
  Response.json(
    { code, detail: "jana@example.cz" },
    { status, headers: { "x-cart-token": NEW_CAP } },
  );

describe("a draft the API refuses comes back as the checkout with what was typed (422)", () => {
  const contact = new URLSearchParams([
    ["_pt", TOKEN],
    ["email", " Eva@Example.test "],
    ["phone", "abc <b>"],
  ]).toString();
  const addresses = new URLSearchParams([
    ["_pt", TOKEN],
    ["billing_name", "Jana"],
    ["billing_street", "Dlouhá 1"],
    ["ship_elsewhere", "1"],
    ["shipping_street", ""],
  ]).toString();
  const seenByWorker = (html: string) =>
    JSON.parse(/<pre id="request">(.*?)<\/pre>/s.exec(html)?.[1] ?? "null") as {
      method: string;
      url: string;
      origin: string | null;
      type: string | null;
      form: [string, string][];
    } | null;

  test.each([
    ["contact", "/checkout/contact", contact, () => problem(422, "invalid_phone"), "invalid_phone"],
    [
      "addresses",
      "/checkout/addresses",
      addresses,
      () => problem(422, "invalid_address"),
      "invalid_address",
    ],
    ["contact", "/checkout/contact", contact, () => problem(422, "<b>"), "failed"],
    ["contact", "/checkout/contact", contact, () => problem(422, "a".repeat(41)), "failed"],
    [
      "addresses",
      "/checkout/addresses",
      addresses,
      () => new Response("nope", { status: 422 }),
      "failed",
    ],
  ])("%s: 422 at %s, rendered with the draft", async (step, route, body, answer, code) => {
    answers[DETAILS_API] = answer;
    const res = await post(route, body);
    expect(res.status).toBe(422);
    expect(res.headers.get("location"), "no redirect").toBeNull();
    expect(res.headers.getSetCookie(), "no cookie: nothing rotated").toEqual([]);
    expect(res.headers.get("x-cart-token")).toBeNull();
    expectPlatformHeaders(res, "422");
    const csp = res.headers.get("content-security-policy") ?? "";
    expect(csp, "the checkout CSP").toContain("connect-src 'self' https://api.stripe.com");
    expect(csp).toContain("form-action 'self'");
    const html = await res.text();
    expect(seenByWorker(html)).toEqual({
      method: "POST",
      url: `${FRESH}/checkout?edit=${step}&error=${code}`,
      origin: FRESH,
      type: "application/x-www-form-urlencoded",
      form: [...new URLSearchParams(body)],
    });
    expect(html, "the token of the request's own capability").toContain(
      `<meta name="page-token" content="${TOKEN}">`,
    );
    const read = api.calls.find((c) => c.url === "http://api.test/storefront/v1/checkout");
    expect(read?.headers["x-cart-token"]).toBe(CAP);
    expect(detailCalls()).toHaveLength(1);
  });

  test("a render that fails keeps the worker's status and the platform headers", async () => {
    answers[DETAILS_API] = () => problem(422, "invalid_email");
    const res = await post("/checkout/contact", `${CONTACT}&throw=1`);
    expect(res.status).toBe(500);
    expect(res.headers.get("location")).toBeNull();
    expectPlatformHeaders(res, "500");
    expect(await res.text()).not.toContain(TOKEN);
  });
});

describe("the API answer becomes a redirect back to the checkout, never forwarded", () => {
  const rotated =
    (headers: Record<string, string>, status = 204) =>
    () =>
      new Response(status === 204 ? null : "{}", { status, headers });

  test.each([
    ["404", "/checkout", () => problem(404, "not_found")],
    ["409", "/checkout", () => problem(409, "checkout_mode_changed")],
    ["500", "/checkout?error=failed", () => problem(500, "internal")],
    ["200 with a capability", "/checkout?error=failed", rotated({ "x-cart-token": NEW_CAP }, 200)],
    ["204 without a capability", "/checkout?error=failed", rotated({})],
    [
      "204 with a malformed capability",
      "/checkout?error=failed",
      rotated({ "x-cart-token": "short" }),
    ],
  ])("%s answers 303 %s without a cookie", async (_, location, answer) => {
    answers[DETAILS_API] = answer;
    const res = await post("/checkout/contact");
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(location);
    expect(await res.text()).toBe("");
    expect(res.headers.getSetCookie(), "no cookie unless the API rotated").toEqual([]);
    expect(res.headers.get("x-cart-token"), "upstream headers stay behind").toBeNull();
    expect(detailCalls()).toHaveLength(1);
    expectPlatformHeaders(res, location);
  });
});

describe("only the enumerated form paths reach the API", () => {
  test.each([
    "/checkout/shipping",
    "/checkout/payment",
    "/checkout/place-order",
    "/checkout/note",
    "/checkout/details",
    "/checkout/contact/",
    "/checkout/CONTACT",
    "/checkout/%63ontact",
    "/cs/checkout/contact",
    "/checkout/contact/x",
  ])("%s is 404 with no API call", async (route) => {
    const res = await post(route);
    expect(res.status).toBe(404);
    expect(res.headers.getSetCookie()).toEqual([]);
    expect(api.calls).toEqual([]);
  });

  test("an isolated shop has no detail forms", async () => {
    const res = await post(
      "/checkout/contact",
      `_pt=${pageTokenOf("demo.localhost:8280", CAP)}&email=jana%40example.cz`,
      navigation({ origin: DEMO }),
      { base: DEMO },
    );
    expect(res.status).toBe(404);
    expect(api.calls).toEqual([]);
  });
});

describe("the legacy checkout host never hands out an internal capability", () => {
  const PLACE_ORDER_API = "http://api.test/storefront/v1/checkout/place-order";

  test.each(["http://checkout.demo.localhost:8280", "http://checkout.fresh.localhost:8280"])(
    "%s strips x-cart-token from place-order and has no details route",
    async (base) => {
      answers[PLACE_ORDER_API] = () =>
        Response.json({ order_id: "o1" }, { status: 201, headers: { "x-cart-token": NEW_CAP } });
      const json = {
        origin: base,
        "content-type": "application/json",
        cookie: "__Host-cart=checkouttoken_000000000001",
      };
      const placed = await post("/_p/checkout/place-order", "{}", json, { base });
      expect(placed.status).toBe(201);
      expect(placed.headers.get("x-cart-token")).toBeNull();
      expect(placed.headers.getSetCookie()).toEqual([]);
      expect(await placed.text()).not.toContain(NEW_CAP);

      api.calls.length = 0;
      const details = await post("/_p/checkout/details", '{"contact":{}}', json, { base });
      expect(details.status).toBe(404);
      expect(api.calls).toEqual([]);
    },
  );
});
