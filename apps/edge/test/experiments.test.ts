import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test, vi } from "vitest";
import { ApiEnroller, type Enroller } from "../src/experiments.ts";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, hostileTheme, site } from "./fixtures.ts";

const EXP = { id: "11111111-1111-4111-8111-111111111111", version: 3 };
const ALICE = "a".repeat(32);

describe("ApiEnroller", () => {
  test("asks the API once per subject and experiment version for 60 s", async () => {
    const seen: string[] = [];
    let arm = "a";
    const enroller = new ApiEnroller("http://api.test", "service-token", async (r) => {
      seen.push(`${r.method} ${r.url} ${r.headers.get("authorization")} ${await r.text()}`);
      return Response.json({ assignment: { experiment_id: EXP.id, version: 3, arm } });
    });
    const s = site({ experiment: EXP });
    const want = { experiment_id: EXP.id, version: 3, arm: "a" };
    expect(await enroller.enrol(s, ALICE, 0)).toEqual(want);
    arm = "b";
    expect(await enroller.enrol(s, ALICE, 59_999)).toEqual(want);
    expect(seen).toEqual([
      `POST http://api.test/internal/v1/experiments/enrol Bearer service-token {"tenant_id":"t-demo","subject":"${ALICE}"}`,
    ]);
    expect(await enroller.enrol(site({ experiment: { ...EXP, version: 4 } }), ALICE, 1)).toEqual({
      ...want,
      arm: "b",
    });
    expect(await enroller.enrol(s, ALICE, 60_001)).toEqual({ ...want, arm: "b" });
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 60_002)).toEqual({ ...want, arm: "b" });
    expect(seen.length).toBe(4);
  });

  test("failures throw and are retried; malformed answers count as not enrolled", async () => {
    const answers: (() => Response)[] = [
      () => new Response("boom", { status: 500 }),
      () => Response.json({ assignment: { experiment_id: EXP.id, version: 3, arm: "A B" } }),
      () => Response.json({ assignment: { experiment_id: EXP.id, version: 3.5, arm: "a" } }),
      () => Response.json({ assignment: null }),
    ];
    let calls = 0;
    const enroller = new ApiEnroller("http://api.test", "t", async () => {
      const answer = answers[calls++];
      if (!answer) throw new Error("unexpected call");
      return answer();
    });
    const s = site({ experiment: EXP });
    await expect(enroller.enrol(s, ALICE, 0)).rejects.toThrow("enrol: 500");
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    expect(calls).toBe(4);
  });

  test("an answer still in flight when the subject is forgotten is not memoized", async () => {
    const pending: ((arm: string | null) => void)[] = [];
    const enroller = new ApiEnroller(
      "http://api.test",
      "t",
      () =>
        new Promise((resolve) =>
          pending.push((arm) =>
            resolve(
              Response.json({
                assignment: arm && { experiment_id: EXP.id, version: 3, arm },
              }),
            ),
          ),
        ),
    );
    const s = site({ experiment: EXP });
    const stale = enroller.enrol(s, ALICE, 0);
    enroller.forget("t-demo", ALICE);
    pending[0]?.(null);
    expect(await stale).toBeNull();
    const fresh = enroller.enrol(s, ALICE, 1);
    expect(pending.length).toBe(2);
    pending[1]?.("b");
    expect(await fresh).toEqual({ experiment_id: EXP.id, version: 3, arm: "b" });
  });

  test("gives up after 500 ms", async () => {
    const enroller = new ApiEnroller(
      "http://api.test",
      "t",
      (r) =>
        new Promise((_, reject) =>
          r.signal.addEventListener("abort", () => reject(r.signal.reason)),
        ),
    );
    const started = Date.now();
    await expect(enroller.enrol(site({ experiment: EXP }), ALICE)).rejects.toThrow();
    expect(Date.now() - started).toBeLessThan(2_000);
  });
});

describe("enrolment at the edge (SHOP-96)", () => {
  const ARM = (arm: string) => `${EXP.id}:3:${arm}`;
  const subject = (c: string) => c.repeat(32);
  const answers = new Map<string, string | null | Record<string, unknown>>();
  const enrolCalls: { authorization: string | null; tenant_id: string; subject: string }[] = [];
  const logs: Record<string, unknown>[] = [];
  let enrolInvocations = 0;
  let api: ReturnType<typeof fakeApi>;
  let gw: Gateway;

  const upstream = async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    if (url.pathname === "/internal/v1/experiments/enrol") {
      const body = (await req.json()) as { tenant_id: string; subject: string };
      enrolCalls.push({ authorization: req.headers.get("authorization"), ...body });
      const a = answers.get(body.subject) ?? null;
      if (a === "error") return new Response("boom", { status: 500 });
      if (a === "hang")
        return new Promise((_, reject) =>
          req.signal.addEventListener("abort", () => reject(req.signal.reason)),
        );
      return Response.json({
        assignment:
          typeof a === "string" ? { experiment_id: EXP.id, version: EXP.version, arm: a } : a,
      });
    }
    const res = await api.fn(req);
    return url.pathname === "/storefront/v1/pages/product/short"
      ? Response.json({ cache: { public: true, max_age: 1, tags: [] } })
      : res;
  };

  const page = (
    path: string,
    cookie?: string,
    host = "demo.localhost",
    headers: Record<string, string> = {},
  ) =>
    gw.fetch(
      new Request(`http://${host}${path}`, {
        headers: { host, ...(cookie ? { cookie } : {}), ...headers },
      }),
    );
  const consented = (s: string, purposes = "analytics%2Cpersonalization") =>
    `__Secure-consent_id=${s}; consent=${purposes}`;
  const experimentHeaders = (p: string) =>
    api.calls
      .filter((c) => c.url === `http://api.test/storefront/v1${p}`)
      .map((c) => c.headers["x-experiment"] ?? null);
  const cacheStates = (rs: Response[]) => rs.map((r) => r.headers.get("x-edge-cache"));
  const PUBLIC = "public, max-age=0, must-revalidate";
  const PRIVATE = "private, max-age=0, must-revalidate";

  beforeAll(async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shop96-artifacts-"));
    const theme = (
      await buildArtifact(root, "theme", hostileTheme("v1"), { "_astro/app.v1.js": "1" })
    ).id;
    api = fakeApi();
    const enroller = new ApiEnroller("http://api.test", "service-token", upstream);
    const experiments: Enroller = {
      enrol: (s, sub) => {
        enrolInvocations++;
        return enroller.enrol(s, sub);
      },
      forget: (t, sub) => enroller.forget(t, sub),
    };
    gw = createGateway({
      artifactRoot: root,
      resolver: new StaticResolver({
        "demo.localhost": site({ theme_artifact: theme, experiment: EXP }),
        "plain.localhost": site({ theme_artifact: theme }),
      }),
      apiOrigin: "http://api.test",
      mediaOrigin: "http://media.test",
      scheme: "http",
      purgeToken: "purge-token-0123456789abcdef",
      upstream,
      log: (e) => logs.push(e),
      experiments,
    });
  });

  afterAll(async () => {
    await gw?.dispose();
  });

  beforeEach(() => {
    api.calls.length = 0;
    enrolCalls.length = 0;
    logs.length = 0;
  });

  test("without consent the page is byte-identical to a shop without an experiment", async () => {
    const withExperiment = await page("/?case=a");
    const without = await page("/?case=a", undefined, "plain.localhost");
    expect(withExperiment.status).toBe(200);
    expect(withExperiment.headers.get("cache-control")).toBe(PUBLIC);
    expect([...withExperiment.headers]).toEqual([...without.headers]);
    expect(new Uint8Array(await withExperiment.arrayBuffer())).toEqual(
      new Uint8Array(await without.arrayBuffer()),
    );
    expect(enrolCalls).toEqual([]);
    expect(experimentHeaders("/shop")).toEqual([null, null]);
  });

  test("no analytics grant or no well-formed subject: nothing is asked", async () => {
    await page("/?case=b", consented(subject("b"), "personalization"));
    await page("/?case=b", consented(subject("b"), ""));
    await page("/?case=b", consented("not-a-subject"));
    await page("/?case=b", "consent=analytics");
    await page("/?case=b", consented(subject("b"), "analytics%ZZ"));
    expect(enrolCalls).toEqual([]);
  });

  test("an enrolled visitor keeps their arm: one enrol call, then a HIT of the arm's entry", async () => {
    answers.set(subject("c"), "a");
    const rs = [await page("/?case=c", consented(subject("c")))];
    rs.push(await page("/?case=c", consented(subject("c"))));
    expect(cacheStates(rs)).toEqual(["MISS", "HIT"]);
    expect(enrolCalls).toEqual([
      { authorization: "Bearer service-token", tenant_id: "t-demo", subject: subject("c") },
    ]);
    expect(experimentHeaders("/shop")).toEqual([ARM("a")]);
  });

  test("enrolled pages are private to the browser; no-store stays no-store", async () => {
    answers.set(subject("d"), "a");
    const miss = await page("/?case=k", consented(subject("d")));
    const hit = await page("/?case=k", consented(subject("d")));
    expect([miss.headers.get("cache-control"), hit.headers.get("cache-control")]).toEqual([
      PRIVATE,
      PRIVATE,
    ]);
    const noStore = await page("/p/theme-no-store", consented(subject("d")));
    expect(noStore.headers.get("cache-control")).toBe("no-store");
  });

  test("each arm and control have their own cache entry and page-model header", async () => {
    answers.set(subject("e"), "a");
    answers.set(subject("f"), "b");
    const visitors = [consented(subject("e")), consented(subject("f")), undefined];
    const rs: Response[] = [];
    for (const cookie of [...visitors, ...visitors]) rs.push(await page("/?case=d", cookie));
    expect(cacheStates(rs)).toEqual(["MISS", "MISS", "MISS", "HIT", "HIT", "HIT"]);
    expect(rs.map((r) => r.headers.get("cache-control"))).toEqual([
      PRIVATE,
      PRIVATE,
      PUBLIC,
      PRIVATE,
      PRIVATE,
      PUBLIC,
    ]);
    expect(experimentHeaders("/shop")).toEqual([ARM("a"), ARM("b"), null]);
  });

  test("a stale arm entry is refreshed with its own arm and without enrolling", async () => {
    answers.set(subject("0"), "b");
    expect(cacheStates([await page("/p/short", consented(subject("0")))])).toEqual(["MISS"]);
    await new Promise((r) => setTimeout(r, 1_100));
    api.calls.length = 0;
    enrolInvocations = 0;
    const stale = await page("/p/short", consented(subject("0")));
    expect(stale.headers.get("x-edge-cache")).toBe("STALE");
    await vi.waitFor(() => expect(experimentHeaders("/pages/product/short")).toEqual([ARM("b")]));
    expect(enrolInvocations).toBe(1);
  });

  test("a consent cookie without analytics stops enrolment at once", async () => {
    answers.set(subject("1"), "a");
    await page("/?case=f1", consented(subject("1")));
    const withdrawn = await page("/?case=f1", consented(subject("1"), "personalization"));
    expect(withdrawn.headers.get("cache-control")).toBe(PUBLIC);
    expect(withdrawn.headers.get("x-edge-cache")).toBe("MISS");
    expect(enrolCalls.length).toBe(1);
    expect(experimentHeaders("/shop")).toEqual([ARM("a"), null]);
  });

  test("a consent choice through the edge forgets the memoized arm", async () => {
    answers.set(subject("2"), "a");
    await page("/?case=f2", consented(subject("2")));
    const choice = await gw.fetch(
      new Request("http://demo.localhost/_p/consent", {
        method: "POST",
        headers: {
          host: "demo.localhost",
          origin: "http://demo.localhost",
          "content-type": "application/json",
          cookie: consented(subject("2")),
        },
        body: JSON.stringify({ purposes: { analytics: false }, text_version: "2026-10-16" }),
      }),
    );
    expect(choice.status).toBe(200);
    answers.set(subject("2"), null);
    const after = await page("/?case=f2", consented(subject("2")));
    expect(after.headers.get("cache-control")).toBe(PUBLIC);
    expect(enrolCalls.map((c) => c.subject)).toEqual([subject("2"), subject("2")]);
    expect(experimentHeaders("/shop")).toEqual([ARM("a"), null]);
  });

  test("an enroller error or timeout serves the control page and logs no subject", async () => {
    answers.set(subject("3"), "error");
    answers.set(subject("4"), "hang");
    const rs = [
      await page("/?case=g", consented(subject("3"))),
      await page("/?case=g", consented(subject("4"))),
    ];
    expect(rs.map((r) => [r.status, r.headers.get("cache-control")])).toEqual([
      [200, PUBLIC],
      [200, PUBLIC],
    ]);
    expect(experimentHeaders("/shop")).toEqual([null]);
    expect(logs.filter((l) => l.msg === "experiment enrolment failed").length).toBe(2);
    const logged = JSON.stringify(logs);
    expect(logged).not.toContain(subject("3"));
    expect(logged).not.toContain(subject("4"));
  });

  test("a new visitor is enrolled even when their arm's page is a cache HIT", async () => {
    answers.set(subject("5"), "a");
    answers.set(subject("6"), "a");
    const rs = [await page("/?case=h", consented(subject("5")))];
    rs.push(await page("/?case=h", consented(subject("6"))));
    expect(cacheStates(rs)).toEqual(["MISS", "HIT"]);
    expect(enrolCalls.map((c) => c.subject)).toEqual([subject("5"), subject("6")]);
  });

  test("an answer for another experiment id or version is control", async () => {
    answers.set(subject("7"), {
      experiment_id: "22222222-2222-4222-8222-222222222222",
      version: 3,
      arm: "a",
    });
    answers.set(subject("8"), { experiment_id: EXP.id, version: 2, arm: "a" });
    const rs = [
      await page("/?case=i", consented(subject("7"))),
      await page("/?case=i", consented(subject("8"))),
    ];
    expect(rs.map((r) => r.headers.get("cache-control"))).toEqual([PUBLIC, PUBLIC]);
    expect(cacheStates(rs)).toEqual(["MISS", "HIT"]);
    expect(experimentHeaders("/shop")).toEqual([null]);
    expect(enrolCalls.length).toBe(2);
  });

  test("a path spelling an arm's cache key does not reach that arm's entry", async () => {
    answers.set(subject("9"), "a");
    const enrolled = await page("/p/short", consented(subject("9")));
    expect(enrolled.headers.get("cache-control")).toBe(PRIVATE);
    const armBody = await enrolled.text();
    const crafted = await page(`/p/short|exp:${EXP.id}:3:a`);
    expect([crafted.status, crafted.headers.get("x-edge-cache")]).toEqual([404, "MISS"]);
    expect(await crafted.text()).not.toBe(armBody);
  });

  test("a client's x-experiment header never reaches the API", async () => {
    const forged = { "x-experiment": ARM("b") };
    expect((await page("/?case=j", undefined, "demo.localhost", forged)).status).toBe(200);
    expect((await page("/_p/public/shop", undefined, "demo.localhost", forged)).status).toBe(200);
    expect(api.calls.length).toBe(2);
    expect(api.calls.map((c) => c.headers["x-experiment"] ?? null)).toEqual([null, null]);
  });
});
