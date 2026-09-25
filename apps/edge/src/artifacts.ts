import { mkdir, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { type ArtifactManifest, artifactId, sha256 } from "@platform/theme-kit";
import type { Upstream } from "./bindings.ts";

const ID_RE = /^[0-9a-f]{32}$/;
// Plain relative segments only: no `..`, no empty segments, no backslashes.
const REL_RE = /^[A-Za-z0-9._@+~-]+(\/[A-Za-z0-9._@+~-]+)*$/;
const MAX_ARTIFACT_BYTES = 50 * 1024 * 1024; // A6
const CONCURRENCY = 8;

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

  async #get(id: string, rel: string): Promise<Uint8Array> {
    const res = await this.#upstream(
      new Request(`${this.#apiOrigin}/internal/v1/artifacts/${id}/${rel}`, {
        headers: { authorization: `Bearer ${this.#token}` },
      }),
    );
    if (!res.ok) throw new Error(`artifact ${id}: ${rel} → ${res.status}`);
    return new Uint8Array(await res.arrayBuffer());
  }

  async #download(id: string): Promise<void> {
    const manifestBytes = await this.#get(id, "manifest.json");
    const m = JSON.parse(new TextDecoder().decode(manifestBytes)) as ArtifactManifest;
    if (m.schema !== 1 || m.id !== id) throw new Error(`artifact ${id}: manifest mismatch`);
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
    let total = manifestBytes.byteLength;
    for (let i = 0; i < files.length; i += CONCURRENCY) {
      const batch = files.slice(i, i + CONCURRENCY);
      const got = await Promise.all(batch.map((f) => this.#get(id, f)));
      batch.forEach((f, k) => {
        const body = got[k] as Uint8Array;
        total += body.byteLength;
        bodies.set(f, body);
      });
      if (total > MAX_ARTIFACT_BYTES) throw new Error(`artifact ${id}: larger than 50 MB`);
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
