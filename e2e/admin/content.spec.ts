/** WP13a: owner content, legal readiness, feed migration and search settings. */
import { join } from "node:path";
import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { createTenant, expectAccessible, magicLink, root, run, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });
const owner = `content-owner-${run}@example.test`;
let context: BrowserContext;
let page: Page;
const nav = (name: string) =>
  page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name, exact: true });
async function saved() {
  await expect(page.getByText("Changes saved", { exact: true }).last()).toBeVisible();
}
async function publish() {
  await page.getByLabel("Status", { exact: true }).selectOption("published");
  await page.getByRole("button", { name: "Save", exact: true }).click();
}
async function addBlock(name: string) {
  await page.getByRole("button", { name: "Add block", exact: true }).click();
  await page.getByRole("menuitem", { name, exact: true }).click();
}

test.beforeAll(async ({ browser }) => {
  createTenant(`content-${run}`, `Content ${run}`, owner);
  context = await browser.newContext();
  page = await context.newPage();
  await useEnglish(page);
});
test.afterAll(async () => {
  await context?.close();
});

test("owner saves legal entity and installs Czech templates", async () => {
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email", { exact: true }).fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await expect(page.getByRole("status")).toContainText(owner);
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
  await nav("Legal & go-live").click();
  await expect(page.getByText("Not ready to launch", { exact: true })).toBeVisible();
  await expectAccessible(page, "content-legal");
  const form = page.getByRole("region", { name: "Legal entity", exact: true });
  for (const [label, value] of Object.entries({
    "Company name": "Content s.r.o.",
    "Company ID": "12345678",
    Street: "Dlouhá 1",
    City: "Praha",
    "Postal code": "11000",
    "Country (ISO code)": "CZ",
    Email: "shop@example.test",
    Phone: "+420123456789",
    "Registry entry": "Městský soud v Praze, oddíl C, vložka 123",
    "Returns address": "Dlouhá 1, 11000 Praha",
  }))
    await form.getByLabel(label, { exact: true }).fill(value);
  await form.getByRole("button", { name: "Save", exact: true }).click();
  await saved();
  const install = page.getByRole("region", { name: "Install legal templates", exact: true });
  await install.getByRole("checkbox", { name: "CS", exact: true }).check();
  await install.getByRole("checkbox", { name: "SK", exact: true }).uncheck();
  await install.getByRole("checkbox", { name: "EN", exact: true }).uncheck();
  await install.getByRole("button", { name: "Install legal templates", exact: true }).click();
  await expect(install.getByRole("status")).toContainText("Created: 6; skipped: 0");
});

test("publishes Terms and a CMS page with heading, rich text and FAQ", async () => {
  await nav("Pages").click();
  const table = page.getByRole("table", { name: "Pages", exact: true });
  const terms = table.getByRole("row").filter({ has: page.getByText("Terms", { exact: true }) });
  await expect(terms).toBeVisible();
  await expectAccessible(page, "content-pages");
  await terms.getByRole("link").click();
  await expect(
    page.getByText("Template, not legal advice: have it reviewed by a lawyer before publishing.", {
      exact: true,
    }),
  ).toBeVisible();
  await expectAccessible(page, "content-legal-editor");
  await publish();
  await expect(page.getByRole("heading", { name: "Pages", exact: true })).toBeVisible();
  await page.getByRole("link", { name: "New page", exact: true }).first().click();
  await page.getByLabel("Title", { exact: true }).fill("Doprava");
  await expect(page.getByLabel("URL slug", { exact: true })).toHaveValue("doprava");
  await addBlock("Heading");
  await page.getByLabel("Heading text", { exact: true }).fill("Doručení zboží");
  await addBlock("Rich text");
  await page
    .getByRole("textbox", { name: "Body text", exact: true })
    .fill("Objednávky doručujeme po celé České republice.");
  await addBlock("FAQ");
  await page.getByRole("button", { name: "Add question", exact: true }).click();
  await page.getByLabel("Question", { exact: true }).fill("Kdy dorazí balík?");
  await page
    .getByRole("textbox", { name: "Answer", exact: true })
    .fill("Obvykle do dvou pracovních dnů.");
  await expectAccessible(page, "content-page-editor");
  await publish();
  await expect(
    page.getByRole("table", { name: "Pages" }).getByRole("row").filter({ hasText: "Doprava" }),
  ).toContainText("Published");
});

test("publishes a blog post", async () => {
  await nav("Blog").click();
  await expect(page.getByRole("heading", { name: "Blog", exact: true })).toBeVisible();
  await expectAccessible(page, "content-blog");
  await page.getByRole("link", { name: "New blog post", exact: true }).first().click();
  await page.getByLabel("Title", { exact: true }).fill("Novinky v obchodě");
  await page.getByLabel("Excerpt", { exact: true }).fill("Představujeme náš nový obchod.");
  await addBlock("Rich text");
  await page
    .getByRole("textbox", { name: "Body text", exact: true })
    .fill("Vítejte v našem obchodě.");
  await expectAccessible(page, "content-blog-editor");
  await publish();
  await expect(
    page
      .getByRole("table", { name: "Blog" })
      .getByRole("row")
      .filter({ hasText: "Novinky v obchodě" }),
  ).toContainText("Published");
});

test("adds a URL to the main menu", async () => {
  await nav("Menus").click();
  await page.getByLabel("Menu handle", { exact: true }).selectOption("main");
  await page.getByRole("button", { name: "Add entry", exact: true }).click();
  await page.getByLabel("Link type", { exact: true }).selectOption("url");
  await page.getByLabel("Link URL", { exact: true }).fill("/doprava");
  await page.getByLabel("Label (cs)", { exact: true }).fill("Doprava");
  await expectAccessible(page, "content-menus");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await saved();
});

test("uploads 107 feed items, reviews the collision and applies the import", async () => {
  test.setTimeout(240_000);
  await nav("Imports").click();
  await page.getByLabel("Source", { exact: true }).selectOption("heureka");
  await page.getByLabel("Market", { exact: true }).selectOption({ label: "CZ" });
  await page.getByRole("radio", { name: "File upload", exact: true }).check();
  await expectAccessible(page, "content-imports");
  await page
    .getByLabel("XML file", { exact: true })
    .setInputFiles(join(root, "fixtures/feeds/heureka-demo.xml"));
  await page.getByRole("button", { name: "Start dry run", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Dry-run report", exact: true })).toBeVisible({
    timeout: 120_000,
  });
  const report = page.getByRole("region", { name: "Dry-run report", exact: true });
  await expect(
    report
      .locator("dl > div")
      .filter({ has: page.getByRole("term").filter({ hasText: /^Items$/ }) })
      .getByRole("definition"),
  ).toHaveText("107");
  await expect(report.getByRole("table", { name: "Collisions", exact: true })).toContainText(
    "redirect",
  );
  await expect(report.getByRole("table", { name: "Collisions", exact: true })).toContainText(
    "sala-vlna-bordo",
  );
  await expectAccessible(page, "content-import-detail");
  await page.getByRole("button", { name: "Apply import", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Products are created as drafts");
  await dialog.getByRole("button", { name: "Apply import", exact: true }).click();
  await expect(page.getByText("Applied", { exact: true })).toBeVisible({ timeout: 120_000 });
  await expect(page.getByRole("heading", { name: "Import results", exact: true })).toBeVisible();
});

test("lists export channels and saves synonyms", async () => {
  await nav("Export feeds").click();
  const table = page.getByRole("table", { name: "Export feeds", exact: true });
  for (const channel of ["Google Merchant", "Heureka", "Zboží"])
    await expect(table.getByRole("cell", { name: channel, exact: true })).toBeVisible();
  await expectAccessible(page, "content-feeds");
  await nav("Search synonyms").click();
  await page.getByLabel("Synonym groups", { exact: true }).fill("boty, obuv");
  await expectAccessible(page, "content-synonyms");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await saved();
  await page.reload();
  await expect(page.getByLabel("Synonym groups", { exact: true })).toHaveValue("boty, obuv");
});
