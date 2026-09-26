import { type Browser, chromium } from "playwright";
import { afterAll, beforeAll, expect, test } from "vitest";
import { scanAxe } from "./axe.ts";

let browser: Browser;
beforeAll(async () => {
  browser = await chromium.launch();
});
afterAll(async () => {
  await browser?.close();
});

// Real Chromium: three full scans plus six centered target checks measured 3.52 s
// locally (3.39 s with target-only rechecks), and exceeded 5 s in CI run 36220248489.
// Keep this browser integration budget local; ordinary unit tests retain 5 s.
test("a faulty sticky header fails the accessibility scan without altering its layout", {
  timeout: 15_000,
}, async () => {
  const context = await browser.newContext({ viewport: { width: 412, height: 400 } });
  const page = await context.newPage();
  await page.setContent(`<!doctype html><html lang="en"><head><title>Sticky fixture</title>
    <style>body{margin:0}header{position:sticky;top:0;height:70px;background:white}
    button{width:28px;height:28px;padding:0}main{height:800px}</style></head><body>
    <header><button aria-label="Menu" style="position:absolute;top:45px;left:10px;width:10px;height:10px"></button><button aria-label="Search" style="position:absolute;top:45px;left:25px;width:10px;height:10px"></button></header>
    <main><div style="height:200px"></div><button aria-label="Filter" style="margin-left:10px">F</button></main>
    </body></html>`);
  const failures = await scanAxe(page);
  expect(await page.locator("header").evaluate((el) => getComputedStyle(el).position)).toBe(
    "sticky",
  );
  expect(
    failures.some((failure) => failure.id === "target-size"),
    JSON.stringify(failures),
  ).toBe(true);
  await context.close();
});

test("a footer target clipped at one scroll position is checked again when centered", async () => {
  const context = await browser.newContext({ viewport: { width: 412, height: 400 } });
  const page = await context.newPage();
  await page.setContent(`<!doctype html><html lang="en"><head><title>Footer fixture</title>
    <style>body{margin:0}header{position:sticky;top:0;height:80px;background:white}
    main{height:420px}footer{padding-bottom:560px}a{display:inline-flex;min-height:32px;align-items:center}</style>
    </head><body><header>Shop</header><main>Products</main>
    <footer><a href="/legal">Cookie policy</a></footer></body></html>`);
  expect(await scanAxe(page)).toEqual([]);
  expect(await page.locator("header").evaluate((el) => getComputedStyle(el).position)).toBe(
    "sticky",
  );
  await context.close();
});

test("a page that fits the viewport still checks rules other than target size", async () => {
  const context = await browser.newContext({ viewport: { width: 412, height: 400 } });
  const page = await context.newPage();
  await page.setContent(`<!doctype html><html lang="en"><head><title>Short fixture</title>
    </head><body><main><button style="width:44px;height:44px"></button></main></body></html>`);
  const failures = await scanAxe(page);
  expect(failures.some((failure) => failure.id === "button-name")).toBe(true);
  await context.close();
});
