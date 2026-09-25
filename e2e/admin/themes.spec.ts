/**
 * WP23 theme builder, against the seeded demo shop after `make theme-build`:
 * fork the default theme → change a token → sandboxed build + gates → preview (token-bound,
 * A21) → publish → the storefront shows the change → roll back restores it. Then hostile
 * archives: refused at upload (symlink, `..`, oversized) or failed by the gates with reasons
 * (extra dependency, foreign fetch, a network attempt during the build, a JS budget blow-out).
 *
 * Builds run for real (one at a time): the suite takes several minutes.
 */

import { randomBytes } from "node:crypto";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { gzipSync } from "node:zlib";
import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { testContext } from "../rate-client";
import { magicLink, sql as querySql, root, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });
const owner = "owner@lnen.example";
const port = process.env.HTTP_PORT ?? "8080";
const shop = `http://demo.localhost:${port}`;
const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
// A light green: the buy button keeps dark text, so contrast (axe) still passes.
const GREEN = "#86d7a8";
let context: BrowserContext;
let page: Page;

function sql(query: string): string[] {
  return querySql(query)
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}

const latest = () =>
  Number(sql(`SELECT coalesce(max(number), 0) FROM theme_revisions WHERE tenant_id = ${DEMO}`)[0]);

/** Polls a revision until it leaves the pending statuses; returns status + failures. */
async function settled(number: number, timeoutMs = 8 * 60_000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const [row] = sql(
      `SELECT status || '|' || coalesce(checks->'failures', '[]'::jsonb)::text FROM theme_revisions
       WHERE tenant_id = ${DEMO} AND number = ${number}`,
    );
    const [status = "", failures = "[]"] = (row ?? "").split(/\|(.*)/s);
    if (status && !["draft", "building", "checking"].includes(status))
      return { status, failures: JSON.parse(failures) as string[] };
    if (Date.now() > deadline) throw new Error(`revision #${number} still ${status}`);
    await new Promise((r) => setTimeout(r, 3000));
  }
}

/** The demo shop's `--color-buy` as the browser computes it (theme CSS from the tokens). */
async function buyColor(p: Page): Promise<string> {
  return p.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--color-buy").trim(),
  );
}

// ---------------------------------------------------------------------------------------
// Archives built in the test: a minimal tar writer, so hostile entries (symlinks, `..`) can
// be produced exactly as an attacker would.

interface Entry {
  name: string;
  body?: Buffer | string;
  type?: "0" | "2" | "5";
  link?: string;
}

function tarGz(entries: Entry[]): Buffer {
  const blocks: Buffer[] = [];
  for (const e of entries) {
    const body = Buffer.from(e.body ?? "");
    const h = Buffer.alloc(512);
    h.write(e.name, 0, 100, "utf8");
    h.write("0000644\0", 100);
    h.write("0000000\0", 108);
    h.write("0000000\0", 116);
    h.write(`${body.length.toString(8).padStart(11, "0")}\0`, 124);
    h.write("00000000000\0", 136);
    h.write("        ", 148);
    h.write(e.type ?? "0", 156);
    if (e.link) h.write(e.link, 157, 100, "utf8");
    h.write("ustar\0", 257);
    h.write("00", 263);
    let sum = 0;
    for (const b of h) sum += b;
    h.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    blocks.push(h, body, Buffer.alloc((512 - (body.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}

/** The default theme's own files (what a fork starts from). */
function defaultTheme(): Map<string, Buffer> {
  const dir = join(root, "themes/default");
  const files = new Map<string, Buffer>();
  const walk = (rel: string) => {
    for (const name of readdirSync(join(dir, rel))) {
      const r = rel ? `${rel}/${name}` : name;
      if (statSync(join(dir, r)).isDirectory()) walk(r);
      else files.set(r, readFileSync(join(dir, r)));
    }
  };
  walk("src");
  walk("public");
  for (const f of ["theme.tokens.json", "package.json", "astro.config.mjs", "tsconfig.json"])
    files.set(f, readFileSync(join(dir, f)));
  return files;
}

const archive = (files: Map<string, Buffer>) =>
  tarGz([...files].map(([name, body]) => ({ name, body })));

async function upload(name: string, bytes: Buffer): Promise<void> {
  await page.goto("/themes");
  await page.getByLabel("Theme archive (.tar.gz)").setInputFiles({
    name,
    mimeType: "application/gzip",
    buffer: bytes,
  });
}

test.beforeAll(async ({ browser }) => {
  context = await testContext(browser);
  page = await context.newPage();
  await useEnglish(page);
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await expect(page.getByRole("status")).toContainText(owner);
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
});

test.afterAll(async () => {
  await context?.close();
});

let forked = 0;
let tokens = 0;

test("forking the default theme builds and passes every gate", async () => {
  test.setTimeout(10 * 60_000);
  await page.goto("/themes");
  await expect(page.getByRole("heading", { name: "Theme", exact: true })).toBeVisible();
  const start = latest();
  const fork = page.getByRole("button", { name: "Create my own theme" });
  await ((await fork.isVisible())
    ? fork
    : page.getByRole("button", { name: "Reset to default theme" })
  ).click();
  await expect(page.getByText(/is being built and checked/)).toBeVisible();
  forked = start + 1;
  const result = await settled(forked);
  expect(result.failures).toEqual([]);
  expect(result.status).toBe("ready");

  // The report: every step, budget numbers for home/category/product, screenshots.
  await page.goto("/themes");
  await page.getByRole("button", { name: `#${forked}`, exact: true }).click();
  const report = page.getByRole("region", { name: `Checks of revision #${forked}` });
  for (const step of ["lint", "typecheck", "build", "budget", "smoke"])
    await expect(report.getByRole("row", { name: new RegExp(`^${step} Passed`) })).toBeVisible();
  await expect(report.getByRole("table").last().getByRole("row")).toHaveCount(4);
  await expect(report.getByRole("img", { name: /Screenshot home-mobile/ })).toBeVisible();
  const [sandbox] = sql(
    `SELECT checks->'sandbox'->>'build_network' FROM theme_revisions WHERE tenant_id = ${DEMO} AND number = ${forked}`,
  );
  expect(sandbox).toBe("none");
});

test("a token change builds on the fast path and previews behind its token", async () => {
  test.setTimeout(6 * 60_000);
  await page.goto("/themes");
  const editor = page.getByRole("region", { name: "Colours, fonts and corners" });
  await editor.getByRole("textbox", { name: "buy", exact: true }).fill(GREEN);
  tokens = latest() + 1;
  await editor.getByRole("button", { name: "Create revision" }).click();
  await expect(page.getByText(`Revision #${tokens} is being built and checked.`)).toBeVisible();
  const result = await settled(tokens);
  expect(result.failures).toEqual([]);
  expect(result.status).toBe("ready");
  const [pipeline] = sql(
    `SELECT checks->>'pipeline' FROM theme_revisions WHERE tenant_id = ${DEMO} AND number = ${tokens}`,
  );
  expect(pipeline).toBe("tokens");

  // Preview in the admin: a sandboxed iframe on the preview origin.
  await page.goto("/themes");
  const row = page
    .getByRole("row")
    .filter({ has: page.getByRole("button", { name: `#${tokens}`, exact: true }) });
  await row.getByRole("button", { name: "Preview" }).click();
  const frame = page.locator(`iframe[title="Preview of revision #${tokens}"]`);
  await expect(frame).toHaveAttribute("sandbox", "allow-scripts allow-same-origin allow-forms");
  const src = (await frame.getAttribute("src")) ?? "";
  expect(src).toMatch(
    new RegExp(`^http://preview-${tokens}--demo\\.localhost:${port}/\\?preview_token=`),
  );
  await expect(frame.contentFrame().locator("main")).toBeVisible();
  expect(
    await frame
      .contentFrame()
      .locator("html")
      .evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--color-buy").trim(),
      ),
  ).toBe(GREEN);

  // Token-bound: no token, or this token on another revision's host, gets nothing.
  const stranger = await context.browser()?.newContext();
  const p = await stranger?.newPage();
  const bare = await p?.goto(`http://preview-${tokens}--demo.localhost:${port}/`);
  expect(bare?.status()).toBe(401);
  const token = new URL(src).searchParams.get("preview_token") ?? "";
  const other = await p?.goto(
    `http://preview-${tokens - 1}--demo.localhost:${port}/?preview_token=${token}`,
  );
  expect(other?.status()).toBe(401);
  const good = await p?.goto(src);
  expect(good?.status()).toBe(200);
  expect(good?.headers()["x-robots-tag"]).toBe("noindex, nofollow");
  expect(good?.headers()["cache-control"]).toBe("no-store");
  await stranger?.close();
});

test("publishing changes the storefront; rolling back restores it", async () => {
  const live = sql(
    `SELECT r.number FROM theme_active a JOIN theme_revisions r ON r.id = a.revision_id WHERE a.tenant_id = ${DEMO}`,
  )[0];
  const shopPage = await context.newPage();
  await shopPage.goto(shop);
  const before = await buyColor(shopPage);
  expect(before).not.toBe(GREEN);
  await page.goto("/themes");
  const row = (n: number | string) =>
    page.getByRole("row").filter({ has: page.getByRole("button", { name: `#${n}`, exact: true }) });
  await row(tokens).getByRole("button", { name: "Publish" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Publish" }).click();
  await expect(page.getByText(`Revision #${tokens} is live.`)).toBeVisible();
  await shopPage.reload();
  expect(await buyColor(shopPage)).toBe(GREEN);
  const audit = sql(
    `SELECT count(*) FROM audit_log WHERE tenant_id = ${DEMO} AND action = 'theme.published'`,
  );
  expect(Number(audit[0])).toBeGreaterThan(0);

  await row(live ?? "")
    .getByRole("button", { name: "Roll back" })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Roll back" }).click();
  await expect(page.getByText(`Revision #${live} is live.`)).toBeVisible();
  await shopPage.reload();
  expect(await buyColor(shopPage)).toBe(before);
  await shopPage.close();
});

test("hostile archives are refused at upload with every reason", async () => {
  const tokensFile = readFileSync(join(root, "themes/default/theme.tokens.json"));
  const count = latest();
  await upload(
    "evil.tar.gz",
    tarGz([
      { name: "theme.tokens.json", body: tokensFile },
      { name: "src/pages/index.astro", type: "2", link: "/etc/passwd" },
      { name: "../escape.astro", body: "x" },
      { name: "src/../../etc/cron.d/x", body: "x" },
      { name: "/root/.ssh/authorized_keys", body: "x" },
    ]),
  );
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("The archive was refused");
  await expect(alert).toContainText('"src/pages/index.astro": symbolic links are not allowed');
  await expect(alert).toContainText(`"../escape.astro": '..' is not allowed`);
  await expect(alert).toContainText("absolute paths are not allowed");

  await upload(
    "big.tar.gz",
    tarGz([
      { name: "theme.tokens.json", body: tokensFile },
      { name: "public/big.bin", body: Buffer.alloc(51 * 1024 * 1024) },
    ]),
  );
  await expect(page.getByRole("alert")).toContainText("larger than 50 MB");
  expect(latest()).toBe(count);
});

test("contract violations and a network attempt fail the build with reasons", async () => {
  test.setTimeout(6 * 60_000);
  const cases: [string, (f: Map<string, Buffer>) => void, RegExp][] = [
    [
      "extra dependency",
      (f) => {
        const pkg = JSON.parse(f.get("package.json")?.toString() ?? "{}");
        pkg.dependencies["left-pad"] = "1.3.0";
        f.set("package.json", Buffer.from(JSON.stringify(pkg, null, 2)));
      },
      /lint: package\.json locked-deps/,
    ],
    [
      "foreign fetch",
      (f) =>
        f.set(
          "src/pages/pages/track.astro",
          Buffer.from('---\nawait fetch("https://tracker.example/pixel");\n---\n<p>x</p>\n'),
        ),
      /lint: src\/pages\/pages\/track\.astro:2 foreign-fetch/,
    ],
    [
      "network during the build",
      (f) =>
        f.set(
          "src/pages/probe.astro",
          Buffer.from(
            [
              "---",
              "export const prerender = true;",
              'const target = ["https:", "", "example.com", ""].join("/");',
              "let outcome = 'NETWORK-PROBE: reachable';",
              "try { await fetch(target); } catch (e) {",
              "  const err = e as { cause?: { code?: string }; message?: string };",
              "  outcome = 'NETWORK-PROBE: blocked ' + (err.cause?.code ?? err.message);",
              "}",
              "// The build fails either way, so the outcome lands in the report.",
              "throw new Error(outcome);",
              "---",
              "<p>probe</p>",
              "",
            ].join("\n"),
          ),
        ),
      // No network in the build sandbox: the lookup itself fails (workerd prerenders the page).
      /build:[\s\S]*(NETWORK-PROBE: blocked|EAI_AGAIN[\s\S]*example\.com)/,
    ],
  ];
  for (const [name, mutate, reason] of cases) {
    const files = defaultTheme();
    mutate(files);
    const n = latest() + 1;
    await upload(`${name}.tar.gz`, archive(files));
    await expect(page.getByText(`Revision #${n} is being built and checked.`)).toBeVisible();
    const result = await settled(n);
    expect(result.status, name).toBe("failed");
    expect(result.failures.join("\n"), name).toMatch(reason);
    expect(result.failures.join("\n")).not.toContain("NETWORK-PROBE: reachable");
  }
  // The failure reasons are shown in the admin.
  await page.goto("/themes");
  await page.getByRole("button", { name: `#${latest()}`, exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(/NETWORK-PROBE: blocked|EAI_AGAIN/);
});

test("a revision that blows the JS budget fails with the numbers", async () => {
  test.setTimeout(10 * 60_000);
  const files = defaultTheme();
  // ~90 kB of incompressible data shipped in a client island on the home page.
  const blob = randomBytes(64 * 1024).toString("base64");
  files.set(
    "src/islands/Heavy.tsx",
    Buffer.from(
      `const DATA = "${blob}";\nexport default function Heavy() {\n  return <p data-heavy={DATA.length}>{DATA.slice(0, 8)}</p>;\n}\n`,
    ),
  );
  const index = files.get("src/pages/index.astro")?.toString() ?? "";
  files.set(
    "src/pages/index.astro",
    Buffer.from(
      index
        .replace(/^---\n/, '---\nimport Heavy from "../islands/Heavy";\n')
        .replace(/<\/Base>\s*$/, "<Heavy client:load /></Base>\n"),
    ),
  );
  const n = latest() + 1;
  await upload("heavy.tar.gz", archive(files));
  const result = await settled(n);
  expect(result.status).toBe("failed");
  expect(result.failures.join("\n")).toMatch(/budget \/: JS [0-9.]+ kB gz > 35/);
});
