/** Helpers for the storefront e2e suite (default theme on the seeded demo shop). */
import { mkdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import AxeBuilder from "@axe-core/playwright";
import { type BrowserContext, expect, type Page } from "@playwright/test";

const root = join(dirname(fileURLToPath(import.meta.url)), "../..");

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

const port = env("HTTP_PORT", "8080");
/** The seeded shops (`make seed`): CZ (cs, CZK) and SK (sk + cs under /cs, EUR). */
export const CZ = `http://demo.localhost:${port}`;
export const SK = `http://demo-sk.localhost:${port}`;

/** A decided consent (nothing granted) so the banner stays closed. */
export async function decideConsent(ctx: BrowserContext, base = CZ, purposes = "") {
  await ctx.addCookies([{ name: "consent", value: purposes, url: base }]);
}

/**
 * A recorded consent choice through the edge (`POST /_p/consent`), as the banner makes it: the
 * server-side record (A20), the HttpOnly subject cookie and the script-readable summary.
 */
export async function grantConsent(page: Page, base: string, granted: string[]) {
  const shop = (await (await page.request.get(`${base}/_p/public/shop`)).json()) as {
    consent: { text_version: string };
  };
  const purposes = Object.fromEntries(
    ["analytics", "ads", "personalization"].map((p) => [p, granted.includes(p)]),
  );
  const res = await page.request.post(`${base}/_p/consent`, {
    headers: { origin: base },
    data: { purposes, text_version: shop.consent.text_version, source: "banner" },
  });
  expect(res.status()).toBe(200);
}

/** WCAG 2.0-2.2 A/AA automated checks: no serious or critical violations (spec §9.6). */
export async function expectAccessible(page: Page, name: string) {
  const result = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  const bad = result.violations
    .filter((v) => v.impact === "serious" || v.impact === "critical")
    .map((v) => `${v.id} (${v.impact}): ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(bad, `axe on ${name}`).toEqual([]);
}

/** Waits until every `client:idle` island has hydrated (Astro drops `ssr` when done). */
export async function hydrated(page: Page) {
  await page.waitForFunction(() => !document.querySelector('astro-island[client="idle"][ssr]'));
}

/** Screenshots for the PR (`WP8_SCREENSHOTS=1 make e2e args=storefront`). */
export async function screenshot(page: Page, name: string) {
  if (!process.env.WP8_SCREENSHOTS) return;
  const dir = join(root, "docs/screenshots/wp8");
  mkdirSync(dir, { recursive: true });
  // JPEG: the grainy demo photos make full-page PNGs ~1 MB each.
  await page.screenshot({
    path: join(dir, `${name}.jpg`),
    fullPage: true,
    type: "jpeg",
    quality: 72,
  });
}
