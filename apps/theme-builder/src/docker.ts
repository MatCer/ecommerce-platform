/**
 * A minimal Docker Engine API client for the sandbox steps, spoken to the sandbox proxy
 * (`proxy.ts`), never to the socket itself.
 */
import { randomBytes } from "node:crypto";
import { LABEL_KEY } from "./policy.ts";

export interface SandboxSpec {
  /** Step name, part of the container name (`lint`, `build`, `check`, `functional`). */
  step: string;
  job: string;
  cmd: string[];
  env: Record<string, string>;
  /** `none` or the internal check network. */
  network: string;
  /** Volume subpaths: `<job>/in` (read-only at /in), `<job>/out/<step>` (at /out). */
  mounts: { target: "/in" | "/out"; subpath: string }[];
  memoryBytes: number;
  cpus: number;
  pids: number;
  timeoutMs: number;
}

export interface StepResult {
  exitCode: number;
  timedOut: boolean;
  stdout: string;
  stderr: string;
  ms: number;
}

export interface DockerOptions {
  /** e.g. `http://theme-sandbox-proxy:2375` */
  url: string;
  image: string;
  /** The compose project: sandbox label value and container name prefix. */
  project: string;
  volume: string;
  /** uid:gid the sandbox runs as (non-root). */
  user: string;
}

/** What the builder keeps of a step's output (the end, where `@@result` is). */
const MAX_LOG = 1024 * 1024;
/**
 * Docker keeps at most this much log per sandbox (json-file rotation, one file), and the
 * builder never reads more: a theme printing without end cannot exhaust either.
 */
export const LOG_MAX_SIZE = "8m";
const MAX_LOG_READ = 12 * 1024 * 1024;

/** Reads a response body up to `max` bytes (then cancels the stream). */
async function readCapped(res: Response, max: number): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let size = 0;
  const reader = res.body?.getReader();
  if (!reader) return new Uint8Array();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > max) {
      await reader.cancel().catch(() => {});
      break;
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks);
}

/** Splits Docker's multiplexed log stream (8-byte frame headers) into stdout and stderr. */
export function demux(buf: Uint8Array): { stdout: string; stderr: string } {
  const out: Buffer[] = [];
  const err: Buffer[] = [];
  const b = Buffer.from(buf);
  let i = 0;
  while (i + 8 <= b.length) {
    const kind = b[i];
    const size = b.readUInt32BE(i + 4);
    const frame = b.subarray(i + 8, i + 8 + size);
    (kind === 2 ? err : out).push(frame);
    i += 8 + size;
  }
  return {
    stdout: Buffer.concat(out).toString("utf8"),
    stderr: Buffer.concat(err).toString("utf8"),
  };
}

export class Docker {
  readonly #o: DockerOptions;
  constructor(o: DockerOptions) {
    this.#o = o;
  }

  async #call(method: string, path: string, body?: unknown, signal?: AbortSignal) {
    const res = await fetch(`${this.#o.url}/v1.47${path}`, {
      method,
      headers: body ? { "content-type": "application/json" } : {},
      body: body ? JSON.stringify(body) : undefined,
      signal,
    });
    if (!res.ok && res.status !== 304) {
      throw new Error(
        `docker ${method} ${path.split("?")[0]}: ${res.status} ${(await res.text()).slice(0, 500)}`,
      );
    }
    return res;
  }

  async ping(): Promise<boolean> {
    return this.#call("GET", "/_ping").then(
      () => true,
      () => false,
    );
  }

  /** The create body for a step (what `policy.ts` expects). */
  createBody(s: SandboxSpec) {
    return {
      Image: this.#o.image,
      Cmd: s.cmd,
      Env: Object.entries(s.env).map(([k, v]) => `${k}=${v}`),
      User: this.#o.user,
      WorkingDir: "/work",
      Labels: { [LABEL_KEY]: this.#o.project, "platform.theme-job": s.job },
      AttachStdout: true,
      AttachStderr: true,
      HostConfig: {
        NetworkMode: s.network,
        ReadonlyRootfs: true,
        CapDrop: ["ALL"],
        SecurityOpt: ["no-new-privileges"],
        Memory: s.memoryBytes,
        MemorySwap: s.memoryBytes,
        NanoCpus: Math.round(s.cpus * 1e9),
        PidsLimit: s.pids,
        ShmSize: 256 * 1024 * 1024,
        Init: true,
        LogConfig: { Type: "json-file", Config: { "max-size": LOG_MAX_SIZE, "max-file": "1" } },
        Tmpfs: {
          "/work": `rw,noexec,nosuid,nodev,size=1024m,uid=${this.#uid()},gid=${this.#uid()},mode=0700`,
          "/tmp": `rw,noexec,nosuid,nodev,size=1024m,uid=${this.#uid()},gid=${this.#uid()},mode=0700`,
        },
        Mounts: s.mounts.map((m) => ({
          Type: "volume",
          Source: this.#o.volume,
          Target: m.target,
          ...(m.target === "/in" ? { ReadOnly: true } : {}),
          VolumeOptions: { Subpath: m.subpath },
        })),
      },
    };
  }

  #uid() {
    return this.#o.user.split(":")[0] ?? "1000";
  }

  /** Runs one disposable container to completion (or the timeout) and removes it. */
  async run(s: SandboxSpec): Promise<StepResult> {
    const started = Date.now();
    const name = `${this.#o.project}-tb-${s.job.slice(0, 12)}-${s.step}-${randomBytes(3).toString("hex")}`;
    const created = (await (
      await this.#call("POST", `/containers/create?name=${name}`, this.createBody(s))
    ).json()) as { Id: string };
    const id = created.Id;
    try {
      await this.#call("POST", `/containers/${id}/start`);
      const abort = new AbortController();
      const timer = setTimeout(() => abort.abort(), s.timeoutMs);
      let exitCode = -1;
      let timedOut = false;
      try {
        const res = await this.#call("POST", `/containers/${id}/wait`, undefined, abort.signal);
        exitCode = ((await res.json()) as { StatusCode: number }).StatusCode;
      } catch (err) {
        if (!abort.signal.aborted) throw err;
        timedOut = true;
        await this.#call("POST", `/containers/${id}/kill?signal=SIGKILL`).catch(() => {});
      } finally {
        clearTimeout(timer);
      }
      const logs = await readCapped(
        await this.#call("GET", `/containers/${id}/logs?stdout=1&stderr=1`),
        MAX_LOG_READ,
      );
      const { stdout, stderr } = demux(logs);
      return {
        exitCode,
        timedOut,
        stdout: stdout.slice(-MAX_LOG),
        stderr: stderr.slice(-MAX_LOG),
        ms: Date.now() - started,
      };
    } finally {
      await this.#call("DELETE", `/containers/${id}?force=1&v=1`).catch(() => {});
    }
  }

  /** Removes sandbox containers left over from a crash (this project's label only). */
  async cleanup(): Promise<number> {
    const list = (await (await this.#call("GET", "/containers/json?all=1")).json()) as {
      Id: string;
    }[];
    for (const c of list)
      await this.#call("DELETE", `/containers/${c.Id}?force=1&v=1`).catch(() => {});
    return list.length;
  }
}
