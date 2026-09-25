/**
 * What the sandbox proxy lets through to the Docker Engine API (WP23, spec A6). The builder
 * never holds the Docker socket: it talks to `proxy.ts`, which allows exactly the calls needed
 * to run disposable sandbox containers of one image and refuses everything else.
 *
 * `checkCreate` is an allowlist over the `POST /containers/create` body: unknown keys anywhere
 * are refused, and every security-relevant field must be present with a safe value (network,
 * read-only root, dropped capabilities, no-new-privileges, non-root user, bounded memory, CPU
 * and pids, tmpfs work dirs, the one work volume through validated subpaths).
 * See `docs/decisions/theme-builder-sandbox.md`.
 */

export interface Policy {
  /** The only image sandbox containers may use. */
  image: string;
  /** Label key/value every sandbox container carries (the compose project). */
  labelKey: string;
  labelValue: string;
  /** The named volume holding job inputs/outputs (mounted through subpaths only). */
  volume: string;
  /** Allowed `NetworkMode`s: `none` and the internal check network. */
  networks: string[];
  maxMemory: number;
  maxNanoCpus: number;
  maxPids: number;
  maxShm: number;
}

export const LABEL_KEY = "platform.theme-sandbox";
const JOB_LABEL = "platform.theme-job";

const TOP_LEVEL = new Set([
  "Image",
  "Cmd",
  "Env",
  "User",
  "WorkingDir",
  "Labels",
  "HostConfig",
  "AttachStdin",
  "AttachStdout",
  "AttachStderr",
  "Tty",
  "OpenStdin",
  "StopTimeout",
  "NetworkDisabled",
]);
const HOST_CONFIG = new Set([
  "NetworkMode",
  "ReadonlyRootfs",
  "CapDrop",
  "SecurityOpt",
  "Memory",
  "MemorySwap",
  "NanoCpus",
  "PidsLimit",
  "ShmSize",
  "Tmpfs",
  "Mounts",
  "Init",
  "AutoRemove",
  "Privileged",
  "LogConfig",
]);
const TMPFS_PATHS = new Set(["/work", "/tmp"]);
const TMPFS_OPTS =
  /^rw,noexec,nosuid,nodev,size=\d{1,5}[km],uid=[1-9]\d{0,5},gid=[1-9]\d{0,5},mode=0?7[05]{2}$/;
const SUBPATH = /^[a-z0-9-]{1,64}\/(in|out\/[a-z]{1,16})$/;
const ENV_ENTRY = /^[A-Z][A-Z0-9_]{0,63}=/;
// Credentials never go into a sandbox (A6). ASTRO_KEY is the tenant's own bundle key.
const SECRET_ENV = /(TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|_KEY)$/;
const NAME = /^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}$/;

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const isInt = (v: unknown, min: number, max: number): v is number =>
  typeof v === "number" && Number.isInteger(v) && v >= min && v <= max;

/** Problems with a create request; empty = allowed. */
export function checkCreate(body: unknown, p: Policy, name: string | null): string[] {
  const errors: string[] = [];
  const fail = (m: string) => errors.push(m);
  if (name !== null && !NAME.test(name)) fail("invalid container name");
  if (!isObject(body)) return ["body must be a JSON object"];
  for (const k of Object.keys(body)) if (!TOP_LEVEL.has(k)) fail(`field ${k} is not allowed`);
  if (body.Image !== p.image) fail(`image must be ${p.image}`);
  if (!Array.isArray(body.Cmd) || !body.Cmd.every((c) => typeof c === "string"))
    fail("Cmd must be a list of strings");
  if (body.Env !== undefined) {
    if (!Array.isArray(body.Env)) fail("Env must be a list");
    else
      for (const e of body.Env) {
        if (typeof e !== "string" || !ENV_ENTRY.test(e)) fail("invalid Env entry");
        else {
          const key = e.slice(0, e.indexOf("="));
          if (SECRET_ENV.test(key) && key !== "ASTRO_KEY") fail(`Env ${key} looks like a secret`);
        }
      }
  }
  if (typeof body.User !== "string" || !/^[1-9]\d{0,5}:[1-9]\d{0,5}$/.test(body.User))
    fail("User must be a numeric non-root uid:gid");
  if (
    body.WorkingDir !== undefined &&
    !(typeof body.WorkingDir === "string" && TMPFS_PATHS.has(body.WorkingDir))
  )
    fail("WorkingDir must be /work or /tmp");
  for (const k of ["AttachStdin", "Tty", "OpenStdin"] as const)
    if (body[k] !== undefined && body[k] !== false) fail(`${k} must be false`);
  const labels = body.Labels;
  if (!isObject(labels) || labels[p.labelKey] !== p.labelValue)
    fail(`label ${p.labelKey}=${p.labelValue} is required`);
  else
    for (const [k, v] of Object.entries(labels))
      if (![p.labelKey, JOB_LABEL].includes(k) || typeof v !== "string" || v.length > 128)
        fail(`label ${k} is not allowed`);

  const h = body.HostConfig;
  if (!isObject(h)) return [...errors, "HostConfig is required"];
  for (const k of Object.keys(h)) if (!HOST_CONFIG.has(k)) fail(`HostConfig.${k} is not allowed`);
  if (typeof h.NetworkMode !== "string" || !p.networks.includes(h.NetworkMode))
    fail(`NetworkMode must be one of ${p.networks.join(", ")}`);
  if (body.NetworkDisabled !== undefined && typeof body.NetworkDisabled !== "boolean")
    fail("NetworkDisabled must be a boolean");
  if (h.ReadonlyRootfs !== true) fail("ReadonlyRootfs must be true");
  if (h.Privileged !== undefined && h.Privileged !== false) fail("Privileged must be false");
  if (JSON.stringify(h.CapDrop) !== '["ALL"]') fail('CapDrop must be ["ALL"]');
  // Bounded log storage on the host (a sandbox may print without end).
  if (
    JSON.stringify(h.LogConfig) !== '{"Type":"json-file","Config":{"max-size":"8m","max-file":"1"}}'
  )
    fail("LogConfig must be json-file with max-size 8m, max-file 1");
  if (JSON.stringify(h.SecurityOpt) !== '["no-new-privileges"]')
    fail('SecurityOpt must be ["no-new-privileges"]');
  if (!isInt(h.Memory, 64 * 1024 * 1024, p.maxMemory)) fail("Memory out of range");
  if (h.MemorySwap !== h.Memory) fail("MemorySwap must equal Memory (no swap)");
  if (!isInt(h.NanoCpus, 1, p.maxNanoCpus)) fail("NanoCpus out of range");
  if (!isInt(h.PidsLimit, 1, p.maxPids)) fail("PidsLimit out of range");
  if (h.ShmSize !== undefined && !isInt(h.ShmSize, 0, p.maxShm)) fail("ShmSize out of range");
  for (const k of ["Init", "AutoRemove"] as const)
    if (h[k] !== undefined && typeof h[k] !== "boolean") fail(`${k} must be a boolean`);

  if (!isObject(h.Tmpfs)) fail("Tmpfs is required");
  else
    for (const [path, opts] of Object.entries(h.Tmpfs))
      if (!TMPFS_PATHS.has(path) || typeof opts !== "string" || !TMPFS_OPTS.test(opts))
        fail(`Tmpfs ${path} is not allowed`);

  if (h.Mounts !== undefined) {
    if (!Array.isArray(h.Mounts)) fail("Mounts must be a list");
    else
      for (const m of h.Mounts) {
        if (!isObject(m)) {
          fail("invalid mount");
          continue;
        }
        const keys = Object.keys(m).sort().join(",");
        const opts = m.VolumeOptions;
        const subpath = isObject(opts) ? opts.Subpath : undefined;
        const ok =
          (keys === "ReadOnly,Source,Target,Type,VolumeOptions" ||
            keys === "Source,Target,Type,VolumeOptions") &&
          m.Type === "volume" &&
          m.Source === p.volume &&
          isObject(opts) &&
          Object.keys(opts).join(",") === "Subpath" &&
          typeof subpath === "string" &&
          SUBPATH.test(subpath) &&
          ((m.Target === "/in" && subpath.endsWith("/in") && m.ReadOnly === true) ||
            (m.Target === "/out" && /\/out\/[a-z]+$/.test(subpath)));
        if (!ok) fail(`mount ${JSON.stringify(m).slice(0, 200)} is not allowed`);
      }
  }
  return errors;
}

/** Per-container calls (start, wait, logs, kill, remove) and the list/ping endpoints. */
export type Route =
  | { kind: "create"; name: string | null }
  | { kind: "container"; id: string; action: "start" | "wait" | "logs" | "kill" | "remove" }
  | { kind: "list" }
  | { kind: "ping" };

/** Parses an Engine API request (optional `/v1.xx` prefix); `null` = refused. */
export function route(method: string, rawUrl: string): Route | null {
  const url = new URL(rawUrl, "http://docker");
  const path = url.pathname.replace(/^\/v1\.\d{1,2}(?=\/)/, "");
  if ((method === "GET" || method === "HEAD") && (path === "/_ping" || path === "/version"))
    return { kind: "ping" };
  if (method === "POST" && path === "/containers/create")
    return { kind: "create", name: url.searchParams.get("name") };
  if (method === "GET" && path === "/containers/json") return { kind: "list" };
  const m = /^\/containers\/([a-zA-Z0-9][a-zA-Z0-9_.-]{0,127})(\/(start|wait|logs|kill))?$/.exec(
    path,
  );
  if (!m?.[1]) return null;
  const id = m[1];
  if (!m[3]) return method === "DELETE" ? { kind: "container", id, action: "remove" } : null;
  const action = m[3] as "start" | "wait" | "logs" | "kill";
  const expected = action === "logs" ? "GET" : "POST";
  return method === expected ? { kind: "container", id, action } : null;
}

/** Query parameters kept per call; everything else is dropped. */
export function safeQuery(r: Route, rawUrl: string, p: Policy): string {
  const q = new URL(rawUrl, "http://docker").searchParams;
  const out = new URLSearchParams();
  const keep = (k: string, re: RegExp) => {
    const v = q.get(k);
    if (v !== null && re.test(v)) out.set(k, v);
  };
  if (r.kind === "create" && r.name) out.set("name", r.name);
  if (r.kind === "list") {
    out.set("all", "1");
    out.set("filters", JSON.stringify({ label: [`${p.labelKey}=${p.labelValue}`] }));
  }
  if (r.kind === "container") {
    if (r.action === "logs") {
      keep("stdout", /^(0|1|true|false)$/);
      keep("stderr", /^(0|1|true|false)$/);
      keep("tail", /^(\d{1,6}|all)$/);
    }
    if (r.action === "wait") keep("condition", /^(not-running|next-exit)$/);
    if (r.action === "kill") keep("signal", /^(SIGKILL|KILL|9)$/);
    if (r.action === "remove") {
      keep("force", /^(1|true)$/);
      keep("v", /^(1|true)$/);
    }
  }
  const s = out.toString();
  return s ? `?${s}` : "";
}
