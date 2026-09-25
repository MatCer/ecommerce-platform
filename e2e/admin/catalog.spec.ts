/**
 * WP5 acceptance: staff sign-in by magic link, catalog creation (category, product with
 * variants and an uploaded image), staff invitation, role-aware UI, axe on the main screens.
 */

import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { testContext } from "../rate-client";
import {
  createTenant,
  expectAccessible,
  magicLink,
  pngFixture,
  resetLink,
  run,
  screenshot,
  totp,
  useEnglish,
} from "./support.ts";

test.describe.configure({ mode: "serial" });

const owner = `owner-${run}@example.test`;
const clerk = `clerk-${run}@example.test`;
const shop = `E2E Shop ${run}`;
const productName = `Tričko E2E ${run}`;

let context: BrowserContext;
let page: Page;
let productUrl = "";

const nav = (p: Page, name: string) =>
  p.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name, exact: true });

async function signInWithLink(p: Page, email: string): Promise<void> {
  await p.goto("/login");
  await expect(p.getByRole("heading", { name: "Sign in" })).toBeVisible();
  const since = new Date(Date.now() - 1000);
  await p.getByLabel("Email").fill(email);
  await p.getByRole("button", { name: "Email me a sign-in link" }).click();
  await expect(p.getByRole("status")).toContainText(email);
  await p.goto(await magicLink(email, since));
  await expect(p.getByRole("heading", { name: "Overview" })).toBeVisible();
}

test.beforeAll(async ({ browser }) => {
  createTenant(`e2e-${run}`, shop, owner);
  context = await testContext(browser);
  page = await context.newPage();
  await useEnglish(page);
});

test.afterAll(async () => {
  await context.close();
});

test("owner signs in with a magic link from Mailpit", async () => {
  await page.goto("/login");
  await expectAccessible(page, "login");
  await screenshot(page, "01-login");
  await signInWithLink(page, owner);
  await expect(page.getByText(`You are working in ${shop} as Owner.`)).toBeVisible();
  // Owners see the settings group, including staff and the audit log.
  const menu = page.getByRole("navigation", { name: "Main navigation" });
  await expect(menu.getByRole("link", { name: "Staff" })).toBeVisible();
  await expect(menu.getByRole("link", { name: "Audit log" })).toBeVisible();
  await expectAccessible(page, "dashboard");
  await screenshot(page, "02-dashboard");
});

test("creates a category", async () => {
  await nav(page, "Categories").click();
  await expect(page.getByText("No categories yet")).toBeVisible();
  await page.getByRole("button", { name: "New category" }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name (cs)").fill("Trička");
  await dialog.getByLabel("Name (en)").fill("T-shirts");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeHidden();
  await page.getByRole("button", { name: "New category" }).first().click();
  await dialog.getByLabel("Parent category").selectOption({ label: "T-shirts" });
  await dialog.getByLabel("Name (en)").fill("Basic");
  await dialog.getByRole("button", { name: "Create" }).click();
  const tree = page.getByRole("list", { name: "Category tree" });
  await expect(tree.getByText("Basic")).toBeVisible();
  // Keyboard reorder: outdent moves Basic to the top level; focus stays on the row's controls.
  const outdent = page.getByRole("button", { name: "Outdent (move up a level): Basic" });
  await outdent.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Category moved")).toBeVisible();
  await expect(outdent).toBeDisabled();
  const up = page.getByRole("button", { name: "Move up: Basic" });
  await expect(up).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(tree.getByRole("listitem").first()).toContainText("Basic");
  await expect(page.getByRole("button", { name: "Move down: Basic" })).toBeFocused();
  await expectAccessible(page, "categories");
  await screenshot(page, "03-categories");
});

test("creates a product with variants and an uploaded image", async () => {
  await nav(page, "Products").click();
  await expect(page.getByText("No products yet")).toBeVisible();
  await expectAccessible(page, "products-empty");
  await page.getByRole("link", { name: "New product" }).first().click();
  await expect(page.getByRole("heading", { name: "New product" })).toBeVisible();

  await page.getByLabel("Brand").fill("Acme");
  await page.getByLabel("Name *").fill(productName);
  await page.getByLabel("URL slug").click(); // filled from the name on focus
  await expect(page.getByLabel("URL slug")).toHaveValue(`tricko-e2e-${run}`);

  // Options: Size S / M -> two variants.
  await page.getByRole("button", { name: "Add option" }).click();
  await page.getByLabel("Option name (cs)").fill("Velikost");
  await page.getByLabel("Option name (en)").fill("Size");
  const values = page.getByRole("group", { name: "Size" });
  await values.getByLabel("Size: Values 1 (en)").fill("S");
  await values.getByRole("button", { name: "Add" }).click();
  await values.getByLabel("Size: Values 2 (en)").fill("M");
  await page.getByRole("button", { name: "Update variants from options" }).click();
  const variants = page.getByRole("table", { name: "Options and variants" });
  await expect(variants.getByRole("row")).toHaveCount(3);
  const skuS = variants.getByLabel("SKU: S");
  await expect(skuS).toHaveValue(/-S$/);
  await variants.getByLabel("EAN: S").fill("4006381333931");
  await variants.getByLabel("Weight (g): M").fill("180");

  await page.getByRole("region", { name: "Categories" }).getByText("T-shirts").click();
  await expect(page.getByRole("checkbox", { name: "T-shirts" })).toBeChecked();

  // Presigned upload -> worker -> ready.
  await page.locator("#media-upload").setInputFiles({
    name: "tricko.png",
    mimeType: "image/png",
    buffer: pngFixture(),
  });
  const image = page.getByRole("listitem", { name: "Image 1" });
  await expect(image.getByText("Ready")).toBeVisible({ timeout: 30_000 });
  await expect(image.getByRole("img")).toBeVisible();
  await image.getByLabel("Alt text (en)").fill("Red T-shirt");

  await page.getByRole("button", { name: "Save product" }).click();
  await expect(page.getByText("Product created")).toBeVisible();
  await expect(page).toHaveURL(/\/products\/[0-9a-f-]{36}$/);
  productUrl = new URL(page.url()).pathname;
  await expect(page.getByRole("heading", { name: "Edit product" })).toBeVisible();
  await expectAccessible(page, "product-editor");
  await screenshot(page, "04-product-editor");

  await page.getByRole("link", { name: "← Products" }).click();
  const row = page.getByRole("row", { name: new RegExp(productName) });
  await expect(row).toBeVisible();
  await expect(row.getByRole("cell", { name: "2", exact: true })).toBeVisible();
  // Search by SKU substring (trigram-indexed on the server).
  await page.getByLabel("Search by name or SKU").fill(`-M`);
  await expect(page).toHaveURL(/q=-M/);
  await expect(row).toBeVisible();
  await page.getByLabel("Search by name or SKU").fill("does-not-exist-xyz");
  await expect(page.getByText("No products match these filters.")).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(row).toBeVisible();
  await expectAccessible(page, "products");
  await screenshot(page, "05-products");

  // Tablet width: the layout keeps the navigation and the table usable.
  await page.setViewportSize({ width: 768, height: 1024 });
  await expect(nav(page, "Products")).toBeVisible();
  await expect(row).toBeVisible();
  await screenshot(page, "05b-products-tablet");
  await page.setViewportSize({ width: 1280, height: 860 });
});

test("keyboard only: edits and saves a product", async () => {
  await page.goto(productUrl);
  const brand = page.getByLabel("Brand");
  await brand.focus();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type("Acme Keyboard");
  const save = page.getByRole("button", { name: "Save product" });
  await save.focus();
  const saved = page.waitForResponse(
    (response) =>
      response.url().includes("/admin/v1/products/") && response.request().method() === "PUT",
  );
  await page.keyboard.press("Enter");
  expect((await saved).status()).toBe(200);
  await page.reload();
  await expect(brand).toHaveValue("Acme Keyboard");
});

test("sets up tax, a price list, variant prices, a sale, a coupon and stock", async () => {
  // Tax settings (owner/admin, fresh sign-in).
  await nav(page, "Tax settings").click();
  await expect(page.getByText("Tax settings are not configured yet")).toBeVisible();
  await page.getByLabel("VAT ID (DIČ)").fill("CZ12345678");
  await expectAccessible(page, "tax-settings");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Changes saved")).toBeVisible();
  await expect(page.getByText("Tax settings are not configured yet")).toBeHidden();
  await screenshot(page, "10-tax-settings");

  // A CZK price list for the Czech market.
  await nav(page, "Price lists").click();
  await expect(page.getByText("No price lists yet")).toBeVisible();
  await page.getByRole("button", { name: "New price list" }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill("Retail CZ");
  await dialog.getByLabel("Code").fill("retail-cz");
  await dialog.getByText("Česko (cz)").click();
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("cell", { name: "Retail CZ", exact: true })).toBeVisible();
  await expectAccessible(page, "price-lists");
  await screenshot(page, "11-price-lists");

  // Per-variant prices in the product editor.
  await page.goto(productUrl);
  const prices = page.getByRole("region", { name: "Prices" });
  await prices.getByLabel("Price (CZK): S", { exact: true }).fill("499");
  await prices.getByLabel("Price (CZK): M", { exact: true }).fill("549,90");
  await prices.getByRole("button", { name: /Save prices/ }).click();
  await expect(page.getByText("Prices saved")).toBeVisible();
  await expect(prices.getByRole("row", { name: /^S/ })).toContainText("499.00");

  // A running 10 % sale shows up in the prices and the Omnibus figures.
  await nav(page, "Sales").click();
  await page.getByRole("button", { name: "New sale" }).first().click();
  await dialog.getByLabel("Name").fill("Autumn");
  await dialog.getByLabel("Discount (%)").fill("10");
  await dialog.getByRole("button", { name: "Create" }).click();
  const sale = page.getByRole("row", { name: /Autumn/ });
  await expect(sale).toContainText("Running");
  await expectAccessible(page, "sales");
  await screenshot(page, "12-sales");
  await page.goto(productUrl);
  await expect(prices.getByRole("row", { name: /^S/ })).toContainText("On sale (−10 %)");
  await prices.getByText("Price history: S").click();
  await expect(prices.getByRole("cell", { name: "Sale", exact: true }).first()).toBeVisible();
  await expectAccessible(page, "product-prices");
  await prices.scrollIntoViewIfNeeded();
  await screenshot(page, "13-product-prices");

  // A coupon.
  await nav(page, "Coupons").click();
  await page.getByRole("button", { name: "New coupon" }).first().click();
  await dialog.getByLabel("Code").fill("welcome10");
  await dialog.getByLabel("Discount (%)").fill("10");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("cell", { name: "WELCOME10", exact: true })).toBeVisible();
  await expectAccessible(page, "coupons");
  await screenshot(page, "14-coupons");

  // Stock: adjust with a reason, then read it in the movement log.
  await page.goto(productUrl);
  await page.getByRole("link", { name: "Stock of this product" }).click();
  await expect(page.getByText("Showing one product.")).toBeVisible();
  await page.getByRole("button", { name: /^Adjust.*-S$/ }).click();
  await dialog.getByLabel("Units (+/−)").fill("+10");
  await dialog.getByLabel("Reason").fill("Initial stock");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Stock updated")).toBeVisible();
  const stock = page.getByRole("row", { name: /-S$/ });
  await expect(stock.getByRole("cell").nth(0)).toHaveText("10");
  await expectAccessible(page, "inventory");
  await screenshot(page, "15-inventory");
  await page.getByRole("button", { name: /^Movements.*-S$/ }).click();
  await expect(dialog.getByRole("row", { name: /Adjusted/ })).toContainText("Initial stock");
  await expect(dialog.getByRole("row", { name: /Adjusted/ })).toContainText("+10");
  await expectAccessible(page, "movements");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("invites a staff member by email", async () => {
  await nav(page, "Staff").click();
  await expect(page.getByRole("cell", { name: `${owner} (you)`, exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Invite member" }).click();
  const dialog = page.getByRole("dialog");
  const since = new Date(Date.now() - 1000);
  await dialog.getByLabel("Email").fill(clerk);
  await dialog.getByLabel("Role").selectOption("staff");
  await dialog.getByRole("button", { name: "Invite member" }).click();
  await expect(page.getByText(`Invitation sent to ${clerk}`)).toBeVisible();
  await expect(page.getByRole("cell", { name: clerk, exact: true })).toBeVisible();
  // The invitation is a sign-in link in Mailpit.
  expect(await magicLink(clerk, since)).toContain("/api/auth/magic-link/verify");
  await expectAccessible(page, "staff");
  await screenshot(page, "06-staff");

  await nav(page, "Audit log").click();
  await expect(page.getByRole("cell", { name: "staff.invited" })).toBeVisible();
  await expect(page.getByRole("cell", { name: "product.created" }).first()).toBeVisible();
  await expectAccessible(page, "audit-log");
  await screenshot(page, "07-audit-log");
});

test("a staff member sees only what their role allows", async ({ browser }) => {
  const ctx = await testContext(browser);
  const p = await ctx.newPage();
  await useEnglish(p);
  await signInWithLink(p, clerk);
  await expect(p.getByText(`You are working in ${shop} as Staff.`)).toBeVisible();
  const menu = p.getByRole("navigation", { name: "Main navigation" });
  await expect(menu.getByRole("link", { name: "Products" })).toBeVisible();
  await expect(menu.getByRole("link", { name: "Staff" })).toHaveCount(0);
  await expect(menu.getByRole("link", { name: "Audit log" })).toHaveCount(0);

  await p.goto("/staff");
  await expect(p.getByText("You don't have access")).toBeVisible();
  await p.goto("/markets");
  await expect(p.getByRole("button", { name: "New market" })).toBeDisabled();
  await expect(p.getByText("Only owners and admins can create markets.")).toBeVisible();
  await screenshot(p, "08-staff-role-markets");
  await p.goto("/settings/tax");
  await expect(p.getByText("Only owners and admins can change tax settings.")).toBeVisible();
  await expect(p.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await p.goto("/price-lists");
  await expect(p.getByRole("button", { name: "New price list" })).toBeDisabled();

  // Catalog work is allowed: the product created by the owner is visible.
  await p.goto("/products");
  await expect(p.getByRole("link", { name: productName })).toBeVisible();
  await ctx.close();
});

test("owner sets a password, turns on TOTP and signs in with password + code", async ({
  browser,
}) => {
  const password = `Correct horse ${run} battery`;
  await page.goto("/account/security");
  await expect(page.getByRole("heading", { name: "Security" })).toBeVisible();
  // Invited by magic link: no password yet, which two-factor needs.
  const since = new Date(Date.now() - 1000);
  await page.getByRole("button", { name: "Email me a link to set a password" }).click();
  await expect(page.getByText("Check your email for the link.")).toBeVisible();
  await page.goto(await resetLink(owner, since));
  await expect(page.getByRole("heading", { name: "Set a new password" })).toBeVisible();
  await page.getByLabel("New password").fill(password);
  await page.getByRole("button", { name: "Save password" }).click();
  await expect(page.getByText("Password saved.")).toBeVisible();

  // The reset revoked the sessions: sign in with the new password.
  await page.goto("/login");
  await page.getByLabel("Email").fill(owner);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();

  await page.goto("/account/security");
  await page.getByLabel("Current password").fill(password);
  await page.getByRole("button", { name: "Turn on two-factor" }).click();
  await expect(page.getByRole("img", { name: "QR code for your authenticator app" })).toBeVisible();
  const secret = await page.getByLabel("Secret key").inputValue();
  await expectAccessible(page, "security-enrollment");
  await screenshot(page, "09-security-2fa");
  await page.getByLabel("Code from the app").fill(await totp(secret));
  await page.getByRole("button", { name: "Activate" }).click();
  await expect(page.getByText("Two-factor authentication is on.")).toBeVisible();
  await expect(page.getByText("On", { exact: true })).toBeVisible();

  // A fresh browser: password, then the TOTP challenge; magic links are refused for 2FA users.
  const ctx = await testContext(browser);
  const p = await ctx.newPage();
  await useEnglish(p);
  await p.goto("/login");
  await p.getByLabel("Email").fill(owner);
  await p.getByLabel("Password").fill(password);
  await p.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(p.getByRole("heading", { name: "Two-factor authentication" })).toBeVisible();
  await expectAccessible(p, "two-factor");
  await p.getByLabel("Authentication code").fill("000000");
  await p.getByRole("button", { name: "Verify" }).click();
  await expect(p.getByRole("alert")).toContainText("The code is not valid");
  await p.getByLabel("Authentication code").fill(await totp(secret));
  await p.getByRole("button", { name: "Verify" }).click();
  await expect(p.getByRole("heading", { name: "Overview" })).toBeVisible();
  await ctx.close();
});
