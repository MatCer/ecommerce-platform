import { describe, expect, test } from "vitest";
import { Docker } from "../src/docker.ts";
import { checkCreate, LABEL_KEY, type Policy, route, safeQuery } from "../src/policy.ts";

const policy: Policy = {
  image: "wp23-theme-builder:local",
  labelKey: LABEL_KEY,
  labelValue: "wp23",
  volume: "wp23_theme-work",
  networks: ["none", "wp23_theme-check"],
  maxMemory: 4 * 1024 ** 3,
  maxNanoCpus: 4e9,
  maxPids: 1024,
  maxShm: 1024 ** 3,
};

const docker = new Docker({
  url: "http://proxy",
  image: policy.image,
  project: "wp23",
  volume: policy.volume,
  user: "1000:1000",
});

/** What the builder really sends for a build step. */
function buildBody() {
  return docker.createBody({
    step: "build",
    job: "0192f000-0000-7000-8000-000000000001",
    cmd: ["node", "/repo/apps/theme-builder/src/sandbox.ts", "build"],
    env: { ASTRO_KEY: "a2V5", HOME: "/tmp" },
    network: "none",
    mounts: [
      { target: "/in", subpath: "0192f000-0000-7000-8000-000000000001/in" },
      { target: "/out", subpath: "0192f000-0000-7000-8000-000000000001/out/build" },
    ],
    memoryBytes: 3 * 1024 ** 3,
    cpus: 2,
    pids: 512,
    timeoutMs: 1000,
  });
}

type Body = ReturnType<typeof buildBody> & Record<string, unknown>;

describe("sandbox create policy (A6)", () => {
  test("the builder's own requests pass", () => {
    expect(checkCreate(buildBody(), policy, "wp23-tb-x-build-abc")).toEqual([]);
    const check = docker.createBody({
      step: "check",
      job: "j",
      cmd: ["node", "x"],
      env: { PREVIEW_COOKIE: "__Host-preview=t", THEME_KIT_CHROMIUM_ARGS: "[]" },
      network: "wp23_theme-check",
      mounts: [{ target: "/out", subpath: "j/out/check" }],
      memoryBytes: 3 * 1024 ** 3,
      cpus: 2,
      pids: 1024,
      timeoutMs: 1,
    });
    expect(checkCreate(check, policy, null)).toEqual([]);
  });

  const escapes: [string, (b: Body) => void][] = [
    ["another image", (b) => (b.Image = "alpine")],
    ["root", (b) => (b.User = "0:0")],
    ["no user", (b) => delete (b as Record<string, unknown>).User],
    ["host network", (b) => (b.HostConfig.NetworkMode = "host")],
    ["bridge network", (b) => (b.HostConfig.NetworkMode = "bridge")],
    ["privileged", (b) => Object.assign(b.HostConfig, { Privileged: true })],
    ["writable root", (b) => (b.HostConfig.ReadonlyRootfs = false)],
    ["capabilities", (b) => (b.HostConfig.CapDrop = [])],
    ["cap add", (b) => Object.assign(b.HostConfig, { CapAdd: ["SYS_ADMIN"] })],
    ["new privileges", (b) => (b.HostConfig.SecurityOpt = [])],
    ["seccomp off", (b) => (b.HostConfig.SecurityOpt = ["no-new-privileges", "seccomp=unconfined"])],
    ["bind mount", (b) => Object.assign(b.HostConfig, { Binds: ["/var/run/docker.sock:/s"] })],
    ["pid host", (b) => Object.assign(b.HostConfig, { PidMode: "host" })],
    ["devices", (b) => Object.assign(b.HostConfig, { Devices: [{ PathOnHost: "/dev/sda" }] })],
    ["unlimited memory", (b) => (b.HostConfig.Memory = 0)],
    ["swap", (b) => (b.HostConfig.MemorySwap = -1)],
    ["too many pids", (b) => (b.HostConfig.PidsLimit = 100_000)],
    ["no cpu limit", (b) => (b.HostConfig.NanoCpus = 0)],
    ["exec tmpfs", (b) => (b.HostConfig.Tmpfs["/work"] = "rw,size=1g")],
    ["tmpfs elsewhere", (b) => Object.assign(b.HostConfig.Tmpfs, { "/etc": "rw,noexec,nosuid,nodev,size=1m,uid=1000,gid=1000,mode=0700" })],
    ["other volume", (b) => ((b.HostConfig.Mounts[0] as { Source: string }).Source = "wp20_pg-data")],
    ["bind type", (b) => ((b.HostConfig.Mounts[0] as { Type: string }).Type = "bind")],
    ["subpath escape", (b) => ((b.HostConfig.Mounts[1] as { VolumeOptions: { Subpath: string } }).VolumeOptions.Subpath = "../x/out/build")],
    ["whole volume", (b) => ((b.HostConfig.Mounts[1] as { VolumeOptions: { Subpath: string } }).VolumeOptions.Subpath = "")],
    ["writable input", (b) => Object.assign(b.HostConfig.Mounts[0] as object, { ReadOnly: false })],
    ["input at /out", (b) => ((b.HostConfig.Mounts[0] as { Target: string }).Target = "/out")],
    ["secret env", (b) => b.Env.push("THEME_BUILDER_TOKEN=x")],
    ["api key env", (b) => b.Env.push("S3_SECRET_ACCESS_KEY=x")],
    ["tty", (b) => Object.assign(b, { Tty: true })],
    ["networking config", (b) => Object.assign(b, { NetworkingConfig: {} })],
    ["missing label", (b) => Object.assign(b, { Labels: {} })],
    ["foreign label", (b) => Object.assign(b.Labels, { "com.docker.compose.project": "wp20" })],
  ];
  for (const [name, mutate] of escapes)
    test(`refuses ${name}`, () => {
      const b = structuredClone(buildBody()) as Body;
      mutate(b);
      expect(checkCreate(b, policy, null)).not.toEqual([]);
    });

  test("refuses odd container names", () => {
    expect(checkCreate(buildBody(), policy, "../x")).not.toEqual([]);
  });
});

describe("routes", () => {
  test("only the sandbox calls", () => {
    expect(route("POST", "/v1.44/containers/create?name=x")).toEqual({ kind: "create", name: "x" });
    expect(route("POST", "/containers/abc/start")).toEqual({ kind: "container", id: "abc", action: "start" });
    expect(route("GET", "/v1.44/containers/abc/logs?stdout=1")).toMatchObject({ action: "logs" });
    expect(route("DELETE", "/containers/abc?force=1")).toMatchObject({ action: "remove" });
    expect(route("GET", "/containers/json")).toEqual({ kind: "list" });
    for (const [m, u] of [
      ["POST", "/containers/abc/exec"],
      ["POST", "/exec/abc/start"],
      ["PUT", "/containers/abc/archive?path=/"],
      ["GET", "/containers/abc/archive?path=/etc/shadow"],
      ["POST", "/containers/abc/attach"],
      ["GET", "/containers/abc/json"],
      ["POST", "/images/create?fromImage=evil"],
      ["POST", "/build"],
      ["POST", "/volumes/create"],
      ["POST", "/networks/create"],
      ["GET", "/info"],
      ["POST", "/containers/abc/update"],
      ["GET", "/containers/abc/start"],
      ["POST", "/swarm/init"],
    ] as const)
      expect(route(m, u), `${m} ${u}`).toBeNull();
  });

  test("queries are rebuilt from an allowlist; lists are always filtered", () => {
    const list = route("GET", "/containers/json?filters={}") ?? { kind: "ping" as const };
    expect(decodeURIComponent(safeQuery(list, "/containers/json?all=1&filters={}", policy))).toContain(
      '"label":["platform.theme-sandbox=wp23"]',
    );
    const logs = route("GET", "/containers/a/logs") ?? { kind: "ping" as const };
    expect(safeQuery(logs, "/containers/a/logs?stdout=1&follow=1&since=x", policy)).toBe("?stdout=1");
  });
});
