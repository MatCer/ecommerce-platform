import { mkdir, mkdtemp, readFile, symlink, writeFile } from "node:fs/promises";
import type http from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import { packArtifact } from "@platform/theme-kit";
import { describe, expect, test } from "vitest";
import { demux, type SandboxSpec, type StepResult } from "../src/docker.ts";
import {
  type Api,
  type CheckSpec,
  errorSummary,
  parseResult,
  type Report,
  runPipeline,
  safeRead,
} from "../src/pipeline.ts";
import { LABEL_KEY } from "../src/policy.ts";
import { createProxy } from "../src/proxy.ts";
import { createQueue, createServer } from "../src/server.ts";

const THEME = fileURLToPath(new URL("../../../themes/default", import.meta.url));
const TOKEN = "builder-token-0123456789abcdef0123456789";

test("demux splits docker's multiplexed log stream", () => {
  const frame = (kind: number, text: string) => {
    const body = Buffer.from(text);
    const head = Buffer.alloc(8);
    head[0] = kind;
    head.writeUInt32BE(body.length, 4);
    return Buffer.concat([head, body]);
  };
  const { stdout, stderr } = demux(
    Buffer.concat([frame(1, "hello "), frame(2, "oops"), frame(1, "world")]),
  );
  expect([stdout, stderr]).toEqual(["hello world", "oops"]);
});

test("errorSummary keeps the error, drops stack frames", () => {
  const log =
    "building...\n12:00 [build] Rendering\nError: NETWORK-PROBE: blocked EAI_AGAIN\n    at probe (x.js:1:1)\n    at y (z.js:2:2)\n  Hint: see docs";
  expect(errorSummary(log)).toBe("Error: NETWORK-PROBE: blocked EAI_AGAIN\n  Hint: see docs");
  expect(errorSummary("a\nb")).toBe("a\nb");
});

test("parseResult takes the last @@result line", () => {
  expect(parseResult('noise\n@@result {"a":1}\nmore\n@@result {"a":2}\n')).toEqual({ a: 2 });
  expect(parseResult("@@result [1]\n")).toBeNull();
  expect(parseResult("@@result {nope\n")).toBeNull();
});

test("safeRead never follows symlinks out of a step's output", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "wp23-out-"));
  const secret = path.join(await mkdtemp(path.join(tmpdir(), "wp23-secret-")), "env");
  await writeFile(secret, "THEME_BUILDER_TOKEN=x");
  await mkdir(path.join(root, "shots"));
  await writeFile(path.join(root, "shots/home-mobile.png"), "png");
  await symlink(secret, path.join(root, "shots/home-desktop.png"));
  await symlink(path.dirname(secret), path.join(root, "linked"));
  expect((await safeRead(root, "shots/home-mobile.png", 10))?.toString()).toBe("png");
  expect(await safeRead(root, "shots/home-mobile.png", 2)).toBeNull();
  expect(await safeRead(root, "shots/home-desktop.png", 100)).toBeNull();
  expect(await safeRead(root, "linked/env", 100)).toBeNull();
  expect(await safeRead(root, "../x", 100)).toBeNull();
});

test("the queue runs one build at a time and ignores duplicates", async () => {
  const order: string[] = [];
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => {
    release = r;
  });
  const q = createQueue(async (_t, rev) => {
    order.push(`start ${rev}`);
    if (rev === "a") await gate;
    order.push(`end ${rev}`);
  });
  expect(q.add("t", "a")).toBe(true);
  expect(q.add("t", "a")).toBe(false);
  expect(q.add("t", "b")).toBe(true);
  expect(q.size).toBe(2);
  release();
  await new Promise((r) => setTimeout(r, 10));
  expect(order).toEqual(["start a", "end a", "start b", "end b"]);
});

test("POST /builds needs the builder token and UUIDs", async () => {
  const added: string[] = [];
  const fetch = createServer({
    token: TOKEN,
    queue: { add: (_t: string, r: string) => added.push(r) > 0, size: 0 } as ReturnType<
      typeof createQueue
    >,
    ready: async () => false,
  });
  const body = JSON.stringify({
    tenant_id: "0192f000-0000-7000-8000-000000000001",
    revision_id: "0192f000-0000-7000-8000-000000000002",
  });
  const post = (auth: string, b = body) =>
    fetch(
      new Request("http://b/builds", { method: "POST", headers: { authorization: auth }, body: b }),
    );
  expect((await post("Bearer wrong")).status).toBe(401);
  expect((await post(`Bearer ${TOKEN}`, '{"tenant_id":"x","revision_id":"../y"}')).status).toBe(
    400,
  );
  expect((await post(`Bearer ${TOKEN}`)).status).toBe(202);
  expect(added).toEqual(["0192f000-0000-7000-8000-000000000002"]);
  expect((await fetch(new Request("http://b/readyz"))).status).toBe(503);
  expect((await fetch(new Request("http://b/healthz"))).status).toBe(200);
});

describe("sandbox proxy", () => {
  async function start(
    containers: Record<string, { Image: string; Labels: Record<string, string> }>,
  ) {
    const calls: string[] = [];
    const upstream = async (o: { method: string; path: string }) => {
      calls.push(`${o.method} ${o.path}`);
      const m = /^\/containers\/([^/]+)\/json$/.exec(o.path);
      const c = m?.[1] ? containers[m[1]] : undefined;
      const res = Readable.from([
        Buffer.from(JSON.stringify(m ? { Config: c ?? {} } : { ok: true })),
      ]) as unknown as http.IncomingMessage;
      Object.assign(res, {
        statusCode: m && !c ? 404 : 200,
        headers: { "content-type": "application/json" },
      });
      return res;
    };
    const server = createProxy(
      {
        image: "img:local",
        labelKey: LABEL_KEY,
        labelValue: "wp23",
        volume: "vol",
        networks: ["none"],
        maxMemory: 4 * 1024 ** 3,
        maxNanoCpus: 4e9,
        maxPids: 1024,
        maxShm: 1024 ** 3,
      },
      upstream,
      () => {},
    );
    await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
    const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    return { base, calls, close: () => server.close() };
  }

  test("forwards only sandbox calls on sandbox containers", async () => {
    const p = await start({
      mine: { Image: "img:local", Labels: { [LABEL_KEY]: "wp23" } },
      other: { Image: "postgres:17", Labels: { "com.docker.compose.project": "wp20" } },
    });
    try {
      expect(
        (await fetch(`${p.base}/v1.44/containers/mine/start`, { method: "POST" })).status,
      ).toBe(200);
      expect(
        (await fetch(`${p.base}/v1.44/containers/other/kill`, { method: "POST" })).status,
      ).toBe(404);
      expect((await fetch(`${p.base}/v1.44/containers/other`, { method: "DELETE" })).status).toBe(
        404,
      );
      expect((await fetch(`${p.base}/v1.44/containers/mine/exec`, { method: "POST" })).status).toBe(
        403,
      );
      const bad = await fetch(`${p.base}/v1.44/containers/create`, {
        method: "POST",
        body: JSON.stringify({ Image: "img:local", HostConfig: { Privileged: true } }),
      });
      expect(bad.status).toBe(403);
      expect(await bad.text()).toContain("sandbox policy");
      expect(p.calls).toEqual([
        "GET /containers/mine/json",
        "POST /v1.44/containers/mine/start",
        "GET /containers/other/json",
        "GET /containers/other/json",
      ]);
    } finally {
      p.close();
    }
  });
});

describe("pipeline", () => {
  /** A fake API + docker that record what the builder does. */
  async function harness(
    steps: Record<string, (s: SandboxSpec, work: string) => Promise<Partial<StepResult>>>,
  ) {
    const workDir = await mkdtemp(path.join(tmpdir(), "wp23-work-"));
    const statuses: [string, Report][] = [];
    const uploads: string[] = [];
    const revision = "0192f000-0000-7000-8000-000000000009";
    const api: Api = {
      spec: async () => ({
        tenant_id: "t",
        revision_id: revision,
        number: 2,
        change: "fork",
        status: "draft",
        tokens_only: false,
        astro_key_hex: "00".repeat(32),
      }),
      source: async () => Buffer.from("tar.gz"),
      status: async (_t, _r, status, checks) => {
        statuses.push([status, structuredClone(checks)]);
      },
      artifact: async (_t, _r, tar): Promise<CheckSpec> => {
        uploads.push(`artifact ${tar.length > 0}`);
        return {
          artifact_id: "x",
          preview_host: "preview-2--demo.localhost",
          preview_token: "tok",
          pages: ["/"],
        };
      },
      screenshot: async (_t, _r, name) => {
        uploads.push(`shot ${name}`);
      },
    };
    const specs: SandboxSpec[] = [];
    const docker = {
      run: async (s: SandboxSpec): Promise<StepResult> => {
        specs.push(s);
        const r = await (steps[s.step] ?? (async () => ({})))(s, workDir);
        return { exitCode: 0, timedOut: false, stdout: "", stderr: "", ms: 1, ...r };
      },
    };
    const run = () =>
      runPipeline(
        {
          docker,
          api,
          workDir,
          checkNetwork: "net",
          proxyHost: "caddy",
          lighthouseRuns: 1,
          log: () => {},
        },
        "t",
        revision,
      );
    return { run, statuses, uploads, specs, workDir, revision };
  }

  test("lint violations fail the revision before anything is built", async () => {
    const h = await harness({
      static: async () => ({
        exitCode: 0,
        stdout: `@@result ${JSON.stringify({ violations: [{ file: "src/x.astro", line: 2, rule: "foreign-fetch", message: "fetch to another origin" }], typecheck: null, checks: [] })}`,
      }),
    });
    await h.run();
    const [first, last] = [h.statuses[0], h.statuses.at(-1)];
    expect(first?.[0]).toBe("building");
    expect(last?.[0]).toBe("failed");
    expect(last?.[1].failures).toEqual([
      "lint: src/x.astro:2 foreign-fetch: fetch to another origin",
    ]);
    expect(h.specs.map((s) => [s.step, s.network])).toEqual([["static", "none"]]);
    expect(h.uploads).toEqual([]);
  });

  test("a failing build reports its log (e.g. blocked network)", async () => {
    const h = await harness({
      static: async () => ({
        stdout: '@@result {"violations":[],"typecheck":{"ok":true,"ms":1,"log":""},"checks":[]}',
      }),
      build: async () => ({
        exitCode: 1,
        stderr: "TypeError: fetch failed (getaddrinfo EAI_AGAIN example.com)",
      }),
    });
    await h.run();
    const last = h.statuses.at(-1);
    expect(last?.[0]).toBe("failed");
    expect(last?.[1].failures[0]).toContain("EAI_AGAIN");
    expect(h.specs.find((s) => s.step === "build")?.network).toBe("none");
    expect(h.specs.find((s) => s.step === "build")?.env.ASTRO_KEY).toBe(
      Buffer.alloc(32).toString("base64"),
    );
  });

  test("a build whose output is a symlink is refused", async () => {
    const h = await harness({
      static: async () => ({
        stdout: '@@result {"violations":[],"typecheck":{"ok":true,"ms":1,"log":""},"checks":[]}',
      }),
      build: async (s, work) => {
        const out = path.join(work, s.job, "out/build");
        await symlink("/etc", path.join(out, "artifacts"));
        return { stdout: `@@result {"ok":true,"artifact_id":"${"a".repeat(32)}"}` };
      },
    });
    await h.run();
    expect(h.statuses.at(-1)?.[1].failures[0]).toContain("not a plain directory");
    expect(h.uploads).toEqual([]);
  });

  test("happy path: verified artifact, budgets, smoke, screenshots → ready", async () => {
    const h = await harness({
      static: async () => ({
        stdout:
          '@@result {"violations":[],"typecheck":{"ok":true,"ms":5,"log":""},"checks":["checks/buy.spec.ts"]}',
      }),
      build: async (s, work) => {
        // A real (tiny) Astro-shaped build, packed like the sandbox does.
        const dist = path.join(work, "dist");
        await mkdir(path.join(dist, "server"), { recursive: true });
        await mkdir(path.join(dist, "client/_astro"), { recursive: true });
        await writeFile(path.join(dist, "server/entry.mjs"), "export default {}");
        await writeFile(path.join(dist, "client/_astro/a.js"), "1");
        const m = await packArtifact({
          dist,
          outRoot: path.join(work, s.job, "out/build/artifacts"),
          kind: "theme",
          tokensFile: path.join(THEME, "theme.tokens.json"),
          projectDir: THEME,
        });
        return { stdout: `@@result {"ok":true,"artifact_id":"${m.id}"}` };
      },
      check: async (s, work) => {
        const out = path.join(work, s.job, "out/check");
        const page = {
          path: "/",
          kind: "home",
          lcpMs: 1100,
          tbtMs: 0,
          cls: 0,
          jsGzip: 20_000,
          jsTransfer: 20_000,
          jsGzipWithRum: 21_000,
          thirdPartyOrigins: [],
          axe: [],
          cspViolations: [],
          subrequests: 2,
        };
        await writeFile(path.join(out, "report.json"), JSON.stringify({ results: [page] }));
        await writeFile(
          path.join(out, "result.json"),
          JSON.stringify({ smoke: { ok: true, log: "" } }),
        );
        await mkdir(path.join(out, "shots"));
        await writeFile(path.join(out, "shots/home-mobile.png"), "png");
        expect(s.env.PREVIEW_COOKIE).toBe("__Host-preview=tok");
        expect(s.env.THEME_KIT_CHROMIUM_ARGS).toContain("MAP *.localhost caddy");
        return {};
      },
    });
    await h.run();
    const [status, report] = h.statuses.at(-1) ?? [];
    expect(report?.failures).toEqual([]);
    expect(status).toBe("ready");
    expect(report?.steps.map((s) => `${s.name}:${s.status}`)).toEqual([
      "lint:passed",
      "typecheck:passed",
      "build:passed",
      "budget:passed",
      "smoke:passed",
      "functional:passed",
    ]);
    expect(h.uploads).toEqual(["artifact true", "shot home-mobile"]);
    expect(h.specs.map((s) => [s.step, s.network])).toEqual([
      ["static", "none"],
      ["build", "none"],
      ["check", "net"],
      ["functional", "net"],
    ]);
    // The job directory is gone afterwards.
    await expect(readFile(path.join(h.workDir, h.revision, "in/source.tar.gz"))).rejects.toThrow();
  });

  test("a budget failure fails the revision with the numbers", async () => {
    const h = await harness({
      static: async () => ({ stdout: '@@result {"violations":[],"typecheck":null,"checks":[]}' }),
      build: async (s, work) => {
        const dist = path.join(work, "dist2");
        await mkdir(path.join(dist, "server"), { recursive: true });
        await mkdir(path.join(dist, "client"), { recursive: true });
        await writeFile(path.join(dist, "server/entry.mjs"), "export default {}");
        const m = await packArtifact({
          dist,
          outRoot: path.join(work, s.job, "out/build/artifacts"),
          kind: "theme",
          projectDir: THEME,
        });
        return { stdout: `@@result {"ok":true,"artifact_id":"${m.id}"}` };
      },
      check: async (s, work) => {
        const out = path.join(work, s.job, "out/check");
        const page = {
          path: "/p/x",
          kind: "product",
          lcpMs: 1200,
          tbtMs: 0,
          cls: 0,
          jsGzip: 60_000,
          jsTransfer: 1,
          jsGzipWithRum: 61_000,
          thirdPartyOrigins: [],
          axe: [],
          cspViolations: [],
          subrequests: 2,
        };
        await writeFile(path.join(out, "report.json"), JSON.stringify({ results: [page] }));
        await writeFile(
          path.join(out, "result.json"),
          JSON.stringify({ smoke: { ok: true, log: "" } }),
        );
        return {};
      },
    });
    await h.run();
    const [status, report] = h.statuses.at(-1) ?? [];
    expect(status).toBe("failed");
    expect(report?.failures).toContain("budget /p/x: JS 58.6 kB gz > 30");
  });
});
