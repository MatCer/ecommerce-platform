/** Helpers for the admin e2e suite: stack env, superadmin CLI, Mailpit, axe, screenshots. */
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { crc32, deflateSync } from "node:zlib";
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page } from "@playwright/test";

export const root = join(dirname(fileURLToPath(import.meta.url)), "../..");

/** `.env` values (the Makefile and compose read the same file); the process env wins. */
function env(name: string, fallback: string): string {
  if (process.env[name]) return process.env[name];
  try {
    const line = readFileSync(join(root, ".env"), "utf8")
      .split("\n")
      .find((l) => l.startsWith(`${name}=`));
    if (line) return line.slice(name.length + 1).trim();
  } catch {
    // No .env: defaults.
  }
  return fallback;
}

export const mailpit = `http://127.0.0.1:${env("MAILPIT_UI_PORT", "58025")}`;
export const run = Date.now().toString(36);

/** Superadmin CLI in the api container (like `make admin args=...`). */
export function createTenant(
  slug: string,
  name: string,
  ownerEmail: string,
): { tenant_id: string } {
  const out = execFileSync(
    "docker",
    [
      "compose",
      "exec",
      "-T",
      "api",
      "/usr/local/bin/api",
      "admin",
      "create-tenant",
      "--slug",
      slug,
      "--name",
      name,
      "--owner-email",
      ownerEmail,
    ],
    { cwd: root, env: { ...process.env, COMPOSE_PROFILES: "full" }, encoding: "utf8" },
  );
  return JSON.parse(out) as { tenant_id: string };
}

interface MailSummary {
  ID: string;
  Created: string;
}

/** The newest sign-in link mailed to `to` after `since` (polls Mailpit for up to 15 s). */
export async function magicLink(to: string, since: Date): Promise<string> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const body = (await res.json()) as { messages: MailSummary[] };
    const fresh = body.messages
      .filter((m) => new Date(m.Created) >= since)
      .sort((a, b) => b.Created.localeCompare(a.Created))[0];
    if (fresh) {
      const msg = (await (await fetch(`${mailpit}/api/v1/message/${fresh.ID}`)).json()) as {
        Text: string;
        HTML: string;
      };
      const link = `${msg.Text}\n${msg.HTML}`.match(
        /https?:\/\/[^\s"'<>]+magic-link\/verify[^\s"'<>]*/,
      );
      if (link) return link[0].replace(/&amp;/g, "&");
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no sign-in link for ${to}`);
}

/** WCAG 2.2 A/AA automated checks: no serious or critical violations. */
export async function expectAccessible(page: Page, name: string): Promise<void> {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  const bad = result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id} (${v.impact}): ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(bad, `axe on ${name}`).toEqual([]);
}

/** Viewport screenshots for the PR, only when WP5_SCREENSHOTS is set. */
export async function screenshot(page: Page, name: string): Promise<void> {
  if (!process.env.WP5_SCREENSHOTS) return;
  const dir = join(root, "docs/screenshots/wp5");
  mkdirSync(dir, { recursive: true });
  await page.screenshot({ path: join(dir, `${name}.png`) });
}

/** A small valid RGB PNG (gradient) for the upload flow. */
export function pngFixture(width = 96, height = 64): Buffer {
  const chunk = (type: string, data: Buffer) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(type, 4, "ascii");
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])) >>> 0, 0);
    return Buffer.concat([head, data, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.writeUInt8(8, 8); // bit depth
  ihdr.writeUInt8(2, 9); // RGB
  const rows: number[] = [];
  for (let y = 0; y < height; y++) {
    rows.push(0); // filter: none
    for (let x = 0; x < width; x++) rows.push((x * 255) / width, (y * 255) / height, 160);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(Buffer.from(rows))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** English UI for stable selectors. */
export async function useEnglish(page: Page): Promise<void> {
  await page.addInitScript(() => localStorage.setItem("admin.locale", "en"));
}
