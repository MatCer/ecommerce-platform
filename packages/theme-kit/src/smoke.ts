#!/usr/bin/env node
/**
 * Playwright smoke gate (spec §12.3): browse → category → product → add to cart → checkout
 * handoff lands on the checkout origin with the cart. Screenshots go to `--shots` for review.
 *
 *   node packages/theme-kit/src/smoke.ts --base http://demo.localhost:8280 [--shots .perf/shots]
 */
import { mkdir } from "node:fs/promises";
import { parseArgs } from "node:util";
import { chromium, expect } from "playwright/test";

const { values } = parseArgs({
  options: {
    base: { type: "string", default: "http://demo.localhost:8080" },
    shots: { type: "string", default: ".perf/shots" },
  },
});
await mkdir(values.shots, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({
  viewport: { width: 412, height: 900 },
  ignoreHTTPSErrors: true,
});
const errors: string[] = [];
page.on("console", (m) => {
  if (m.type() === "error") errors.push(m.text());
});
page.on("pageerror", (e) => errors.push(e.message));

try {
  await page.goto(new URL("/", values.base).href);
  await page.getByRole("button", { name: /Odmítnout|Reject/ }).click();
  await page.getByRole("navigation", { name: "Kategorie" }).getByRole("link").first().click();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/category.png` });

  await page.locator("main article a").first().click();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/product.png` });

  await page.getByRole("button", { name: /do košíku/ }).click();
  await expect(page.getByRole("dialog", { name: "Košík" })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/cart.png` });

  await page.getByRole("button", { name: /pokladně/ }).click();
  await page.waitForURL(/\/\/checkout\./);
  await expect(page.getByRole("heading", { name: "Souhrn" })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/checkout.png` });
  if (errors.length) throw new Error(`console errors:\n${errors.join("\n")}`);
  console.log(`smoke: ok (${page.url()})`);
} catch (err) {
  await page.screenshot({ path: `${values.shots}/failure.png` }).catch(() => {});
  console.error(`smoke: FAILED — ${err instanceof Error ? err.message : String(err)}`);
  process.exitCode = 1;
} finally {
  await browser.close();
}
