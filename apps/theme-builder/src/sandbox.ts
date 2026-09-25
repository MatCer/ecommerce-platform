#!/usr/bin/env node
/**
 * Runs inside a disposable sandbox container (WP23, A6): `node sandbox.ts <step>`.
 *
 *   static      unpack + validate the source, contract lint, `astro check` (unless token-only).
 *               No theme code runs here: the lint forbids content collections, the only thing
 *               `astro check` would execute.                             --network none
 *   build       unpack, platform files overlaid, `astro build` (ASTRO_KEY), `theme-kit pack`
 *               into /out/artifacts. Theme code may run (prerendered pages).  --network none
 *   check       budgets + axe (`theme-kit measure`), smoke + screenshots against the preview
 *               (PREVIEW_BASE + PREVIEW_COOKIE, PAGES). Platform code only.   internal network
 *   functional  the revision's own `checks/*.spec.ts` (Playwright) against the preview.
 *
 * The source arrives read-only at /in/source.tar.gz; /work and /tmp are tmpfs; node_modules are
 * the image's (read-only). One `@@result {json}` line on stdout reports the outcome; the builder
 * trusts it only for steps where no theme code ran (static, check).
 */
import { spawn } from "node:child_process";
import { copyFile, lstat, mkdir, readdir, symlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { lintTheme, type Violation } from "@platform/theme-kit/lint";

const REPO = path.resolve(import.meta.dirname, "../../..");
const PLATFORM_THEME = path.join(REPO, "themes/default");
const KIT = path.join(REPO, "packages/theme-kit/src");
const THEME = "/work/theme";
const MAX_BYTES = 50 * 1024 * 1024;
const PLATFORM_FILES = ["package.json", "astro.config.mjs", "tsconfig.json"];

function result(value: unknown): void {
  process.stdout.write(`\n@@result ${JSON.stringify(value)}\n`);
}

/** Runs a command, streaming its output, resolving with the exit code and the output tail. */
function run(cmd: string, args: string[], opts: { cwd?: string; env?: NodeJS.ProcessEnv } = {}) {
  return new Promise<{ code: number; tail: string }>((resolve) => {
    const child = spawn(cmd, args, {
      cwd: opts.cwd ?? THEME,
      env: { ...process.env, ...opts.env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let tail = "";
    const keep = (chunk: Buffer, to: NodeJS.WriteStream) => {
      to.write(chunk);
      tail = (tail + chunk.toString("utf8")).slice(-6000);
    };
    child.stdout.on("data", (c: Buffer) => keep(c, process.stdout));
    child.stderr.on("data", (c: Buffer) => keep(c, process.stderr));
    child.on("close", (code) => resolve({ code: code ?? 1, tail }));
  });
}

/** Unpacks the source and re-checks the tree (A6, defense in depth: the API validated it). */
async function unpack(): Promise<string[]> {
  await mkdir(THEME, { recursive: true });
  const tar = await run(
    "tar",
    ["-xzf", "/in/source.tar.gz", "-C", THEME, "--no-same-owner", "--no-same-permissions"],
    { cwd: "/work" },
  );
  if (tar.code !== 0) throw new Error(`cannot unpack the source: ${tar.tail}`);
  let total = 0;
  const files: string[] = [];
  const walk = async (rel: string) => {
    for (const e of await readdir(path.join(THEME, rel), { withFileTypes: true })) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      const st = await lstat(path.join(THEME, r));
      if (st.isDirectory()) await walk(r);
      else if (st.isFile()) {
        total += st.size;
        files.push(r);
      } else throw new Error(`${r}: only regular files are allowed`);
      if (total > MAX_BYTES) throw new Error("the source is larger than 50 MB");
    }
  };
  await walk("");
  return files;
}

/**
 * The platform's locked files and dependencies. `node_modules` is a writable directory of
 * absolute links into the image's read-only packages: tools can create their caches next to
 * them (`.vite`, `.astro`) but cannot change a dependency.
 */
async function overlay() {
  for (const f of PLATFORM_FILES)
    await copyFile(path.join(PLATFORM_THEME, f), path.join(THEME, f));
  const src = path.join(PLATFORM_THEME, "node_modules");
  const dst = path.join(THEME, "node_modules");
  await mkdir(dst);
  for (const e of await readdir(src, { withFileTypes: true })) {
    if (e.name.startsWith("@") && e.isDirectory()) {
      await mkdir(path.join(dst, e.name));
      for (const child of await readdir(path.join(src, e.name)))
        await symlink(path.join(src, e.name, child), path.join(dst, e.name, child));
    } else await symlink(path.join(src, e.name), path.join(dst, e.name));
  }
}

const ASTRO = path.join(PLATFORM_THEME, "node_modules/astro/bin/astro.mjs");

/** Build-time code the contract does not allow: content collections run during `astro check`. */
function buildTimeCode(files: string[]): Violation[] {
  return files
    .filter((f) => /^src\/(content\.config|content\/config)\.[cm]?[jt]s$/.test(f))
    .map((f) => ({
      file: f,
      line: 0,
      rule: "content-config",
      message: "content collections are not part of the theme contract (data comes from the SDK)",
    }));
}

async function staticStep() {
  const files = await unpack();
  const violations = [
    ...(await lintTheme(THEME, { referenceDir: PLATFORM_THEME })),
    ...buildTimeCode(files),
  ];
  const checks = files.filter((f) => /^checks\/[A-Za-z0-9_-]+\.spec\.ts$/.test(f));
  if (violations.length || process.env.TOKENS_ONLY === "1") {
    result({ violations, typecheck: null, checks });
    return;
  }
  await overlay();
  const started = Date.now();
  const tc = await run("node", [ASTRO, "check", "--minimumSeverity", "error"]);
  result({
    violations,
    typecheck: { ok: tc.code === 0, ms: Date.now() - started, log: tc.tail.slice(-3000) },
    checks,
  });
}

async function buildStep() {
  await unpack();
  await overlay();
  let started = Date.now();
  const build = await run("node", [ASTRO, "build", "--silent"]);
  const buildMs = Date.now() - started;
  if (build.code !== 0) {
    result({ ok: false, stage: "build", ms: buildMs });
    process.exit(1);
  }
  started = Date.now();
  const packed = await run("node", [
    path.join(KIT, "cli.ts"),
    "pack",
    "--dist",
    path.join(THEME, "dist"),
    "--kind",
    "theme",
    "--tokens",
    path.join(THEME, "theme.tokens.json"),
    "--out",
    "/out/artifacts",
  ]);
  const id = /([0-9a-f]{32})\s*$/.exec(packed.tail)?.[1];
  if (packed.code !== 0 || !id) {
    result({ ok: false, stage: "pack", ms: Date.now() - started });
    process.exit(1);
  }
  result({ ok: true, artifact_id: id, build_ms: buildMs, pack_ms: Date.now() - started });
}

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`missing env ${name}`);
  return v;
}

async function checkStep() {
  const base = required("PREVIEW_BASE");
  const cookie = required("PREVIEW_COOKIE");
  const pages = required("PAGES");
  const tokensOnly = process.env.TOKENS_ONLY === "1";
  const measure = await run(
    "node",
    [
      path.join(KIT, "measure.ts"),
      "--base",
      base,
      "--pages",
      pages,
      "--runs",
      process.env.LIGHTHOUSE_RUNS ?? "3",
      "--cookie",
      cookie,
      "--out",
      "/out/report.json",
      "--no-fail",
      ...(tokensOnly ? ["--skip-lighthouse"] : []),
    ],
    { cwd: "/work" },
  );
  const smoke = await run(
    "node",
    [
      path.join(KIT, "smoke.ts"),
      "--base",
      base,
      "--preview",
      "--cookie",
      cookie,
      "--pages",
      pages,
      "--shots",
      "/out/shots",
      ...(tokensOnly ? ["--screenshots-only"] : []),
    ],
    { cwd: "/work" },
  );
  const summary = {
    measure: { ok: measure.code === 0, log: measure.tail.slice(-2000) },
    smoke: { ok: smoke.code === 0, skipped: tokensOnly, log: smoke.tail.slice(-2000) },
  };
  await writeFile("/out/result.json", JSON.stringify(summary));
  result(summary);
}

async function functionalStep() {
  const files = await unpack();
  if (!files.some((f) => /^checks\/[A-Za-z0-9_-]+\.spec\.ts$/.test(f))) {
    result({ skipped: true });
    return;
  }
  // `@playwright/test` resolves for the checks from /work/node_modules (the image's copy).
  await mkdir("/work/node_modules/@playwright", { recursive: true });
  const pw = path.join(REPO, "apps/theme-builder/node_modules/@playwright/test");
  await symlink(pw, "/work/node_modules/@playwright/test");
  const r = await run(
    "node",
    [
      path.join(pw, "cli.js"),
      "test",
      "--config",
      path.join(REPO, "apps/theme-builder/functional.config.ts"),
    ],
    { cwd: "/work" },
  );
  result({ ok: r.code === 0 });
  process.exit(r.code === 0 ? 0 : 1);
}

const steps: Record<string, () => Promise<void>> = {
  static: staticStep,
  build: buildStep,
  check: checkStep,
  functional: functionalStep,
};
const step = steps[process.argv[2] ?? ""];
if (!step) {
  console.error(`usage: sandbox.ts ${Object.keys(steps).join("|")}`);
  process.exit(2);
}
try {
  await step();
} catch (err) {
  console.error(err instanceof Error ? err.message : String(err));
  result({ error: err instanceof Error ? err.message : String(err) });
  process.exit(1);
}
