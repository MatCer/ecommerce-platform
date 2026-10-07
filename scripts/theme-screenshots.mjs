#!/usr/bin/env node
// Before/after screenshots of the storefront theme: home, category, PDP and the open cart
// drawer at 390px and 1440px (SHOP-75, SHOP-89). Needs a running, seeded stack.
//   node scripts/theme-screenshots.mjs [outDir] [baseUrl]
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

const require = createRequire(new URL("../e2e/package.json", import.meta.url));
const { chromium } = require("@playwright/test");

const out = process.argv[2] ?? "docs/screenshots/conversion";
const base = process.argv[3] ?? "http://demo.localhost:8080";
const pages = { home: "/", category: "/c/obleceni", product: "/p/mikina-fleece" };
const widths = { mobile: [390, 844], desktop: [1440, 900] };

mkdirSync(out, { recursive: true });
const browser = await chromium.launch();
for (const [label, [width, height]] of Object.entries(widths)) {
  const context = await browser.newContext({ viewport: { width, height }, reducedMotion: "reduce" });
  // An empty consent decision keeps the banner from covering the page.
  await context.addCookies([{ name: "consent", value: "", url: base }]);
  const page = await context.newPage();
  const shoot = (name) =>
    page.screenshot({ path: join(out, `${name}-${label}.jpg`), fullPage: true, type: "jpeg", quality: 80 });
  for (const [name, path] of Object.entries(pages)) {
    await page.goto(base + path, { waitUntil: "networkidle" });
    await page.waitForFunction(() => !document.querySelector("astro-island[ssr]"));
    // Lazy images below the fold load only when scrolled into view.
    await page.evaluate(async () => {
      for (let y = 0; y < document.body.scrollHeight; y += 600) {
        window.scrollTo(0, y);
        await new Promise((r) => setTimeout(r, 120));
      }
      window.scrollTo(0, 0);
    });
    await page.waitForLoadState("networkidle");
    await shoot(name);
  }
  await page.getByRole("main").getByRole("button", { name: "Přidat do košíku" }).first().click();
  await page.getByRole("dialog", { name: /Košík/ }).waitFor();
  await page.waitForTimeout(400);
  await page.screenshot({ path: join(out, `cart-${label}.jpg`), type: "jpeg", quality: 80 });
  await context.close();
}
await browser.close();
console.log(`screenshots in ${out}`);
