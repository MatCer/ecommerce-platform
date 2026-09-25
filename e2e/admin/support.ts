/** Helpers for the admin e2e suite: stack env, superadmin CLI, Mailpit, axe, screenshots. */
import { execFileSync } from "node:child_process";
import { createHmac } from "node:crypto";
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
export function magicLink(to: string, since: Date): Promise<string> {
  return mailedLink(to, since, /https?:\/\/[^\s"'<>]+magic-link\/verify[^\s"'<>]*/);
}

/** The newest password-reset link mailed to `to` after `since`. */
export function resetLink(to: string, since: Date): Promise<string> {
  return mailedLink(to, since, /https?:\/\/[^\s"'<>]+\/reset-password\/[^\s"'<>]*/);
}

async function mailedLink(to: string, since: Date, pattern: RegExp): Promise<string> {
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
      const link = `${msg.Text}\n${msg.HTML}`.match(pattern);
      if (link) return link[0].replace(/&amp;/g, "&");
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no link matching ${pattern} for ${to}`);
}

/** WCAG 2.0-2.2 A/AA automated checks: no violations of any impact. */
export async function expectAccessible(page: Page, name: string): Promise<void> {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  const bad = result.violations.map(
    (v) => `${v.id} (${v.impact}): ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`,
  );
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

function base32(secret: string): Buffer {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of secret.replace(/=+$/, "").toUpperCase()) {
    const v = alphabet.indexOf(c);
    if (v < 0) throw new Error("not base32");
    bits += v.toString(2).padStart(5, "0");
  }
  const bytes = bits.match(/.{8}/g) ?? [];
  return Buffer.from(bytes.map((b) => Number.parseInt(b, 2)));
}

let lastStep = -1;

/**
 * RFC 6238 TOTP (SHA-1, 30 s, 6 digits), as an authenticator app computes it. Each call uses a
 * fresh time step (waits for the next one if needed) so a code is never reused.
 */
export async function totp(secret: string): Promise<string> {
  let step = Math.floor(Date.now() / 30_000);
  if (step <= lastStep) {
    await new Promise((r) => setTimeout(r, (lastStep + 1) * 30_000 - Date.now() + 200));
    step = lastStep + 1;
  }
  lastStep = step;
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(step));
  const hmac = createHmac("sha1", base32(secret)).update(counter).digest();
  const offset = (hmac[hmac.length - 1] ?? 0) & 0xf;
  const value = hmac.readUInt32BE(offset) & 0x7fffffff;
  return String(value % 1_000_000).padStart(6, "0");
}

/**
 * A read (or a job enqueue) as the database superuser in the postgres container, for
 * assertions the UI does not show (WP12: invoice dates, stock). Returns psql's unaligned output.
 */
export function sql(query: string): string {
  return execFileSync(
    "docker",
    [
      "compose",
      "exec",
      "-T",
      "postgres",
      "psql",
      "-U",
      "postgres",
      "-d",
      "app",
      "-At",
      "-c",
      query,
    ],
    { cwd: root, env: { ...process.env, COMPOSE_PROFILES: "full" }, encoding: "utf8" },
  ).trim();
}

/** Runs a worker job now instead of waiting for its cron slot (e.g. `shipping.track`). */
export function enqueueJob(kind: string): void {
  if (!/^[a-z_.]+$/.test(kind)) throw new Error("bad job kind");
  sql(
    `SELECT queue.enqueue('${kind}', '{}'::jsonb, NULL, 'default', NULL, 3, 'e2e:${kind}:${Date.now()}')`,
  );
}
