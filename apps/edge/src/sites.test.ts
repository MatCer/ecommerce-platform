import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { expect, test } from "vitest";
import {
  CachedResolver,
  ChannelResolver,
  classifyHost,
  normalizeHost,
  type Site,
  StaticResolver,
} from "./sites.ts";

test("@channel references resolve to the pointer file (local publish = re-pack + purge)", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "wp2-channels-"));
  await mkdir(path.join(root, "channels"));
  await writeFile(
    path.join(root, "channels", "default-theme"),
    "0123456789abcdef0123456789abcdef\n",
  );
  const inner = new StaticResolver({
    "demo.localhost": { theme_artifact: "@default-theme" } as Site,
  });
  const site = await new ChannelResolver(inner, root).resolve("demo.localhost");
  expect(site?.theme_artifact).toBe("0123456789abcdef0123456789abcdef");
  inner.set("x.localhost", { theme_artifact: "@../../etc/passwd" } as Site);
  expect((await new ChannelResolver(inner, root).resolve("x.localhost"))?.theme_artifact).toBe(
    "@../../etc/passwd",
  );
});

test.each([
  ["Demo.Localhost:8280", "demo.localhost"],
  ["demo.localhost.", "demo.localhost"],
  ["checkout.demo.localhost", "checkout.demo.localhost"],
  ["evil.com/x", null],
  ["a b", null],
  ["", null],
  ["-bad.localhost", null],
])("normalizeHost(%s)", (raw, want) => expect(normalizeHost(raw)).toBe(want));

test("checkout origin maps to its shop", () => {
  expect(classifyHost("checkout.demo.localhost")).toEqual({
    kind: "checkout",
    shopHost: "demo.localhost",
  });
  expect(classifyHost("demo.localhost")).toEqual({ kind: "shop", shopHost: "demo.localhost" });
});

test("resolution is cached 60 s and purged per tenant", async () => {
  const site = { tenant_id: "t1", theme_artifact: "a" } as Site;
  const inner = new StaticResolver({ "demo.localhost": site });
  const cached = new CachedResolver(inner, 60_000);
  expect((await cached.resolve("demo.localhost", 0))?.theme_artifact).toBe("a");
  inner.set("demo.localhost", { ...site, theme_artifact: "b" });
  expect((await cached.resolve("demo.localhost", 59_000))?.theme_artifact).toBe("a");
  cached.purge("t1");
  expect((await cached.resolve("demo.localhost", 59_001))?.theme_artifact).toBe("b");
  expect(await cached.resolve("__proto__", 0)).toBeNull();
});
