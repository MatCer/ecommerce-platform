import { mkdtemp, readFile, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { readManifest } from "@platform/theme-kit";
import { beforeAll, expect, test } from "vitest";
import { ArtifactFetcher } from "../src/artifacts.ts";
import { buildArtifact, hostileTheme } from "./fixtures.ts";

let source: string;
let id: string;

beforeAll(async () => {
  source = await mkdtemp(path.join(tmpdir(), "wp6-src-"));
  id = (
    await buildArtifact(source, "theme", hostileTheme("dl"), {
      "_astro/app.dl.js": "console.log('dl')",
      "favicon.svg": "<svg/>",
    })
  ).id;
});

/** The API's `/internal/v1/artifacts/{id}/{path}` over `source`, with an optional tamper hook. */
function api(tamper?: (rel: string, body: Buffer) => Buffer) {
  const seen: string[] = [];
  const upstream = async (req: Request) => {
    const url = new URL(req.url);
    seen.push(url.pathname);
    if (req.headers.get("authorization") !== "Bearer svc-token")
      return new Response(null, { status: 401 });
    const m = /^\/internal\/v1\/artifacts\/([0-9a-f]{32})\/(.+)$/.exec(url.pathname);
    if (!m) return new Response(null, { status: 404 });
    const body = await readFile(path.join(source, m[1] ?? "", m[2] ?? "")).catch(() => null);
    if (!body) return new Response(null, { status: 404 });
    return new Response(new Uint8Array(tamper ? tamper(m[2] ?? "", body) : body));
  };
  return { upstream, seen };
}

const exists = (p: string) =>
  stat(p).then(
    () => true,
    () => false,
  );

test("downloads, verifies the content address and unpacks an artifact once", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "wp6-dst-"));
  const { upstream, seen } = api();
  const f = new ArtifactFetcher({ root, apiOrigin: "http://api", token: "svc-token", upstream });
  await Promise.all([f.ensure(id), f.ensure(id)]);
  const m = await readManifest(root, id);
  expect(m.id).toBe(id);
  expect(await readFile(path.join(root, id, "client/_astro/app.dl.js"), "utf8")).toBe(
    "console.log('dl')",
  );
  const downloads = seen.length;
  expect(seen.filter((p) => p.endsWith("/manifest.json"))).toHaveLength(1);
  await f.ensure(id); // present: nothing fetched
  expect(seen.length).toBe(downloads);
});

test("a tampered module or asset is refused and nothing is unpacked", async () => {
  for (const target of ["server/entry.mjs", "client/_astro/app.dl.js"]) {
    const root = await mkdtemp(path.join(tmpdir(), "wp6-dst-"));
    const { upstream } = api((rel, body) =>
      rel === target ? Buffer.concat([body, Buffer.from("\n// evil")]) : body,
    );
    const f = new ArtifactFetcher({ root, apiOrigin: "http://api", token: "svc-token", upstream });
    await expect(f.ensure(id)).rejects.toThrow(/does not match|content address/);
    expect(await exists(path.join(root, id))).toBe(false);
  }
});

test("a manifest for another id, bad ids and a wrong token are refused", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "wp6-dst-"));
  const { upstream } = api();
  await expect(
    new ArtifactFetcher({ root, apiOrigin: "http://api", token: "nope", upstream }).ensure(id),
  ).rejects.toThrow(/401/);
  const f = new ArtifactFetcher({ root, apiOrigin: "http://api", token: "svc-token", upstream });
  await expect(f.ensure("../etc")).rejects.toThrow(/invalid id/);
  const other = "f".repeat(32);
  await expect(f.ensure(other)).rejects.toThrow(/404/);
  // A manifest served under the wrong id.
  const swapped = api((rel, body) => (rel === "manifest.json" ? body : body));
  const g = new ArtifactFetcher({
    root,
    apiOrigin: "http://api",
    token: "svc-token",
    upstream: async (req) => {
      const u = new URL(req.url);
      u.pathname = u.pathname.replace(other, id);
      return swapped.upstream(new Request(u, req));
    },
  });
  await expect(g.ensure(other)).rejects.toThrow(/manifest mismatch/);
});
