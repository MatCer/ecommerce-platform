#!/usr/bin/env node
/**
 * Playwright smoke gate (spec §12.3): browse → category → product → add to cart → checkout
 * handoff lands on the checkout origin with the cart. Every page must also stay within the
 * storefront-calls budget (an uncached render's `x-edge-subrequests`, N+1 guard). Screenshots
 * go to `--shots` for review.
 *
 *   node packages/theme-kit/src/smoke.ts --base http://demo.localhost:8280 [--shots .perf/shots]
 */
import { mkdir } from "node:fs/promises";
import { parseArgs } from "node:util";
import { chromium, expect, type Page } from "playwright/test";
import { BUDGET } from "./budget.ts";

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

/** Page-model calls of a fresh render of the current URL (Authorization bypasses the cache). */
const calls: string[] = [];
async function budget(p: Page) {
  const res = await p.request.get(p.url(), { headers: { authorization: "Bearer smoke" } });
  const n = Number(res.headers()["x-edge-subrequests"]);
  calls.push(`${new URL(p.url()).pathname}=${n}`);
  if (!(n <= BUDGET.maxSubrequests))
    throw new Error(`${p.url()}: ${n} storefront calls per render > ${BUDGET.maxSubrequests}`);
}

try {
  await page.goto(new URL("/", values.base).href);
  await budget(page);
  await page.getByRole("button", { name: /Odmítnout|Odmietnuť|Reject/ }).click();
  await page
    .getByRole("navigation", { name: /^(Kategorie|Kategórie|Categories)$/ })
    .getByRole("link")
    .first()
    .click();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/category.png` });
  await budget(page);

  await page.locator("main article a").first().click();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/product.png` });
  await budget(page);

  // .first(): a theme may repeat the buy button in a sticky bar on phones.
  await page
    .getByRole("button", { name: /do košíku|do košíka|to cart/i })
    .first()
    .click();
  await expect(page.getByRole("dialog", { name: /Košík|Cart/ })).toBeVisible();
  await page.screenshot({ path: `${values.shots}/cart.png` });

  // The platform's own view of the cart, before the handoff rotates the shop capability.
  const before = (await (await page.request.get(new URL("/_p/cart", page.url()).href)).json()) as {
    total?: { formatted?: string };
    vat?: { rate: string }[];
  };
  await page.getByRole("button", { name: /pokladně|pokladni|checkout/i }).click();
  await page.waitForURL(/\/\/checkout\./);
  await expect(page.getByRole("heading", { name: /Souhrn|Súhrn|Summary/ })).toBeVisible();
  // The checkout origin shows the same cart: same total (incl. VAT) and the VAT recap.
  await expect(page.locator("[data-cart-total]")).toHaveText(before.total?.formatted ?? "?");
  for (const row of before.vat ?? [])
    await expect(page.locator(`[data-vat-rate="${row.rate}"]`)).toBeVisible();
  await page.screenshot({ path: `${values.shots}/checkout.png` });
  if (errors.length) throw new Error(`console errors:\n${errors.join("\n")}`);
  console.log(`smoke: ok (${page.url()}; calls ${calls.join(" ")})`);
} catch (err) {
  await page.screenshot({ path: `${values.shots}/failure.png` }).catch(() => {});
  console.error(`smoke: FAILED — ${err instanceof Error ? err.message : String(err)}`);
  process.exitCode = 1;
} finally {
  await browser.close();
}
