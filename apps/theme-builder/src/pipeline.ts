/**
 * One revision through the gates (WP23, spec §12.3, A6): static checks → build → artifact →
 * browser checks on the preview → functional checks → `ready` or `failed` with a report.
 *
 * Every step runs in its own disposable sandbox container (`docker.ts`, `sandbox.ts`). What
 * the builder trusts: container exit codes; the static and check steps' reports (no theme
 * code runs there); the artifact only after its content address is recomputed from the files
 * (`verifyArtifact`). Step outputs are read without following symlinks.
 */
import { spawn } from "node:child_process";
import { lstat, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { verifyArtifact } from "@platform/theme-kit";
import { judge, type PageResult } from "@platform/theme-kit/budget";
import type { Docker, SandboxSpec, StepResult } from "./docker.ts";

export const SCREENSHOTS = [
  "home-mobile",
  "home-desktop",
  "category-mobile",
  "category-desktop",
  "product-mobile",
  "product-desktop",
] as const;

export interface BuildSpec {
  tenant_id: string;
  revision_id: string;
  number: number;
  change: string;
  status: string;
  tokens_only: boolean;
  astro_key_hex: string;
}

export interface CheckSpec {
  artifact_id: string;
  preview_host: string;
  preview_token: string;
  pages: string[];
}

/** The platform API as the builder sees it (`/internal/v1/themes/*`, builder token). */
export interface Api {
  spec(tenant: string, revision: string): Promise<BuildSpec>;
  source(tenant: string, revision: string): Promise<Buffer>;
  status(tenant: string, revision: string, status: string, checks: Report): Promise<void>;
  artifact(tenant: string, revision: string, tar: Buffer): Promise<CheckSpec>;
  screenshot(tenant: string, revision: string, name: string, png: Buffer): Promise<void>;
}

export interface StepReport {
  name: string;
  status: "passed" | "failed" | "skipped";
  ms?: number;
  [detail: string]: unknown;
}

export interface Report {
  pipeline: "full" | "tokens";
  started_at: string;
  finished_at?: string;
  sandbox: Record<string, unknown>;
  steps: StepReport[];
  failures: string[];
  artifact_id?: string;
  screenshots?: string[];
}

export interface PipelineConfig {
  docker: Pick<Docker, "run">;
  api: Api;
  /** The work volume as mounted in the builder (`/work`). */
  workDir: string;
  /** Internal network for the browser steps (reaches only the public proxy). */
  checkNetwork: string;
  /** Separate network for merchant-authored Node checks; only the preview proxy is attached. */
  functionalNetwork: string;
  /** Host the preview hosts are mapped to inside the check network (`caddy`). */
  proxyHost: string;
  functionalProxyHost: string;
  lighthouseRuns: number;
  log?: (e: Record<string, unknown>) => void;
}

const GiB = 1024 ** 3;
const SANDBOX = "/repo/apps/theme-builder/src/sandbox.ts";
const RESULT = /^@@result (.+)$/m;

/** The last `@@result` line of a step's stdout. */
export function parseResult(stdout: string): Record<string, unknown> | null {
  const lines = stdout.split("\n").filter((l) => l.startsWith("@@result "));
  const last = lines.at(-1);
  if (!last) return null;
  try {
    const v: unknown = JSON.parse(last.replace(RESULT, "$1"));
    return typeof v === "object" && v !== null && !Array.isArray(v)
      ? (v as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

const tail = (r: StepResult, n = 1500) =>
  `${r.stderr}\n${r.stdout.replace(/^@@result .*$/gm, "")}`.trim().slice(-n);

/**
 * The readable part of a failed step's log: the first error line and what follows it, without
 * stack frames (the full tail stays in the step's report).
 */
export function errorSummary(log: string, max = 1200): string {
  const lines = log.split("\n").filter((l) => !/^\s+at /.test(l));
  const i = lines.findIndex((l) => /error/i.test(l));
  return (i < 0 ? lines.slice(-12) : lines.slice(i, i + 8)).join("\n").trim().slice(0, max);
}

/**
 * Reads `rel` under `root` only if every component is a real directory and the file a regular
 * file of at most `max` bytes: a sandbox cannot make the builder read through a symlink.
 */
export async function safeRead(root: string, rel: string, max: number): Promise<Buffer | null> {
  const parts = rel.split("/");
  let p = root;
  for (const [i, part] of parts.entries()) {
    if (!part || part === "." || part === "..") return null;
    p = path.join(p, part);
    const st = await lstat(p).catch(() => null);
    if (!st) return null;
    const last = i === parts.length - 1;
    if (last ? !st.isFile() || st.size > max : !st.isDirectory()) return null;
  }
  return readFile(p);
}

/**
 * Walks an output tree without following anything: only directories and regular files, at
 * most `maxFiles` files and `maxBytes` in total. Run before any file of it is opened (a FIFO
 * would block a read, a huge file would exhaust memory).
 */
export async function checkTree(root: string, maxBytes: number, maxFiles: number): Promise<void> {
  let bytes = 0;
  let files = 0;
  const walk = async (dir: string) => {
    for (const name of await readdir(dir)) {
      const p = path.join(dir, name);
      const st = await lstat(p);
      if (st.isDirectory()) await walk(p);
      else if (st.isFile()) {
        bytes += st.size;
        files += 1;
        if (bytes > maxBytes || files > maxFiles)
          throw new Error("the build output is larger than allowed");
      } else throw new Error(`the build output contains a special file or link: ${name}`);
    }
  };
  if (!(await realDir(root))) throw new Error("the build output is not a plain directory");
  await walk(root);
}

async function realDir(p: string): Promise<boolean> {
  const st = await lstat(p).catch(() => null);
  return st?.isDirectory() ?? false;
}

function tarDir(dir: string): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const child = spawn("tar", ["-cf", "-", "-C", dir, "."], { stdio: ["ignore", "pipe", "pipe"] });
    const out: Buffer[] = [];
    let err = "";
    child.stdout.on("data", (c: Buffer) => out.push(c));
    child.stderr.on("data", (c: Buffer) => {
      err += c.toString();
    });
    child.on("close", (code) =>
      code === 0 ? resolve(Buffer.concat(out)) : reject(new Error(`tar: ${err}`)),
    );
  });
}

export async function runPipeline(cfg: PipelineConfig, tenant: string, revision: string) {
  const log = cfg.log ?? ((e) => console.log(JSON.stringify(e)));
  const spec = await cfg.api.spec(tenant, revision);
  if (spec.status !== "draft" && spec.status !== "building") {
    log({ level: "info", msg: "revision already handled", revision, status: spec.status });
    return;
  }
  const tokensOnly = spec.tokens_only;
  const report: Report = {
    pipeline: tokensOnly ? "tokens" : "full",
    started_at: new Date().toISOString(),
    sandbox: {
      image_steps: ["static", "build", "check", "functional"],
      build_network: "none",
      check_network: "internal (public proxy only)",
      read_only_rootfs: true,
      user: "non-root",
      cap_drop: "ALL",
      credentials: "none (ASTRO_KEY only)",
    },
    steps: [],
    failures: [],
  };
  let status: "building" | "checking" = "building";
  await cfg.api.status(tenant, revision, "building", report);

  const job = revision.toLowerCase();
  const dir = path.join(cfg.workDir, job);
  await rm(dir, { recursive: true, force: true });
  for (const sub of ["in", "out/build", "out/check"])
    await mkdir(path.join(dir, sub), { recursive: true });
  await writeFile(path.join(dir, "in/source.tar.gz"), await cfg.api.source(tenant, revision));

  const step = (s: Omit<SandboxSpec, "job" | "cmd">) =>
    cfg.docker.run({ ...s, job, cmd: ["node", SANDBOX, s.step] });
  const fail = (m: string) => report.failures.push(m);
  const inMount = { target: "/in" as const, subpath: `${job}/in` };

  try {
    // 1. Static: archive re-check, contract lint, types. No theme code runs.
    const st = await step({
      step: "static",
      env: { TOKENS_ONLY: tokensOnly ? "1" : "0", HOME: "/tmp" },
      network: "none",
      mounts: [inMount],
      memoryBytes: 2 * GiB,
      cpus: 2,
      pids: 256,
      timeoutMs: 240_000,
    });
    const sr = parseResult(st.stdout);
    const violations = (Array.isArray(sr?.violations) ? sr.violations : []) as {
      file: string;
      line: number;
      rule: string;
      message: string;
    }[];
    const functional = (Array.isArray(sr?.checks) ? sr.checks : []) as string[];
    report.steps.push({
      name: "lint",
      status: sr && !sr.error && violations.length === 0 ? "passed" : "failed",
      ms: st.ms,
      violations,
    });
    for (const v of violations)
      fail(`lint: ${v.file}${v.line ? `:${v.line}` : ""} ${v.rule}: ${v.message}`);
    if (!sr || sr.error || st.timedOut)
      fail(`static checks: ${st.timedOut ? "timed out" : String(sr?.error ?? tail(st))}`);
    const tc = sr?.typecheck as { ok: boolean; ms: number; log: string } | null | undefined;
    report.steps.push(
      tc
        ? {
            name: "typecheck",
            status: tc.ok ? "passed" : "failed",
            ms: tc.ms,
            log: tc.ok ? "" : tc.log,
          }
        : { name: "typecheck", status: "skipped" },
    );
    if (tc && !tc.ok) fail(`types (astro check): ${tc.log.slice(-800)}`);
    if (report.failures.length) return;

    // 2. Build + pack, no network. Only the exit code and the verified artifact count.
    const b = await step({
      step: "build",
      env: {
        ASTRO_KEY: Buffer.from(spec.astro_key_hex, "hex").toString("base64"),
        HOME: "/tmp",
        NODE_OPTIONS: "--max-old-space-size=2048",
      },
      network: "none",
      mounts: [inMount, { target: "/out", subpath: `${job}/out/build` }],
      memoryBytes: 3 * GiB,
      cpus: 2,
      pids: 512,
      timeoutMs: 300_000,
    });
    const br = parseResult(b.stdout);
    const id = typeof br?.artifact_id === "string" ? br.artifact_id : "";
    if (b.exitCode !== 0 || b.timedOut || !/^[0-9a-f]{32}$/.test(id)) {
      report.steps.push({ name: "build", status: "failed", ms: b.ms, log: tail(b, 4000) });
      fail(`build: ${b.timedOut ? "timed out" : errorSummary(tail(b, 20_000))}`);
      return;
    }
    const root = path.join(dir, "out/build/artifacts");
    if (
      !(await realDir(path.join(dir, "out/build"))) ||
      !(await realDir(root)) ||
      !(await realDir(path.join(root, id)))
    )
      throw new Error("the build output is not a plain directory");
    await checkTree(path.join(root, id), 52 * 1024 * 1024, 20_000);
    const manifest = await verifyArtifact(root, id);
    report.steps.push({
      name: "build",
      status: "passed",
      ms: b.ms,
      artifact_id: id,
      assets: Object.keys(manifest.assets).length,
      modules: manifest.runtime.modules.length,
    });
    report.artifact_id = id;
    const check = await cfg.api.artifact(tenant, revision, await tarDir(path.join(root, id)));
    status = "checking";

    // 3. Budgets, axe, smoke, screenshots against the preview (platform code only).
    const c = await step({
      step: "check",
      env: {
        PREVIEW_BASE: `https://${check.preview_host}`,
        PREVIEW_COOKIE: `__Host-preview=${check.preview_token}`,
        PAGES: check.pages.join(","),
        TOKENS_ONLY: tokensOnly ? "1" : "0",
        LIGHTHOUSE_RUNS: String(cfg.lighthouseRuns),
        THEME_KIT_CHROMIUM_ARGS: JSON.stringify([
          `--host-resolver-rules=MAP *.localhost ${cfg.proxyHost}`,
        ]),
        HOME: "/tmp",
      },
      network: cfg.checkNetwork,
      mounts: [{ target: "/out", subpath: `${job}/out/check` }],
      memoryBytes: 3 * GiB,
      cpus: 2,
      pids: 1024,
      timeoutMs: 600_000,
    });
    const out = path.join(dir, "out/check");
    // Only a check step that completed counts; each page must have been measured.
    if (c.exitCode !== 0 || c.timedOut)
      fail(`checks did not complete: ${c.timedOut ? "timed out" : errorSummary(tail(c, 20_000))}`);
    const measured = await safeRead(out, "report.json", 8 * 1024 * 1024);
    const results = measured
      ? ((JSON.parse(measured.toString("utf8")) as { results?: PageResult[] }).results ?? [])
      : [];
    const pages = results.map((r) => {
      const failures = judge(tokensOnly ? { ...r, lcpMs: 0, tbtMs: 0, cls: 0 } : r);
      for (const f of failures) fail(`budget ${r.path}: ${f}`);
      return {
        path: r.path,
        kind: r.kind,
        lcp_ms: tokensOnly ? null : Math.round(r.lcpMs),
        tbt_ms: tokensOnly ? null : Math.round(r.tbtMs),
        cls: tokensOnly ? null : r.cls,
        js_gzip: r.jsGzip,
        js_gzip_with_rum: r.jsGzipWithRum,
        calls: r.subrequests,
        third_party: r.thirdPartyOrigins,
        axe: r.axe.filter((v) => v.impact === "serious" || v.impact === "critical"),
        csp_violations: r.cspViolations.length,
        failures,
      };
    });
    const measuredPaths = new Set(results.map((r) => r.path));
    for (const p of check.pages) if (!measuredPaths.has(p)) fail(`budget: ${p} was not measured`);
    report.steps.push({
      name: "budget",
      status:
        results.length &&
        check.pages.every((p) => measuredPaths.has(p)) &&
        pages.every((p) => p.failures.length === 0)
          ? "passed"
          : "failed",
      ms: c.ms,
      lighthouse: !tokensOnly,
      pages,
    });
    const summary = await safeRead(out, "result.json", 64 * 1024);
    const parsed = summary
      ? (JSON.parse(summary.toString("utf8")) as {
          measure?: { ok: boolean; log: string };
          smoke?: { ok: boolean; log: string };
        })
      : {};
    const smoke = parsed.smoke ?? null;
    if (!parsed.measure?.ok)
      fail(
        `budget: the measurement did not finish (${parsed.measure?.log.slice(-400) ?? "no report"})`,
      );
    if (tokensOnly) {
      report.steps.push({ name: "smoke", status: "skipped" });
      if (!smoke?.ok) fail(`screenshots: ${smoke?.log.slice(-400) ?? "did not run"}`);
    } else {
      report.steps.push({
        name: "smoke",
        status: smoke?.ok ? "passed" : "failed",
        log: smoke?.ok ? "" : smoke?.log,
      });
      if (!smoke?.ok)
        fail(`smoke (browse → cart → checkout): ${smoke?.log.slice(-600) ?? "did not run"}`);
    }
    report.screenshots = [];
    const expected = SCREENSHOTS.slice(0, Math.min(check.pages.length, 3) * 2);
    for (const name of SCREENSHOTS) {
      const png = await safeRead(out, `shots/${name}.png`, 5 * 1024 * 1024);
      if (!png) continue;
      await cfg.api.screenshot(tenant, revision, name, png);
      report.screenshots.push(name);
    }
    const missing = expected.filter((n) => !report.screenshots?.includes(n));
    if (missing.length) fail(`screenshots missing: ${missing.join(", ")}`);

    // 4. The revision's own functional checks (untrusted code: exit code only).
    if (functional.length) {
      const f = await step({
        step: "functional",
        env: {
          PREVIEW_BASE: `https://${check.preview_host}`,
          PREVIEW_COOKIE: `__Host-preview=${check.preview_token}`,
          THEME_KIT_CHROMIUM_ARGS: JSON.stringify([
            `--host-resolver-rules=MAP *.localhost ${cfg.functionalProxyHost}`,
          ]),
          HOME: "/tmp",
        },
        network: cfg.functionalNetwork,
        mounts: [inMount],
        memoryBytes: 2 * GiB,
        cpus: 2,
        pids: 1024,
        timeoutMs: 240_000,
      });
      const ok = f.exitCode === 0 && !f.timedOut;
      report.steps.push({
        name: "functional",
        status: ok ? "passed" : "failed",
        ms: f.ms,
        checks: functional,
        log: ok ? "" : tail(f, 3000),
      });
      if (!ok) fail(`functional checks: ${f.timedOut ? "timed out" : tail(f, 800)}`);
    } else report.steps.push({ name: "functional", status: "skipped" });
  } catch (err) {
    fail(`builder: ${err instanceof Error ? err.message : String(err)}`);
  } finally {
    report.finished_at = new Date().toISOString();
    const final = report.failures.length || status === "building" ? "failed" : "ready";
    await cfg.api
      .status(tenant, revision, final, report)
      .catch((err) =>
        log({ level: "error", msg: "status callback failed", revision, err: String(err) }),
      );
    await rm(dir, { recursive: true, force: true }).catch(() => {});
    log({
      level: "info",
      msg: "theme build finished",
      revision,
      status: final,
      failures: report.failures.length,
    });
  }
}
