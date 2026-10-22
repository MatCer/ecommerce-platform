import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { testContext } from "../rate-client";
import { createTenant, expectAccessible, magicLink, run, sql, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });
const owner = `store-owner-${run}@example.test`;
const slug = `store-${run}`;
let tenant: string;
let context: BrowserContext;
let page: Page;
const nav = (name: string) =>
  page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name, exact: true });
const findings = () => page.getByRole("region", { name: "Your current shop", exact: true });
const count = (term: string) =>
  findings()
    .locator("dl > div")
    .filter({ has: page.getByRole("term").filter({ hasText: new RegExp(`^${term}$`) }) })
    .getByRole("definition");

test.beforeAll(async ({ browser }) => {
  tenant = createTenant(slug, `Store ${run}`, owner).tenant_id;
  const [list] = sql(
    `INSERT INTO price_lists (tenant_id, code, name, currency) VALUES ('${tenant}', 'cz', 'CZ', 'CZK') RETURNING id`,
  ).split("\n");
  sql(`UPDATE markets SET price_list_id = '${list}' WHERE tenant_id = '${tenant}'`);
  context = await testContext(browser);
  page = await context.newPage();
  await useEnglish(page);
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
});
test.afterAll(async () => {
  await context?.close();
});

test("refuses a plain http shop address", async () => {
  await nav("Imports").click();
  await expect(
    page.getByRole("heading", { name: "Import from your current shop", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("textbox", { name: "Shop address", exact: true })
    .fill("http://shop.example/");
  await page.getByRole("button", { name: "Read my shop", exact: true }).click();
  await expect(page.getByText("The shop's address must start with https://.")).toBeVisible();
  expect(sql(`SELECT count(*) FROM import_runs WHERE tenant_id = '${tenant}'`)).toBe("0");
});

test("reads the old shop and shows its brand, feed and problems", async () => {
  test.setTimeout(240_000);
  await expectAccessible(page, "store-import-form");
  await page.getByRole("textbox", { name: "Shop address", exact: true }).fill("http://mocks:4010/");
  await page.getByRole("button", { name: "Read my shop", exact: true }).click();
  await expect(findings()).toBeVisible({ timeout: 120_000 });
  await expect(findings().getByText("Stará Kočka", { exact: true })).toBeVisible();
  await expect(page.getByText("Your shop links to what looks like a product feed")).toBeVisible();
  await expect(count("Product pages in the sitemap")).toHaveText("5");
  await expect(count("Pages read")).toHaveText("5");
  const problems = page.getByRole("table", { name: "Problems", exact: true });
  await expect(problems).toContainText("invalid_json_ld");
  await expect(problems).toContainText("currency_mismatch");
  expect(sql(`SELECT count(*) FROM products WHERE tenant_id = '${tenant}'`)).toBe("0");
  await expect(findings().getByRole("button", { name: "Use as email logo" })).toBeEnabled({
    timeout: 60_000,
  });
  await expectAccessible(page, "store-import-run");
  await page.screenshot({ path: test.info().outputPath("run-desktop.png"), fullPage: true });
  await page.setViewportSize({ width: 375, height: 812 });
  await page.screenshot({ path: test.info().outputPath("run-phone.png"), fullPage: true });
  await page.setViewportSize({ width: 1280, height: 720 });
});

test("applies the name and the logo only when asked", async () => {
  expect(sql(`SELECT name FROM platform.tenants WHERE id = '${tenant}'`)).toBe(`Store ${run}`);
  await findings().getByRole("button", { name: "Use as shop name", exact: true }).click();
  await expect(page.getByText("Shop name updated.")).toBeVisible();
  expect(sql(`SELECT name FROM platform.tenants WHERE id = '${tenant}'`)).toBe("Stará Kočka");
  await findings().getByRole("button", { name: "Use as email logo", exact: true }).click();
  await expect(page.getByText("Email logo updated.")).toBeVisible();
  await expect(findings().getByText("In use", { exact: true })).toHaveCount(2);
  expect(
    sql(`SELECT logo_asset_id IS NOT NULL FROM email_settings WHERE tenant_id = '${tenant}'`),
  ).toBe("t");
  expect(
    sql(
      `SELECT count(*) FROM audit_log WHERE tenant_id = '${tenant}' AND action = 'import.brand_applied'`,
    ),
  ).toBe("2");
});

test("applies the import as drafts with prices and safe descriptions", async () => {
  test.setTimeout(240_000);
  await page.getByRole("button", { name: "Apply import", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Products are created as drafts");
  await dialog.getByRole("button", { name: "Apply import", exact: true }).click();
  await expect(page.getByText("Applied", { exact: true })).toBeVisible({ timeout: 120_000 });
  expect(
    sql(
      `SELECT string_agg(status, ',' ORDER BY status) FROM products WHERE tenant_id = '${tenant}'`,
    ),
  ).toBe("draft,draft,draft,draft");
  expect(
    sql(
      `SELECT p.amount_minor FROM variant_prices p JOIN variants v ON v.id = p.variant_id WHERE v.tenant_id = '${tenant}' AND v.sku = 'SK-MIKINA-CREW'`,
    ),
  ).toBe("129050");
  const description = sql(
    `SELECT t.description_html FROM product_translations t JOIN variants v ON v.product_id = t.product_id WHERE v.tenant_id = '${tenant}' AND v.sku = 'SK-CEPICE-OLIVE'`,
  );
  expect(description).toContain("Hřejivá čepice.");
  for (const vector of ["<script", "onclick", "onerror", "javascript:"])
    expect(description).not.toContain(vector);
});
