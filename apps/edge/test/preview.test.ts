/**
 * WP23 theme previews (A21) and local artifact GC: token exchange for a partitioned cookie,
 * the previewed artifact served uncached and `noindex`, framable by the admin only, no
 * checkout handoff, no counters; unknown/expired tokens refused.
 */
import { mkdtemp, stat, utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { Counters } from "../src/counters.ts";
import { createGateway, type Gateway, type GatewayOptions } from "../src/gateway.ts";
import { ApiPreviewResolver, StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, hostileTheme, site } from "./fixtures.ts";

const TOKEN = `${"a".repeat(32)}.1900000000.${"b".repeat(64)}`;
const OTHER = `${"c".repeat(32)}.1900000000.${"d".repeat(64)}`;
const HOST = "preview-7--demo.localhost";
let root: string;
let live: string;
let draft: string;
let gw: Gateway;
const api = fakeApi();
const resolveCalls: string[] = [];
const counters = new Counters();

/** The API's preview resolution: TOKEN is valid for revision 7 of demo, nothing else. */
const upstream = async (req: Request) => {
  const url = new URL(req.url);
  if (url.pathname === "/internal/v1/previews/resolve") {
    resolveCalls.push(
      `${url.searchParams.get("host")} ${url.searchParams.get("token")?.slice(0, 4)}`,
    );
    if (url.searchParams.get("host") !== HOST || url.searchParams.get("token") !== TOKEN)
      return Response.json({ code: "not_found" }, { status: 404 });
    return Response.json({
      site: {
        hostname: HOST,
        tenant_id: "t-demo",
        market_id: "m-cz",
        default_locale: "cs",
        locales: ["cs"],
        storefront_token: "sf_demo_public",
        theme_artifact: draft,
        retained_artifacts: [],
        checkout_artifact: null,
      },
      revision_id: "r",
      revision_number: 7,
      expires_at: new Date(Date.now() + 3_600_000).toISOString(),
    });
  }
  return api.fn(req);
};

const get = (url: string, headers: Record<string, string> = {}, init: RequestInit = {}) =>
  gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

beforeAll(async () => {
  root = await mkdtemp(path.join(tmpdir(), "wp23-preview-"));
  live = (await buildArtifact(root, "theme", hostileTheme("live"), { "_astro/app.live.js": "1" }))
    .id;
  draft = (
    await buildArtifact(root, "theme", hostileTheme("draft"), { "_astro/app.draft.js": "2" })
  ).id;
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({ "demo.localhost": site({ theme_artifact: live }) }),
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream,
    previews: new ApiPreviewResolver("http://api.test", "edge-token", upstream),
    adminOrigin: "http://admin.localhost:8080",
    counters,
    log: () => {},
  });
});

afterAll(async () => {
  await gw?.dispose();
});

describe("previews (A21)", () => {
  test("the link's token becomes a partitioned host-only cookie, then a clean URL", async () => {
    const res = await get(`http://${HOST}:8080/c/x?preview_token=${TOKEN}&sort=price`);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/c/x?sort=price");
    const cookie = res.headers.get("set-cookie") ?? "";
    expect(cookie).toMatch(
      /^__Host-preview=a{32}\.1900000000\.b{64}; Path=\/; HttpOnly; Secure; SameSite=None; Partitioned; Max-Age=3[0-9]{3}$/,
    );
    expect(res.headers.get("referrer-policy")).toBe("no-referrer");
  });

  test("the previewed revision renders uncached, noindex, framable by the admin only", async () => {
    const headers = { cookie: `__Host-preview=${TOKEN}` };
    for (let i = 0; i < 2; i++) {
      const res = await get(`http://${HOST}:8080/`, headers);
      expect(res.status).toBe(200);
      expect(await res.text()).toContain("app.draft.js");
      expect(res.headers.get("cache-control")).toBe("no-store");
      expect(res.headers.get("x-edge-cache")).toBe("BYPASS");
      expect(res.headers.get("x-robots-tag")).toBe("noindex, nofollow");
      expect(res.headers.get("content-security-policy")).toContain(
        "frame-ancestors http://admin.localhost:8080",
      );
    }
    // The live shop is untouched and still unframable.
    const shop = await get("http://demo.localhost:8080/");
    expect(await shop.text()).toContain("app.live.js");
    expect(shop.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
    // Assets of the draft artifact only, never cached.
    const asset = await get(`http://${HOST}:8080/_astro/app.draft.js`, headers);
    expect(asset.status).toBe(200);
    expect(asset.headers.get("cache-control")).toBe("no-store");
    expect((await get(`http://${HOST}:8080/_astro/app.live.js`, headers)).status).toBe(404);
    // Resolutions are cached (one API call for all of the above).
    expect(resolveCalls.filter((c) => c.startsWith(HOST) && c.endsWith("aaaa")).length).toBe(1);
  });

  test("no checkout handoff, events or counters from a preview", async () => {
    const headers = { cookie: `__Host-preview=${TOKEN}` };
    const res = await get(
      `http://${HOST}:8080/_p/checkout/start`,
      { ...headers, origin: `http://${HOST}:8080` },
      { method: "POST" },
    );
    expect(res.status).toBe(200);
    expect(await res.text()).toContain("Checkout is disabled in preview");
    expect(api.calls.some((c) => c.url.includes("/cart/handoff"))).toBe(false);
    const ev = await get(
      `http://${HOST}:8080/_p/e`,
      { ...headers, "content-type": "application/json" },
      { method: "POST", body: "[]" },
    );
    expect(ev.status).toBe(204);
    expect(api.calls.some((c) => c.url.includes("/events"))).toBe(false);
    // Only the one live shop render above was counted (A20 counters), no preview render.
    const sent: string[] = [];
    await counters.flush("http://api.test", "t", async (r) => {
      sent.push(await r.text());
      return Response.json({ recorded: 1 });
    });
    const rows = sent.flatMap(
      (b) => (JSON.parse(b) as { counters: { requests: number }[] }).counters,
    );
    expect(rows.reduce((n, r) => n + r.requests, 0)).toBe(1);
  });

  test("missing, forged, other-revision and expired tokens are refused", async () => {
    const variants: Record<string, string>[] = [
      {},
      { cookie: "__Host-preview=nope" },
      { cookie: `__Host-preview=${OTHER}` },
    ];
    for (const headers of variants) {
      const res = await get(`http://${HOST}:8080/`, headers);
      expect(res.status).toBe(401);
      expect(res.headers.get("x-robots-tag")).toBe("noindex, nofollow");
    }
    // The same token on another revision's host.
    expect(
      (await get("http://preview-8--demo.localhost:8080/", { cookie: `__Host-preview=${TOKEN}` }))
        .status,
    ).toBe(401);
    const link = await get(`http://${HOST}:8080/?preview_token=${OTHER}`);
    expect(link.status).toBe(401);
    expect(link.headers.get("set-cookie")).toBeNull();
    expect((await get("http://preview-x--demo.localhost:8080/")).status).toBe(404);
  });
});

test("local artifact GC removes long-unused artifacts only", async () => {
  const old = Date.now() / 1000 - 30 * 86_400;
  const extra = (
    await buildArtifact(root, "theme", hostileTheme("old"), { "_astro/app.old.js": "3" })
  ).id;
  await utimes(path.join(root, extra), old, old);
  const removed = await gw.pruneArtifacts(7 * 86_400_000);
  expect(removed).toEqual([extra]);
  await expect(stat(path.join(root, extra))).rejects.toThrow();
  // Recently used ones stay (the draft and live artifacts rendered above).
  await stat(path.join(root, draft, "manifest.json"));
  await stat(path.join(root, live, "manifest.json"));
});

test("pruning never removes an artifact a request is using, and drops its cached manifest", async () => {
  const old = Date.now() / 1000 - 30 * 86_400;
  const id = (
    await buildArtifact(root, "theme", hostileTheme("racy"), { "_astro/app.racy.js": "4" })
  ).id;
  await utimes(path.join(root, id), old, old);
  const downloads: string[] = [];
  const racy = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "racy.localhost": site({ theme_artifact: id, shop_host: "racy.localhost" }),
    }),
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream,
    artifacts: {
      ensure: async (a: string) => {
        downloads.push(a);
      },
    } as unknown as GatewayOptions["artifacts"],
    log: () => {},
  });
  const asset = () =>
    racy.fetch(
      new Request("http://racy.localhost:8080/_astro/app.racy.js", {
        headers: { host: "racy.localhost:8080" },
      }),
    );
  try {
    // A request racing the prune marks the artifact as used before the (synchronous)
    // eligibility check: it is kept and served.
    const [pruned, res] = await Promise.all([racy.pruneArtifacts(7 * 86_400_000), asset()]);
    expect(pruned).toEqual([]);
    expect(res.status).toBe(200);
    // Idle long enough: removed, and the next request loads it afresh (download + verify).
    const later = Date.now() + 30 * 86_400_000;
    expect(await racy.pruneArtifacts(7 * 86_400_000, later)).toContain(id);
    expect((await asset()).status).toBe(404);
    expect(downloads).toEqual([id, id]);
  } finally {
    await racy.dispose();
  }
});
