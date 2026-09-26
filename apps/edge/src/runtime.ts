import { readFile } from "node:fs/promises";
import path from "node:path";
import { type ArtifactManifest, readManifest } from "@platform/theme-kit";
import { Log, LogLevel, Response as MfResponse, Miniflare } from "miniflare";

/** A Node-side service binding: the worker calls `env.NAME.fetch(request)`. */
export type NodeBinding = (request: Request) => Promise<Response>;

export interface PoolOptions {
  artifactRoot: string;
  /**
   * Bindings for an instance (see bindings.ts for the policy). Nothing else is injected.
   * `scope` is the tenant for untrusted theme instances (see WorkerPool), else undefined.
   */
  bindings: (manifest: ArtifactManifest, scope: string | undefined) => Record<string, NodeBinding>;
  /** Called for every outbound `fetch()`/`connect()` a worker attempts. Always denied. */
  onOutbound?: (manifest: ArtifactManifest, url: string) => void;
}

interface Instance {
  manifest: ArtifactManifest;
  mf: Miniflare;
  lastUsed: number;
}

// Miniflare hands us its own bundled undici classes; convert at the boundary.
type MfRequestLike = {
  url: string;
  method: string;
  headers: Iterable<[string, string]>;
  arrayBuffer(): Promise<ArrayBuffer>;
};

const hasBody = (method: string) => method !== "GET" && method !== "HEAD";

async function toStdRequest(req: MfRequestLike): Promise<Request> {
  return new Request(req.url, {
    method: req.method,
    headers: [...req.headers],
    body: hasBody(req.method) ? await req.arrayBuffer() : undefined,
  });
}

async function toMfResponse(res: Response): Promise<MfResponse> {
  return new MfResponse(res.body ? await res.arrayBuffer() : null, {
    status: res.status,
    headers: [...res.headers],
  });
}

const EGRESS_DENY = "egress-deny";
const MAX_THEME_INSTANCES = 28;
const MAX_CHECKOUT_INSTANCES = 4;
// Answers every outbound fetch with 403 and reports it; has no `connect` handler, so TCP
// sockets fail to open.
const DENY_WORKER = `export default {
  async fetch(request, env) {
    await env.REPORT.fetch("http://report/?url=" + encodeURIComponent(request.url));
    return new Response("outbound network access is not allowed", { status: 403 });
  },
};`;

async function createInstance(
  opts: PoolOptions,
  id: string,
  scope: string | undefined,
  onCreated: (mf: Miniflare) => void,
): Promise<Instance> {
  const manifest = await readManifest(opts.artifactRoot, id);
  const serverDir = path.resolve(opts.artifactRoot, id, "server");
  // Explicit module map from the manifest: nothing outside it can be imported.
  const modules: Record<string, { type: "esm"; contents: string }> = {};
  for (const p of manifest.runtime.modules) {
    modules[p] = { type: "esm", contents: await readFile(path.join(serverDir, p), "utf8") };
  }
  const env = Object.fromEntries(
    Object.entries(opts.bindings(manifest, scope)).map(([name, fn]) => [
      name,
      {
        type: "fetcher" as const,
        handler: async (req: MfRequestLike) => toMfResponse(await fn(await toStdRequest(req))),
      },
    ]),
  );
  const mf = new Miniflare({
    port: 0,
    // No request.cf lookup: Miniflare would fetch it from the internet at startup and cache it
    // under node_modules (read-only in the image). Themes get the placeholder object.
    cf: false,
    log: new Log(LogLevel.WARN, { prefix: `workerd:${id.slice(0, 8)}` }),
    workers: [
      {
        config: {
          name: `${manifest.kind}-${id}`,
          compatibilityDate: manifest.runtime.compatibility_date,
          compatibilityFlags: manifest.runtime.compatibility_flags,
          manifest: { mainModule: manifest.runtime.main, modulesRoot: serverDir, modules },
          env,
        },
        // Spec A7: global outbound is denied; only the bindings reach the platform.
        // A worker (not a Node "fetcher") is required here: with a fetcher, Miniflare still passes
        // `connect()` TCP sockets through to the internet (verified in WP2, see runtime-contract.md).
        dev: { outboundService: { type: "worker", worker: EGRESS_DENY } },
      },
      {
        config: {
          name: EGRESS_DENY,
          compatibilityDate: manifest.runtime.compatibility_date,
          manifest: {
            mainModule: "deny.mjs",
            modules: { "deny.mjs": { type: "esm", contents: DENY_WORKER } },
          },
          env: {
            REPORT: {
              type: "fetcher",
              handler: (req: MfRequestLike) => {
                opts.onOutbound?.(manifest, new URL(req.url).searchParams.get("url") ?? "");
                return new MfResponse(null, { status: 204 });
              },
            },
          },
        },
      },
    ],
  });
  // Publish the handle before waiting for startup. A deadline can dispose a workerd
  // whose ready promise never settles.
  onCreated(mf);
  await mf.ready;
  return { manifest, mf, lastUsed: Date.now() };
}

interface PoolEntry {
  instance: Promise<Instance>;
  created: Promise<Miniflare | undefined>;
  active: number;
  lastUsed: number;
  stopping?: Promise<void>;
}

/**
 * One Miniflare (workerd) instance per distinct artifact (spec A30), created on first use.
 * Instances are keyed by the content-addressed artifact id, so publish/rollback is a pointer
 * change and never mutates a running instance.
 */
export class WorkerPool {
  readonly #opts: PoolOptions;
  readonly #instances = new Map<string, PoolEntry>();
  #admission: Promise<void> = Promise.resolve();

  constructor(opts: PoolOptions) {
    this.#opts = opts;
  }

  get size() {
    return this.#instances.size;
  }

  /** Instance key: an artifact, optionally isolated per scope (tenant). */
  static key(id: string, scope?: string) {
    return scope ? `${id}@${scope}` : id;
  }

  has(id: string, scope?: string) {
    const entry = this.#instances.get(WorkerPool.key(id, scope));
    return Boolean(entry && !entry.stopping);
  }

  /** Artifact ids with a running instance (any scope). */
  ids(): Set<string> {
    return new Set([...this.#instances.keys()].map((k) => k.split("@")[0] ?? k));
  }

  async #instance(
    id: string,
    scope: string | undefined,
  ): Promise<{ inst: Instance; entry: PoolEntry }> {
    const key = WorkerPool.key(id, scope);
    let entry = this.#instances.get(key);
    if (!entry) {
      // Serialize admission, including eviction: concurrent cold requests cannot each
      // observe a free slot and exceed the process bound.
      const previous = this.#admission;
      let release = () => {};
      this.#admission = new Promise<void>((resolve) => {
        release = resolve;
      });
      await previous;
      try {
        entry = this.#instances.get(key);
        if (!entry) {
          const limit = scope === undefined ? MAX_CHECKOUT_INSTANCES : MAX_THEME_INSTANCES;
          const category = (candidate: string) => candidate.includes("@") === (scope !== undefined);
          const members = [...this.#instances].filter(([candidate]) => category(candidate));
          if (members.length >= limit) {
            const idle = members
              .filter(([, value]) => value.active === 0 && !value.stopping)
              .sort((a, b) => a[1].lastUsed - b[1].lastUsed)[0];
            if (!idle) throw new Error("theme runtime capacity reached");
            await this.#evictKey(idle[0]);
          }
          let signalCreated = (_mf: Miniflare | undefined) => {};
          const created = new Promise<Miniflare | undefined>((resolve) => {
            signalCreated = resolve;
          });
          const value: PoolEntry = {
            created,
            active: 0,
            lastUsed: Date.now(),
            instance: createInstance(this.#opts, id, scope, signalCreated),
          };
          this.#instances.set(key, value);
          void value.instance.catch(() => {
            signalCreated(undefined);
            return this.#evictKey(key);
          });
          entry = value;
        }
      } finally {
        release();
      }
    }
    if (entry.stopping) {
      // Keep the slot charged until workerd exits, then let this request retry.
      await entry.stopping;
      return this.#instance(id, scope);
    }
    entry.active++;
    try {
      const inst = await entry.instance;
      entry.lastUsed = inst.lastUsed = Date.now();
      return { inst, entry };
    } catch (err) {
      entry.active--;
      throw err;
    }
  }

  /**
   * Dispatches to the instance of artifact `id`. With a `scope`, each scope gets its own isolate:
   * module state is never shared between tenants, so a worker cannot keep one tenant's request
   * context and replay it while serving another tenant (WP2 review finding).
   */
  async fetch(id: string, request: Request, scope?: string): Promise<Response> {
    const {
      inst: { mf },
      entry,
    } = await this.#instance(id, scope);
    try {
      const res = await mf.dispatchFetch(request.url, {
        method: request.method,
        headers: [...request.headers],
        body: hasBody(request.method) ? await request.arrayBuffer() : undefined,
        redirect: "manual",
      });
      return new Response(res.body as ReadableStream<Uint8Array> | null, {
        status: res.status,
        statusText: res.statusText,
        headers: [...res.headers],
      });
    } finally {
      entry.active--;
      entry.lastUsed = Date.now();
    }
  }

  /** Disposes every instance of artifact `id` (all scopes). */
  async evict(id: string) {
    const keys = [...this.#instances.keys()].filter((k) => k === id || k.startsWith(`${id}@`));
    await Promise.all(keys.map((k) => this.#evictKey(k)));
  }

  /** Stop one tenant's timed-out instance without interrupting other tenants. */
  async evictScope(id: string, scope?: string) {
    await this.#evictKey(WorkerPool.key(id, scope));
  }

  async #evictKey(key: string) {
    const entry = this.#instances.get(key);
    if (!entry) return;
    if (!entry.stopping) {
      entry.stopping = (async () => {
        const mf = await entry.created;
        await mf?.dispose();
        // A failed termination must keep occupying its slot; otherwise a hostile
        // startup can force unbounded replacements.
        if (this.#instances.get(key) === entry) this.#instances.delete(key);
      })();
    }
    await entry.stopping;
  }

  /** Disposes instances unused for `idleMs`; they are recreated on the next request. */
  async evictIdle(idleMs: number, now = Date.now()) {
    for (const [id, entry] of this.#instances) {
      if (entry.active === 0 && now - entry.lastUsed >= idleMs) await this.#evictKey(id);
    }
  }

  async dispose() {
    await Promise.all([...this.#instances.keys()].map((k) => this.#evictKey(k)));
  }
}
