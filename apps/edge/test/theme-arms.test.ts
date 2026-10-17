import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { ApiEnroller } from "../src/experiments.ts";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, hostileTheme, site } from "./fixtures.ts";

const EXP_ID = "11111111-1111-4111-8111-111111111111";
const subject = (c: string) => c.repeat(32);
const consented = (s: string) =>
  `__Secure-consent_id=${s}; consent=analytics; __Secure-consent_exp=1`;

describe("code variant arms (SHOP-100)", () => {
  const api = fakeApi();
  const arms = new Map<string, string>([
    [subject("a"), "next"],
    [subject("b"), "control"],
    [subject("c"), "b"],
    [subject("d"), "b"],
  ]);
  const enrolled: string[] = [];
  let control: string;
  let next: string;
  let gw: Gateway;

  const upstream = async (req: Request): Promise<Response> => {
    if (new URL(req.url).pathname !== "/internal/v1/experiments/enrol") return api.fn(req);
    const { subject: s } = (await req.json()) as { subject: string };
    enrolled.push(s);
    const arm = arms.get(s);
    return Response.json({
      assignment: arm ? { experiment_id: EXP_ID, version: 3, arm } : null,
    });
  };

  const get = (p: string, cookie?: string, host = "demo.localhost", extra = {}) =>
    gw.fetch(
      new Request(`http://${host}${p}`, {
        headers: { host, ...(cookie ? { cookie } : {}), ...extra },
      }),
    );
  const workerArm = async (p: string, cookie?: string, host?: string, extra = {}) => {
    const res = await get(p, cookie, host, extra);
    const { headers } = (await res.json()) as { headers: [string, string][] };
    return headers.find(([k]) => k === "x-platform-arm")?.[1] ?? null;
  };

  beforeAll(async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shop100-artifacts-"));
    control = (
      await buildArtifact(root, "theme", hostileTheme("v1"), {
        "_astro/app.v1.js": "control()",
        "favicon.svg": "<svg>control</svg>",
      })
    ).id;
    next = (
      await buildArtifact(root, "theme", hostileTheme("v2"), {
        "_astro/app.v2.js": "next()",
        "favicon.svg": "<svg>next</svg>",
      })
    ).id;
    const experiment = (name: string, theme_arm: { arm: string; artifact: string } | null) => ({
      id: EXP_ID,
      version: 3,
      name,
      theme_arm,
    });
    gw = createGateway({
      artifactRoot: root,
      resolver: new StaticResolver({
        "demo.localhost": site({
          theme_artifact: control,
          experiment: experiment("new-theme", { arm: "next", artifact: next }),
        }),
        "code.localhost": site({
          tenant_id: "t-code",
          theme_artifact: control,
          experiment: experiment("buy-panel", null),
        }),
      }),
      apiOrigin: "http://api.test",
      mediaOrigin: "http://media.test",
      scheme: "http",
      purgeToken: "purge-token-0123456789abcdef",
      upstream,
      log: () => {},
      experiments: new ApiEnroller("http://api.test", "service-token", upstream),
    });
  });

  afterAll(async () => {
    await gw?.dispose();
  });

  beforeEach(() => {
    enrolled.length = 0;
  });

  test("a theme arm's visitors get its artifact; everyone else gets the active one", async () => {
    const pages = await Promise.all(
      [consented(subject("a")), consented(subject("b")), undefined].map((c) => get("/", c)),
    );
    const bodies = await Promise.all(pages.map((r) => r.text()));
    expect(bodies.map((b) => /app\.(v\d)\.js/.exec(b)?.[1])).toEqual(["v2", "v1", "v1"]);
    expect(pages.map((r) => r.headers.get("x-edge-cache"))).toEqual(["MISS", "MISS", "MISS"]);
    expect((await get("/", consented(subject("a")))).headers.get("x-edge-cache")).toBe("HIT");
  });

  test("the tenant runs at most two instances, both in its own scope", async () => {
    for (const c of [consented(subject("a")), consented(subject("b")), undefined])
      await get("/?pool", c);
    expect([gw.pool.has(control, "t-demo"), gw.pool.has(next, "t-demo")]).toEqual([true, true]);
    expect([gw.pool.has(control), gw.pool.has(next)]).toEqual([false, false]);
    expect(gw.pool.size).toBe(2);
  });

  test("the worker reads its own visitor's experiment and arm, never a client's", async () => {
    const forged = { "x-platform-arm": "buy-panel:b" };
    const code = "code.localhost";
    expect(await workerArm("/pages/headers?1", consented(subject("c")), code)).toBe("buy-panel:b");
    expect(await workerArm("/pages/headers?2", consented(subject("b")), code)).toBe(
      "buy-panel:control",
    );
    expect(await workerArm("/pages/headers?3", undefined, code, forged)).toBeNull();
    expect(await workerArm("/pages/headers?4", consented(subject("b")), code, forged)).toBe(
      "buy-panel:control",
    );
    expect(await workerArm("/pages/headers?5", consented(subject("a")))).toBe("new-theme:next");
  });

  test("both arms' hashed assets are served; public files come from the visitor's arm", async () => {
    const asset = async (p: string) => {
      const res = await get(p);
      return [res.status, await res.text(), res.headers.get("cache-control")];
    };
    const immutable = "public, max-age=31536000, immutable";
    expect(await asset("/_astro/app.v1.js")).toEqual([200, "control()", immutable]);
    expect(await asset("/_astro/app.v2.js")).toEqual([200, "next()", immutable]);
    const favicon = async (cookie?: string) => (await get("/favicon.svg", cookie)).text();
    expect(await favicon(consented(subject("a")))).toBe("<svg>next</svg>");
    expect(await favicon(consented(subject("b")))).toBe("<svg>control</svg>");
    expect(await favicon()).toBe("<svg>control</svg>");
  });

  test("no browser or shared cache reuses a public file across arms", async () => {
    const visitors = [consented(subject("a")), consented(subject("b")), undefined];
    const cacheControl = await Promise.all(
      visitors.map(async (c) => (await get("/favicon.svg", c)).headers.get("cache-control")),
    );
    expect(cacheControl).toEqual(Array(3).fill("private, no-cache"));
  });

  test("without a theme arm, public files never enrol (SHOP-96 is unchanged)", async () => {
    const res = await get("/favicon.svg", consented(subject("d")), "code.localhost");
    expect([await res.text(), res.headers.get("cache-control")]).toEqual([
      "<svg>control</svg>",
      "public, max-age=300",
    ]);
    expect(enrolled).toEqual([]);
    await get("/", consented(subject("d")), "code.localhost");
    expect(enrolled).toEqual([subject("d")]);
  });
});
