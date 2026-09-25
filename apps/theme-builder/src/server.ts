#!/usr/bin/env node
/**
 * Theme builder service (WP23, spec §3, §12.3): takes build requests from the worker
 * (`POST /builds`, THEME_BUILDER_TOKEN), runs them one at a time through the sandboxed
 * pipeline and reports back to the API. It holds no Docker socket (only the sandbox proxy's
 * address) and no database or storage credentials.
 *
 * Env: API_ORIGIN, THEME_BUILDER_TOKEN, DOCKER_PROXY_URL, SANDBOX_IMAGE, SANDBOX_LABEL,
 * SANDBOX_VOLUME, CHECK_NETWORK, PROXY_HOST (caddy), WORK_DIR (/work), PORT (4020),
 * LIGHTHOUSE_RUNS (3).
 */
import { timingSafeEqual } from "node:crypto";
import { readdir, rm } from "node:fs/promises";
import path from "node:path";
import { serve } from "@hono/node-server";
import { Docker } from "./docker.ts";
import { type Api, type BuildSpec, type CheckSpec, type Report, runPipeline } from "./pipeline.ts";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export function apiClient(origin: string, token: string): Api {
  const call = async (tenant: string, method: string, p: string, body?: BodyInit, type?: string) => {
    const res = await fetch(`${origin}/internal/v1/themes/revisions/${p}`, {
      method,
      headers: {
        authorization: `Bearer ${token}`,
        "x-tenant-id": tenant,
        ...(type ? { "content-type": type } : {}),
      },
      body,
      signal: AbortSignal.timeout(120_000),
    });
    if (!res.ok) throw new Error(`api ${method} ${p.split("/").slice(1).join("/")}: ${res.status} ${(await res.text()).slice(0, 300)}`);
    return res;
  };
  return {
    spec: async (t, r) => (await (await call(t, "GET", `${r}/build`)).json()) as BuildSpec,
    source: async (t, r) => Buffer.from(await (await call(t, "GET", `${r}/source`)).arrayBuffer()),
    status: async (t, r, status, checks: Report) => {
      await call(t, "POST", `${r}/status`, JSON.stringify({ status, checks }), "application/json");
    },
    artifact: async (t, r, tar) =>
      (await (await call(t, "PUT", `${r}/artifact`, new Uint8Array(tar), "application/x-tar")).json()) as CheckSpec,
    screenshot: async (t, r, name, png) => {
      await call(t, "PUT", `${r}/screenshots/${name}`, new Uint8Array(png), "image/png");
    },
  };
}

function bearerOk(header: string | null, expected: string) {
  const got = Buffer.from(header?.replace(/^Bearer /, "") ?? "");
  const want = Buffer.from(expected);
  return expected.length >= 32 && got.length === want.length && timingSafeEqual(got, want);
}

/** One build at a time (machine limits); a revision already queued or running is not re-added. */
export function createQueue(run: (tenant: string, revision: string) => Promise<void>) {
  const pending: { tenant: string; revision: string }[] = [];
  const known = new Set<string>();
  let busy = false;
  const next = async () => {
    if (busy) return;
    const item = pending.shift();
    if (!item) return;
    busy = true;
    try {
      await run(item.tenant, item.revision);
    } catch (err) {
      console.log(JSON.stringify({ level: "error", msg: "build failed", revision: item.revision, err: String(err) }));
    } finally {
      known.delete(item.revision);
      busy = false;
      void next();
    }
  };
  return {
    add(tenant: string, revision: string) {
      if (known.has(revision)) return false;
      known.add(revision);
      pending.push({ tenant, revision });
      void next();
      return true;
    },
    get size() {
      return pending.length + (busy ? 1 : 0);
    },
  };
}

export function createServer(opts: {
  token: string;
  queue: ReturnType<typeof createQueue>;
  ready: () => Promise<boolean>;
}) {
  return async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    if (url.pathname === "/healthz") return Response.json({ status: "ok" });
    if (url.pathname === "/readyz") {
      const ok = await opts.ready();
      return Response.json({ status: ok ? "ok" : "degraded", queued: opts.queue.size }, { status: ok ? 200 : 503 });
    }
    if (url.pathname !== "/builds") return new Response("not found", { status: 404 });
    if (req.method !== "POST") return new Response("method not allowed", { status: 405 });
    if (!bearerOk(req.headers.get("authorization"), opts.token))
      return new Response("unauthorized", { status: 401 });
    const body = (await req.json().catch(() => null)) as { tenant_id?: unknown; revision_id?: unknown } | null;
    const tenant = typeof body?.tenant_id === "string" ? body.tenant_id : "";
    const revision = typeof body?.revision_id === "string" ? body.revision_id : "";
    if (!UUID.test(tenant) || !UUID.test(revision))
      return Response.json({ message: "tenant_id and revision_id must be UUIDs" }, { status: 400 });
    const queued = opts.queue.add(tenant, revision);
    return Response.json({ queued, position: opts.queue.size }, { status: 202 });
  };
}

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name}`);
  return v;
}

if (import.meta.main) {
  const workDir = process.env.WORK_DIR ?? "/work";
  const docker = new Docker({
    url: required("DOCKER_PROXY_URL"),
    image: required("SANDBOX_IMAGE"),
    project: required("SANDBOX_LABEL"),
    volume: required("SANDBOX_VOLUME"),
    user: "1000:1000",
  });
  const api = apiClient(required("API_ORIGIN"), required("THEME_BUILDER_TOKEN"));
  const queue = createQueue((tenant, revision) =>
    runPipeline(
      {
        docker,
        api,
        workDir,
        checkNetwork: required("CHECK_NETWORK"),
        proxyHost: process.env.PROXY_HOST ?? "caddy",
        lighthouseRuns: Number(process.env.LIGHTHOUSE_RUNS ?? 3),
      },
      tenant,
      revision,
    ),
  );
  // Leftovers of a crash: this project's sandbox containers and job directories.
  void docker
    .cleanup()
    .then((n) => n && console.log(JSON.stringify({ level: "info", msg: "removed leftover sandboxes", n })))
    .catch(() => {});
  for (const d of await readdir(workDir).catch(() => []))
    if (UUID.test(d)) await rm(path.join(workDir, d), { recursive: true, force: true });
  const port = Number(process.env.PORT ?? 4020);
  const server = serve({
    fetch: createServer({ token: required("THEME_BUILDER_TOKEN"), queue, ready: () => docker.ping() }),
    port,
  });
  console.log(JSON.stringify({ level: "info", msg: "theme builder listening", port }));
  for (const signal of ["SIGINT", "SIGTERM"] as const)
    process.on(signal, () => {
      server.close();
      process.exit(0);
    });
}
