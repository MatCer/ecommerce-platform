/**
 * Playwright config for a revision's own functional checks (`checks/*.spec.ts`, WP23; the AI
 * loop of WP24 writes one per change). Runs inside the `functional` sandbox step against the
 * revision's preview: `baseURL` is the preview origin, the preview token travels as a cookie
 * header, and consent is decided (nothing granted) so the banner stays out of the way.
 *
 * A check is a plain Playwright test, e.g.
 *
 *   import { expect, test } from "@playwright/test";
 *   test("size guide opens", async ({ page }) => {
 *     await page.goto("/p/tricko-basic");
 *     await page.getByRole("button", { name: /Tabulka velikostí/ }).click();
 *     await expect(page.getByRole("dialog")).toBeVisible();
 *   });
 */
import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "/work/theme/checks",
  testMatch: "*.spec.ts",
  outputDir: "/tmp/functional-results",
  workers: 1,
  retries: 0,
  timeout: 30_000,
  reporter: [["list"]],
  use: {
    baseURL: process.env.PREVIEW_BASE ?? "https://preview.invalid",
    ignoreHTTPSErrors: true,
    viewport: { width: 412, height: 900 },
    extraHTTPHeaders: { cookie: `${process.env.PREVIEW_COOKIE ?? ""}; consent=` },
    launchOptions: {
      args: JSON.parse(process.env.THEME_KIT_CHROMIUM_ARGS ?? "[]") as string[],
    },
  },
});
