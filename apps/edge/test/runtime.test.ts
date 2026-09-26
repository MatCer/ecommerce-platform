import { readFileSync } from "node:fs";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { expect, test } from "vitest";
import { WorkerPool } from "../src/runtime.ts";
import { buildArtifact, checkoutWorker, hostileTheme } from "./fixtures.ts";

function children(): number[] {
  try {
    // /proc avoids counting the inspection command itself as a transient child.
    return readFileSync(`/proc/${process.pid}/task/${process.pid}/children`, "utf8")
      .split(/\s+/)
      .map(Number)
      .filter(Boolean);
  } catch {
    return [];
  }
}

async function until(predicate: () => boolean, timeout = 5000): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeout) throw new Error("runtime process did not change");
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

test("a stalled startup is terminated before its capacity slot is released", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "edge-stalled-"));
  const stalled = (
    await buildArtifact(
      root,
      "theme",
      "await new Promise(() => {}); export default { fetch() { return new Response('ok') } };",
      { "favicon.svg": "<svg/>" },
    )
  ).id;
  const pool = new WorkerPool({ artifactRoot: root, bindings: () => ({}) });
  const before = new Set(children());
  const request = pool
    .fetch(stalled, new Request("http://theme.test/"), "tenant-a")
    .catch(() => undefined);
  try {
    await until(() => children().some((pid) => !before.has(pid)));
    const spawned = children().filter((pid) => !before.has(pid));
    expect(pool.size).toBe(1);
    await pool.evictScope(stalled, "tenant-a");
    await until(() => spawned.every((pid) => !children().includes(pid)));
    expect(pool.size).toBe(0);
  } finally {
    await pool.dispose();
    await request;
  }
});

test("checkout has reserved capacity when theme admission is saturated", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "edge-capacity-"));
  const theme = (
    await buildArtifact(root, "theme", hostileTheme("capacity"), { "favicon.svg": "<svg/>" })
  ).id;
  const checkout = (
    await buildArtifact(root, "checkout", checkoutWorker, { "favicon.svg": "<svg/>" })
  ).id;
  const pool = new WorkerPool({
    artifactRoot: root,
    bindings: () => ({
      CHECKOUT: async () => Response.json({}),
      STOREFRONT: async () => Response.json({ name: "Shop" }),
    }),
  });
  try {
    for (let i = 0; i < 28; i++) {
      await pool.fetch(theme, new Request("http://theme.test/"), `tenant-${i}`);
    }
    expect(pool.size).toBe(28);
    expect((await pool.fetch(checkout, new Request("http://checkout.test/"))).status).toBe(200);
    expect(pool.size).toBe(29);
    expect((await pool.fetch(theme, new Request("http://theme.test/"), "tenant-new")).status).toBe(
      200,
    );
    expect(pool.size).toBe(29);
  } finally {
    await pool.dispose();
  }
}, 120_000);
