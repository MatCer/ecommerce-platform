import { mkdir, mkdtemp, readFile, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";
import { packArtifact, readManifest, verifyArtifact } from "./artifact.ts";

const PROJECT = fileURLToPath(new URL("../../../themes/default", import.meta.url));

async function dist(files: Record<string, string>) {
  const d = await mkdtemp(path.join(tmpdir(), "wp2-pack-"));
  for (const [p, body] of Object.entries(files)) {
    await mkdir(path.dirname(path.join(d, p)), { recursive: true });
    await writeFile(path.join(d, p), body);
  }
  return d;
}

test("content-addressed, idempotent, control files excluded, exact bytes published", async () => {
  const out = await mkdtemp(path.join(tmpdir(), "wp2-out-"));
  const d = await dist({
    "server/entry.mjs": "export default {}",
    "client/_astro/a.js": "1",
    "client/_headers": "x",
  });
  const a = await packArtifact({ dist: d, outRoot: out, kind: "theme", projectDir: PROJECT });
  const b = await packArtifact({ dist: d, outRoot: out, kind: "theme", projectDir: PROJECT });
  expect(a.id).toBe(b.id);
  expect(Object.keys(a.assets)).toEqual(["/_astro/a.js"]);
  expect(await readFile(path.join(out, a.id, "client/_astro/a.js"), "utf8")).toBe("1");
  expect((await readManifest(out, a.id)).runtime.modules).toEqual(["entry.mjs"]);
  await expect(readManifest(out, "../etc")).rejects.toThrow();
});

test("refuses symlinked roots and entries", async () => {
  const out = await mkdtemp(path.join(tmpdir(), "wp2-out-"));
  const secret = await dist({ "secret.txt": "private" });
  const d = await dist({ "server/entry.mjs": "export default {}" });
  await symlink(secret, path.join(d, "client"));
  await expect(
    packArtifact({ dist: d, outRoot: out, kind: "theme", projectDir: PROJECT }),
  ).rejects.toThrow(/real directory/);

  const e = await dist({ "server/entry.mjs": "export default {}", "client/ok.txt": "1" });
  await symlink(path.join(secret, "secret.txt"), path.join(e, "client/leak.txt"));
  await expect(
    packArtifact({ dist: e, outRoot: out, kind: "theme", projectDir: PROJECT }),
  ).rejects.toThrow(/symlink/);
});

test("verify recomputes the content address and refuses tampered, missing or extra files", async () => {
  const d = await dist({ "server/entry.mjs": "export default {}", "client/_astro/a.js": "1" });
  const pack = async () => {
    const out = await mkdtemp(path.join(tmpdir(), "wp6-verify-"));
    return {
      out,
      m: await packArtifact({ dist: d, outRoot: out, kind: "theme", projectDir: PROJECT }),
    };
  };
  const ok = await pack();
  expect((await verifyArtifact(ok.out, ok.m.id)).id).toBe(ok.m.id);

  const tampered = await pack();
  await writeFile(path.join(tampered.out, tampered.m.id, "server/entry.mjs"), "export default 1");
  await expect(verifyArtifact(tampered.out, tampered.m.id)).rejects.toThrow(/content address/);

  const extra = await pack();
  await writeFile(path.join(extra.out, extra.m.id, "server/evil.mjs"), "1");
  await expect(verifyArtifact(extra.out, extra.m.id)).rejects.toThrow(/extra server\/evil.mjs/);

  const asset = await pack();
  await writeFile(path.join(asset.out, asset.m.id, "client/_astro/a.js"), "2");
  await expect(verifyArtifact(asset.out, asset.m.id)).rejects.toThrow(/does not match/);
});
