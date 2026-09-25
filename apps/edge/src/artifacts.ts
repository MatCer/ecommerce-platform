import { mkdir, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { type ArtifactManifest, artifactId, sha256, validateManifest } from "@platform/theme-kit";
import type { Upstream } from "./bindings.ts";

const ID_RE = /^[0-9a-f]{32}$/;
// Plain relative segments only: no `..`, no empty segments, no backslashes.
const REL_RE = /^[A-Za-z0-9._@+~-]+(\/[A-Za-z0-9._@+~-]+)*$/;
const MAX_ARTIFACT_BYTES = 50 * 1024 * 1024; // A6
const MAX_MANIFEST_BYTES = 2 * 1024 * 1024;
const MAX_FILES = 5000;
const CONCURRENCY = 8;
/** One artifact download, all files included, must finish within this. */
const DOWNLOAD_DEADLINE_MS = 60_000;

const safeRel = (p: string) => REL_RE.test(p) && !p.split("/").some((s) => s === "." || s === "..");

/**
 * Loads theme/checkout artifacts on demand (spec §9.3.3, A22): the files come from the private
 * bucket through `GET /internal/v1/artifacts/{id}/{path}` (service token) and are unpacked
 * under `<root>/<id>/`. Before anything is written in place, the content address is recomputed
 * from the downloaded bytes; a tampered, incomplete or mislabelled artifact is refused.
 * Present directories (baked in, or downloaded earlier) are used as they are.
 */
export class ArtifactFetcher {
  readonly #root: string;
  readonly #apiOrigin: string;
  readonly #token: string;
  readonly #upstream: Upstream;
  readonly #inflight = new Map<string, Promise<void>>();

  constructor(opts: { root: string; apiOrigin: string; token: string; upstream?: Upstream }) {
    this.#root = opts.root;
    this.#apiOrigin = opts.apiOrigin;
    this.#token = opts.token;
    this.#upstream = opts.upstream ?? ((r) => fetch(r));
  }

  async ensure(id: string): Promise<void> {
    if (!ID_RE.test(id)) throw new Error(`artifact: invalid id ${JSON.stringify(id)}`);
    if (await present(path.join(this.#root, id, "manifest.json"))) return;
    let p = this.#inflight.get(id);
    if (!p) {
      p = this.#download(id).finally(() => this.#inflight.delete(id));
      this.#inflight.set(id, p);
    }
    return p;
  }

  /** Streams one file into memory against the download's shared byte budget. */
  async #get(
    id: string,
    rel: string,
    budget: { left: number },
    signal: AbortSignal,
  ): Promise<Uint8Array> {
    const res = await this.#upstream(
      new Request(`${this.#apiOrigin}/internal/v1/artifacts/${id}/${rel}`, {
        headers: { authorization: `Bearer ${this.#token}` },
        signal,
      }),
    );
    if (!res.ok || !res.body) throw new Error(`artifact ${id}: ${rel} → ${res.status}`);
    const chunks: Uint8Array[] = [];
    let size = 0;
    const reader = res.body.getReader();
    try {
      for (;;) {
        if (signal.aborted) throw new Error(`artifact ${id}: download timed out`);
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        budget.left -= value.byteLength;
        if (budget.left < 0) throw new Error(`artifact ${id}: larger than the size limit`);
        chunks.push(value);
      }
    } catch (err) {
      await reader.cancel().catch(() => {});
      throw err;
    }
    const out = new Uint8Array(size);
    let offset = 0;
    for (const c of chunks) {
      out.set(c, offset);
      offset += c.byteLength;
    }
    return out;
  }

  async #download(id: string): Promise<void> {
    const signal = AbortSignal.timeout(DOWNLOAD_DEADLINE_MS);
    const manifestBytes = await this.#get(
      id,
      "manifest.json",
      { left: MAX_MANIFEST_BYTES },
      signal,
    );
    const m = validateManifest(
      JSON.parse(new TextDecoder().decode(manifestBytes)) as ArtifactManifest,
      id,
    );
    if (m.runtime.modules.length + Object.keys(m.assets ?? {}).length > MAX_FILES)
      throw new Error(`artifact ${id}: too many files`);
    const files: string[] = [];
    for (const mod of m.runtime.modules) {
      if (!safeRel(mod)) throw new Error(`artifact ${id}: bad module path ${mod}`);
      files.push(`server/${mod}`);
    }
    for (const p of Object.keys(m.assets)) {
      if (!p.startsWith("/") || !safeRel(p.slice(1)))
        throw new Error(`artifact ${id}: bad asset path ${p}`);
      files.push(`client${p}`);
    }

    const bodies = new Map<string, Uint8Array>();
    // One budget for every file: the limit holds while the downloads are in flight.
    const budget = { left: MAX_ARTIFACT_BYTES };
    for (let i = 0; i < files.length; i += CONCURRENCY) {
      const batch = files.slice(i, i + CONCURRENCY);
      const got = await Promise.all(batch.map((f) => this.#get(id, f, budget, signal)));
      batch.forEach((f, k) => {
        bodies.set(f, got[k] as Uint8Array);
      });
    }

    // Every byte must match the content address before the artifact can run.
    for (const [p, entry] of Object.entries(m.assets)) {
      const body = bodies.get(`client${p}`);
      if (!body || sha256(body) !== entry.sha256 || body.byteLength !== entry.size)
        throw new Error(`artifact ${id}: ${p} does not match the manifest`);
    }
    const server = m.runtime.modules.map((mod): [string, string] => [
      mod,
      sha256(bodies.get(`server/${mod}`) ?? new Uint8Array()),
    ]);
    const computed = artifactId({
      kind: m.kind,
      server,
      assets: m.assets,
      tokens: m.tokens,
      csp: m.csp,
    });
    if (computed !== id) throw new Error(`artifact ${id}: content address mismatch`);

    await mkdir(this.#root, { recursive: true });
    const tmp = path.join(this.#root, `.dl-${id}-${process.pid}-${Date.now()}`);
    await rm(tmp, { recursive: true, force: true });
    for (const [rel, body] of bodies) {
      await mkdir(path.dirname(path.join(tmp, rel)), { recursive: true });
      await writeFile(path.join(tmp, rel), body);
    }
    await writeFile(path.join(tmp, "manifest.json"), manifestBytes);
    await rename(tmp, path.join(this.#root, id)).catch(async (err: unknown) => {
      // Another process may have won the race with the same (verified) content.
      await rm(tmp, { recursive: true, force: true });
      if (!(await present(path.join(this.#root, id, "manifest.json")))) throw err;
    });
  }
}

async function present(p: string) {
  try {
    return (await stat(p)).isFile();
  } catch {
    return false;
  }
}
