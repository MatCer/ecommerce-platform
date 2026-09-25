#!/usr/bin/env node
/**
 * Sandbox proxy (WP23, A6): the only process that holds the Docker socket. It forwards the
 * few Engine API calls the theme builder needs (`policy.ts`) and refuses everything else:
 * - `POST /containers/create`: the body must pass `checkCreate` (one image, one label, no
 *   network or the internal check network, read-only root, no capabilities, non-root, limits);
 * - start / wait / logs / kill / remove: only for containers carrying the sandbox label and
 *   image (checked with an inspect on every call);
 * - `GET /containers/json`: always filtered to the sandbox label (cleanup of leftovers);
 * - `/_ping`, `/version`.
 *
 * Env: DOCKER_SOCKET (default /var/run/docker.sock), PORT (2375), SANDBOX_IMAGE,
 * SANDBOX_LABEL (compose project), SANDBOX_VOLUME, SANDBOX_NETWORKS ("none,<net>").
 */
import http from "node:http";
import { checkCreate, LABEL_KEY, type Policy, route, safeQuery } from "./policy.ts";

const MAX_BODY = 64 * 1024;

function env(name: string, fallback?: string): string {
  const v = process.env[name] ?? fallback;
  if (!v) throw new Error(`missing env ${name}`);
  return v;
}

export function policyFromEnv(): Policy {
  return {
    image: env("SANDBOX_IMAGE"),
    labelKey: LABEL_KEY,
    labelValue: env("SANDBOX_LABEL"),
    volume: env("SANDBOX_VOLUME"),
    networks: env("SANDBOX_NETWORKS", "none")
      .split(",")
      .map((n) => n.trim())
      .filter(Boolean),
    maxMemory: 4 * 1024 ** 3,
    maxNanoCpus: 4e9,
    maxPids: 1024,
    maxShm: 1024 ** 3,
  };
}

type Upstream = (opts: {
  method: string;
  path: string;
  headers: http.OutgoingHttpHeaders;
  body?: Buffer;
}) => Promise<http.IncomingMessage>;

export function socketUpstream(socketPath: string): Upstream {
  return ({ method, path, headers, body }) =>
    new Promise((resolve, reject) => {
      const req = http.request({ socketPath, method, path, headers }, resolve);
      req.on("error", reject);
      req.setTimeout(15 * 60_000, () => req.destroy(new Error("docker timeout")));
      req.end(body);
    });
}

async function readBody(req: http.IncomingMessage, max: number): Promise<Buffer | null> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    size += (chunk as Buffer).length;
    if (size > max) return null;
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks);
}

async function json(res: http.IncomingMessage): Promise<unknown> {
  const body = await readBody(res, 4 * 1024 * 1024);
  return body ? JSON.parse(body.toString("utf8")) : null;
}

export function createProxy(policy: Policy, upstream: Upstream, log = console.log) {
  const deny = (res: http.ServerResponse, status: number, message: string) => {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify({ message }));
  };

  /** Only containers of the sandbox image and label, looked up fresh on every call. */
  async function ours(id: string): Promise<boolean> {
    const res = await upstream({ method: "GET", path: `/containers/${id}/json`, headers: {} });
    if (res.statusCode !== 200) {
      res.resume();
      return false;
    }
    const c = (await json(res)) as { Config?: { Image?: string; Labels?: Record<string, string> } };
    return c.Config?.Image === policy.image && c.Config.Labels?.[policy.labelKey] === policy.labelValue;
  }

  return http.createServer(async (req, res) => {
    try {
      const method = req.method ?? "GET";
      const url = req.url ?? "/";
      const r = route(method, url);
      if (!r) {
        log(JSON.stringify({ level: "warn", msg: "sandbox proxy refused", method, path: url.split("?")[0] }));
        return deny(res, 403, "not allowed by the sandbox proxy");
      }
      let body: Buffer | undefined;
      if (r.kind === "create") {
        const raw = await readBody(req, MAX_BODY);
        if (!raw) return deny(res, 413, "body too large");
        let parsed: unknown;
        try {
          parsed = JSON.parse(raw.toString("utf8"));
        } catch {
          return deny(res, 400, "invalid JSON");
        }
        const problems = checkCreate(parsed, policy, r.name);
        if (problems.length) {
          log(JSON.stringify({ level: "warn", msg: "sandbox create refused", problems }));
          return deny(res, 403, `sandbox policy: ${problems.join("; ")}`);
        }
        body = Buffer.from(JSON.stringify(parsed));
      } else {
        req.resume();
      }
      if (r.kind === "container" && !(await ours(r.id)))
        return deny(res, 404, "no such sandbox container");
      const path = `${new URL(url, "http://docker").pathname}${safeQuery(r, url, policy)}`;
      const up = await upstream({
        method,
        path,
        headers: body ? { "content-type": "application/json", "content-length": body.length } : {},
        body,
      });
      const headers: http.OutgoingHttpHeaders = {};
      for (const k of ["content-type", "api-version", "docker-experimental", "ostype"])
        if (up.headers[k]) headers[k] = up.headers[k];
      res.writeHead(up.statusCode ?? 502, headers);
      up.pipe(res);
    } catch (err) {
      log(JSON.stringify({ level: "error", msg: "sandbox proxy error", err: String(err) }));
      if (!res.headersSent) deny(res, 502, "docker unavailable");
      else res.destroy();
    }
  });
}

if (import.meta.main) {
  const policy = policyFromEnv();
  const server = createProxy(policy, socketUpstream(env("DOCKER_SOCKET", "/var/run/docker.sock")));
  const port = Number(process.env.PORT ?? 2375);
  server.listen(port, () =>
    console.log(JSON.stringify({ level: "info", msg: "sandbox proxy listening", port, image: policy.image })),
  );
  for (const signal of ["SIGINT", "SIGTERM"] as const)
    process.on(signal, () => server.close(() => process.exit(0)));
}
