import { createHash } from "node:crypto";
import { lstat, mkdir, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { type ThemeTokens, validateTokens } from "./tokens.ts";

/**
 * Runtime contract (spec A22): the platform, not the theme, decides the workerd compatibility
 * date and flags. Themes cannot change them (their wrangler config is ignored).
 * Bump together with the pinned workerd/miniflare versions after re-running the WP2 checks.
 */
export const RUNTIME = {
  compatibility_date: "2026-09-21",
  compatibility_flags: [] as string[],
} as const;

export type ArtifactKind = "theme" | "checkout";

export interface AssetEntry {
  sha256: string;
  size: number;
}

/** `manifest.json` at the root of an unpacked artifact directory `<root>/<id>/`. */
export interface ArtifactManifest {
  schema: 1;
  /** Content address: sha256 over every file path + content hash. Immutable. */
  id: string;
  kind: ArtifactKind;
  runtime: {
    compatibility_date: string;
    compatibility_flags: string[];
    /** Entry module, relative to `server/`. Always the first module. */
    main: string;
    /** Every module workerd may load, relative to `server/`. Nothing else is loadable. */
    modules: string[];
  };
  /** URL path (e.g. `/_astro/app.X1.js`, `/favicon.svg`) → file under `client/`. */
  assets: Record<string, AssetEntry>;
  /** Design tokens (theme.tokens.json, spec A6), used by the checkout origin. */
  tokens: ThemeTokens | null;
  /** CSP hashes of the only inline code allowed: Astro's island bootstrap (spec A26). */
  csp: { script_hashes: string[]; style_hashes: string[] };
}

// Adapter/Cloudflare control files that must never be served as assets.
const CLIENT_IGNORE = new Set(["_headers", "_redirects", "_routes.json", ".assetsignore"]);
const MAX_ARTIFACT_BYTES = 50 * 1024 * 1024; // spec A6: expanded size ≤ 50 MB

const cspHash = (text: string) => `sha256-${createHash("sha256").update(text).digest("base64")}`;

/**
 * Astro inlines its island bootstrap (`<script>` per client directive + the `astro-island`
 * element, one `<style>`). The strings ship prebuilt with the pinned Astro version, and themes
 * cannot register custom directives (astro.config is platform-owned), so hashing them here
 * covers every inline script a theme can legitimately emit.
 */
export async function astroInlineHashes(projectDir: string) {
  const require = createRequire(path.join(path.resolve(projectDir), "package.json"));
  const load = async (spec: string) =>
    (await import(pathToFileURL(require.resolve(spec)).href)) as Record<string, unknown>;
  const scripts: string[] = [];
  for (const d of ["idle", "load", "visible", "media", "only"]) {
    scripts.push(String((await load(`astro/runtime/client/${d}.prebuilt.js`)).default));
  }
  scripts.push(String((await load("astro/runtime/server/astro-island.prebuilt.js")).default));
  // @astrojs/solid-js adds Solid's event-replay bootstrap once per page.
  const solid = (await load("solid-js/web")) as { generateHydrationScript: () => string };
  const hy = /<script[^>]*>([\s\S]*?)<\/script>/.exec(solid.generateHydrationScript())?.[1];
  if (!hy) throw new Error("artifact: cannot extract Solid's hydration script");
  scripts.push(hy);
  const styles = [
    String((await load("astro/runtime/server/astro-island-styles.js")).ISLAND_STYLES),
  ];
  return { script_hashes: scripts.map(cspHash), style_hashes: styles.map(cspHash) };
}

const sha256 = (data: string | Uint8Array) => createHash("sha256").update(data).digest("hex");

/** Lists regular files under `dir` (relative, posix). Rejects symlinks and special files. */
async function listFiles(dir: string, rel = ""): Promise<string[]> {
  const out: string[] = [];
  for (const entry of await readdir(path.join(dir, rel), { withFileTypes: true })) {
    const relPath = rel ? `${rel}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...(await listFiles(dir, relPath)));
    else if (entry.isFile()) out.push(relPath);
    else throw new Error(`artifact: unsupported file type (symlink?): ${relPath}`);
  }
  return out.sort();
}

/**
 * Packs an Astro Cloudflare build (`dist/server` + `dist/client`) into an immutable,
 * content-addressed artifact directory `<outRoot>/<id>/`. Returns the manifest.
 */
export async function packArtifact(opts: {
  dist: string;
  outRoot: string;
  kind: ArtifactKind;
  tokensFile?: string;
  /** Project that produced `dist` (resolves the pinned Astro). Defaults to `dist/..`. */
  projectDir?: string;
}): Promise<ArtifactManifest> {
  const serverDir = path.join(opts.dist, "server");
  const clientDir = path.join(opts.dist, "client");
  // listFiles() refuses symlinked entries; the roots must be real directories too, or a build
  // could point dist/client at any readable directory and publish its files.
  for (const dir of [opts.dist, serverDir, clientDir]) {
    const st = await lstat(dir);
    if (!st.isDirectory())
      throw new Error(`artifact: ${dir} must be a real directory (no symlinks)`);
  }

  const serverFiles = (await listFiles(serverDir)).filter((f) => /\.(m?js)$/.test(f));
  if (!serverFiles.includes("entry.mjs"))
    throw new Error("artifact: dist/server/entry.mjs missing");
  const clientFiles = (await listFiles(clientDir)).filter((f) => !CLIENT_IGNORE.has(f));

  let total = 0;
  const digest = createHash("sha256");
  // Read each file once and publish exactly the bytes that were hashed (no re-read races).
  const contents = new Map<string, Buffer>();
  const hashFile = async (abs: string, label: string) => {
    const buf = await readFile(abs);
    total += buf.byteLength;
    if (total > MAX_ARTIFACT_BYTES) throw new Error("artifact: larger than 50 MB");
    contents.set(label, buf);
    const h = sha256(buf);
    digest.update(`${label}\0${h}\n`);
    return { sha256: h, size: buf.byteLength };
  };

  for (const f of serverFiles) await hashFile(path.join(serverDir, f), `server/${f}`);
  const assets: Record<string, AssetEntry> = {};
  for (const f of clientFiles)
    assets[`/${f}`] = await hashFile(path.join(clientDir, f), `client/${f}`);

  const tokens = opts.tokensFile
    ? validateTokens(JSON.parse(await readFile(opts.tokensFile, "utf8")))
    : null;
  const csp = await astroInlineHashes(opts.projectDir ?? path.dirname(path.resolve(opts.dist)));
  digest.update(`tokens\0${JSON.stringify(tokens)}\ncsp\0${JSON.stringify(csp)}\n`);
  digest.update(`runtime\0${JSON.stringify(RUNTIME)}\nkind\0${opts.kind}\n`);
  const id = digest.digest("hex").slice(0, 32);

  const manifest: ArtifactManifest = {
    schema: 1,
    id,
    kind: opts.kind,
    runtime: {
      ...RUNTIME,
      compatibility_flags: [...RUNTIME.compatibility_flags],
      main: "entry.mjs",
      modules: ["entry.mjs", ...serverFiles.filter((f) => f !== "entry.mjs")],
    },
    assets,
    tokens,
    csp,
  };

  const finalDir = path.join(opts.outRoot, id);
  if (await exists(finalDir)) return manifest; // content-addressed: identical build, nothing to do
  const tmp = path.join(opts.outRoot, `.tmp-${id}-${process.pid}`);
  await rm(tmp, { recursive: true, force: true });
  for (const [label, buf] of contents) {
    await mkdir(path.dirname(path.join(tmp, label)), { recursive: true });
    await writeFile(path.join(tmp, label), buf);
  }
  await writeFile(path.join(tmp, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  await rename(tmp, finalDir); // atomic publish of the directory
  return manifest;
}

async function exists(p: string) {
  try {
    await lstat(p);
    return true;
  } catch {
    return false;
  }
}

const ID_RE = /^[0-9a-f]{32}$/;

/** Reads and validates `<root>/<id>/manifest.json`. The id must be a content address. */
export async function readManifest(root: string, id: string): Promise<ArtifactManifest> {
  if (!ID_RE.test(id)) throw new Error(`artifact: invalid id ${JSON.stringify(id)}`);
  const m = JSON.parse(
    await readFile(path.join(root, id, "manifest.json"), "utf8"),
  ) as ArtifactManifest;
  if (m.schema !== 1 || m.id !== id) throw new Error(`artifact ${id}: manifest mismatch`);
  for (const mod of m.runtime.modules) {
    if (mod.startsWith("/") || mod.split("/").includes("..")) {
      throw new Error(`artifact ${id}: bad module path ${mod}`);
    }
  }
  return m;
}
