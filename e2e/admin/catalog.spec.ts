/**
 * WP5 acceptance: staff sign-in by magic link, catalog creation (category, product with
 * variants and an uploaded image), staff invitation, role-aware UI, axe on the main screens.
 */
import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import {
  createTenant,
  expectAccessible,
  magicLink,
  pngFixture,
  run,
  screenshot,
  useEnglish,
} from "./support.ts";

test.describe.configure({ mode: "serial" });

const owner = `owner-${run}@example.test`;
const clerk = `clerk-${run}@example.test`;
const shop = `E2E Shop ${run}`;
const productName = `Tričko E2E ${run}`;

let context: BrowserContext;
let page: Page;

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
  context = await browser.newContext();
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
  const nav = page.getByRole("navigation", { name: "Main navigation" });
  await expect(nav.getByRole("link", { name: "Staff" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Audit log" })).toBeVisible();
  await expectAccessible(page, "dashboard");
  await screenshot(page, "02-dashboard");
});

test("creates a category", async () => {
  await page.getByRole("link", { name: "Categories" }).click();
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
  // Keyboard-accessible reorder: outdent moves Basic to the top level.
  await page.getByRole("button", { name: "Outdent (move up a level): Basic" }).click();
  await expect(page.getByText("Category moved")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Outdent (move up a level): Basic" }),
  ).toBeDisabled();
  await expectAccessible(page, "categories");
  await screenshot(page, "03-categories");
});

test("creates a product with variants and an uploaded image", async () => {
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Products" })
    .click();
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

  await page.getByRole("checkbox", { name: "T-shirts" }).check();

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
});

test("invites a staff member by email", async () => {
  await page.getByRole("link", { name: "Staff" }).click();
  await expect(page.getByRole("cell", { name: new RegExp(owner) })).toBeVisible();
  await page.getByRole("button", { name: "Invite member" }).click();
  const dialog = page.getByRole("dialog");
  const since = new Date(Date.now() - 1000);
  await dialog.getByLabel("Email").fill(clerk);
  await dialog.getByLabel("Role").selectOption("staff");
  await dialog.getByRole("button", { name: "Invite member" }).click();
  await expect(page.getByText(`Invitation sent to ${clerk}`)).toBeVisible();
  await expect(page.getByRole("cell", { name: clerk })).toBeVisible();
  // The invitation is a sign-in link in Mailpit.
  expect(await magicLink(clerk, since)).toContain("/api/auth/magic-link/verify");
  await expectAccessible(page, "staff");
  await screenshot(page, "06-staff");

  await page.getByRole("link", { name: "Audit log" }).click();
  await expect(page.getByRole("cell", { name: "staff.invited" })).toBeVisible();
  await expect(page.getByRole("cell", { name: "product.created" }).first()).toBeVisible();
  await expectAccessible(page, "audit-log");
  await screenshot(page, "07-audit-log");
});

test("a staff member sees only what their role allows", async ({ browser }) => {
  const ctx = await browser.newContext();
  const p = await ctx.newPage();
  await useEnglish(p);
  await signInWithLink(p, clerk);
  await expect(p.getByText(`You are working in ${shop} as Staff.`)).toBeVisible();
  const nav = p.getByRole("navigation", { name: "Main navigation" });
  await expect(nav.getByRole("link", { name: "Products" })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Staff" })).toHaveCount(0);
  await expect(nav.getByRole("link", { name: "Audit log" })).toHaveCount(0);

  await p.goto("/staff");
  await expect(p.getByText("You don't have access")).toBeVisible();
  await p.goto("/markets");
  await expect(p.getByRole("button", { name: "New market" })).toBeDisabled();
  await expect(p.getByText("Only owners and admins can create markets.")).toBeVisible();
  await screenshot(p, "08-staff-role-markets");

  // Catalog work is allowed: the product created by the owner is visible.
  await p.goto("/products");
  await expect(p.getByRole("link", { name: productName })).toBeVisible();
  await ctx.close();
});
