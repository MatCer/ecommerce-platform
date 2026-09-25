#!/usr/bin/env node
/**
 * Playwright smoke gate (spec §12.3): browse → category → product → add to cart → checkout
 * handoff lands on the checkout origin with the cart. Every page must also stay within the
 * storefront-calls budget (an uncached render's `x-edge-subrequests`, N+1 guard). Screenshots
 * go to `--shots` for review.
 *
 *   node packages/theme-kit/src/smoke.ts --base http://demo.localhost:8280 [--shots .perf/shots]
 *
 * WP23 (theme builder, preview hosts): `--preview` expects the edge's "checkout is disabled in
 * preview" page instead of the checkout origin; `--cookie name=value` carries the preview token;
 * `--pages /,/c/x,/p/y` adds merchant screenshots `<home|category|product>-<mobile|desktop>.png`.
 */
import { mkdir } from "node:fs/promises";
import { parseArgs } from "node:util";
import { chromium, expect, type Page } from "playwright/test";
import { addCookie, chromiumArgs, freshSubrequests, parseCookie } from "./browser.ts";
import { BUDGET } from "./budget.ts";

const { values } = parseArgs({
  options: {
    base: { type: "string", default: "http://demo.localhost:8080" },
    shots: { type: "string", default: ".perf/shots" },
    preview: { type: "boolean", default: false },
    cookie: { type: "string" },
    pages: { type: "string" },
  },
});
await mkdir(values.shots, { recursive: true });
const base = new URL(values.base);
const cookie = parseCookie(values.cookie);

const browser = await chromium.launch({ args: chromiumArgs() });
const context = await browser.newContext({
  viewport: { width: 412, height: 900 },
  ignoreHTTPSErrors: true,
});
await addCookie(context, base, cookie);
const page = await context.newPage();
const errors: string[] = [];
page.on("console", (m) => {
  if (m.type() === "error") errors.push(m.text());
});
page.on("pageerror", (e) => errors.push(e.message));

/** Page-model calls of a fresh render of the current URL (Authorization bypasses the cache). */
const calls: string[] = [];
async function budget(p: Page) {
  const n = await freshSubrequests(p, p.url());
  calls.push(`${new URL(p.url()).pathname}=${n}`);
  if (!(n <= BUDGET.maxSubrequests))
    throw new Error(`${p.url()}: ${n} storefront calls per render > ${BUDGET.maxSubrequests}`);
}

try {
  await page.goto(new URL("/", base).href);
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
  const before = (await page.evaluate(async () => (await fetch("/_p/cart")).json())) as {
    total?: { formatted?: string };
    vat?: { rate: string }[];
  };
  if (!before.total?.formatted) throw new Error("the cart is empty after adding a product");
  await page.getByRole("button", { name: /pokladně|pokladni|checkout/i }).click();
  if (values.preview) {
    // Previews never hand a cart to the real checkout (A21): the edge answers with a notice.
    await page.waitForURL(/\/_p\/checkout\/start$/);
    await expect(page.getByRole("heading", { name: /preview/i })).toBeVisible();
  } else {
    await page.waitForURL(/\/\/checkout\./);
    await expect(page.getByRole("heading", { name: /Souhrn|Súhrn|Summary/ })).toBeVisible();
    // The checkout origin shows the same cart: same total (incl. VAT) and the VAT recap.
    await expect(page.locator("[data-cart-total]")).toHaveText(before.total.formatted);
    for (const row of before.vat ?? [])
      await expect(page.locator(`[data-vat-rate="${row.rate}"]`)).toBeVisible();
  }
  await page.screenshot({ path: `${values.shots}/checkout.png` });
  if (errors.length) throw new Error(`console errors:\n${errors.join("\n")}`);
  await screenshots();
  console.log(`smoke: ok (${page.url()}; calls ${calls.join(" ")})`);
} catch (err) {
  await page.screenshot({ path: `${values.shots}/failure.png` }).catch(() => {});
  console.error(`smoke: FAILED — ${err instanceof Error ? err.message : String(err)}`);
  process.exitCode = 1;
} finally {
  await browser.close();
}

/** Merchant screenshots (WP23): each `--pages` entry on a phone and a desktop viewport. */
async function screenshots() {
  const pages = (values.pages ?? "").split(",").filter(Boolean);
  const kinds = ["home", "category", "product"] as const;
  for (const [viewport, size] of [
    ["mobile", { width: 412, height: 900 }],
    ["desktop", { width: 1280, height: 860 }],
  ] as const) {
    const ctx = await browser.newContext({ viewport: size, ignoreHTTPSErrors: true });
    await addCookie(ctx, base, cookie);
    // A decided consent (nothing granted): the banner does not cover the page.
    await ctx.addCookies([{ name: "consent", value: "", url: base.origin }]);
    const p = await ctx.newPage();
    for (const [i, path] of pages.slice(0, 3).entries()) {
      await p.goto(new URL(path, base).href, { waitUntil: "networkidle" });
      await p.screenshot({ path: `${values.shots}/${kinds[i]}-${viewport}.png` });
    }
    await ctx.close();
  }
}
