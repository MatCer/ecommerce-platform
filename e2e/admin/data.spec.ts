/**
 * WP13b acceptance (admin): CSV imports of customers, historical orders and newsletter
 * subscribers (column mapping, dry-run report, apply), the order archive, the full data
 * export with its download, and a GDPR access + erasure request, on the seeded demo shop
 * (`make seed`). Every address is unique to the run and erased again at the end.
 */
import { readFileSync } from "node:fs";
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, run, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });

async function signIn(page: Page, email: string): Promise<void> {
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(email, since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
}

const nav = (page: Page, name: string) =>
  page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name, exact: true });

/** Uploads `csv` as `kind`, checks the dry run and applies it; returns the run page. */
async function importCsv(
  page: Page,
  kind: string,
  csv: string,
  mapping: Record<string, string> = {},
): Promise<void> {
  await nav(page, "CSV import").click();
  await expect(page.getByRole("heading", { name: "CSV import", level: 1 })).toBeVisible();
  await page.getByLabel("What to import").selectOption({ label: kind });
  await page.getByLabel("CSV file (UTF-8, at most 20 MB)").setInputFiles({
    name: `${run}.csv`,
    mimeType: "text/csv",
    buffer: Buffer.from(csv),
  });
  for (const [field, column] of Object.entries(mapping)) {
    await page.getByLabel(field, { exact: false }).first().selectOption(column);
  }
  await page.getByRole("button", { name: "Upload and check" }).click();
  await expect(page.getByRole("status").getByText("Checked", { exact: true })).toBeVisible({
    timeout: 30_000,
  });
}

async function apply(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByRole("status").getByText("Imported", { exact: true })).toBeVisible({
    timeout: 30_000,
  });
}

test("merchant imports, exports and answers a GDPR request", async ({ page }) => {
  const anna = `anna-${run}@example.com`;
  const news = `news-${run}@example.com`;
  const cold = `cold-${run}@example.com`;
  await signIn(page, "owner@lnen.example");

  // Customers: a semicolon file with Czech headers, one bad row reported by line.
  await importCsv(
    page,
    "Customers",
    `E-mail;Jméno;street;city;postal_code;country\n${anna};Anna Nová;Dlouhá 1;Praha;11000;CZ\nnot-an-email;X;;;;\n`,
    { "email *": "E-mail", name: "Jméno" },
  );
  const errors = page.getByRole("table", { name: "Row errors" });
  await expect(errors.getByRole("row", { name: /3 .*not-an-email/ })).toBeVisible();
  await expectAccessible(page, "import report");
  await apply(page);

  // The imported account is listed and found by name under Customers.
  await nav(page, "Customers").click();
  await expect(page.getByRole("heading", { name: "Customers", level: 1 })).toBeVisible();
  await page.getByLabel("Search by email or name").fill("Anna Nová");
  await expect(page.getByRole("row", { name: new RegExp(anna) })).toBeVisible();
  await page.getByLabel("Search by email or name").fill(anna);
  await expect(page.getByRole("status").getByText("Customers: 1")).toBeVisible();
  await expectAccessible(page, "customers");

  // Historical orders land in the archive, nowhere else.
  await importCsv(
    page,
    "Historical orders",
    `order_number,placed_at,email,currency,total,status,item_name,quantity,unit_price\nOLD-${run},2023-02-01,${anna},CZK,249.00,Vyřízeno,Tričko,1,249.00\n`,
  );
  await apply(page);
  await nav(page, "Order archive").click();
  await page.getByLabel("Order number or email").fill(`OLD-${run}`);
  await expect(page.getByRole("row", { name: new RegExp(`OLD-${run}.*Vyřízeno`) })).toBeVisible();
  await expectAccessible(page, "order archive");

  // Subscribers: only the row with consent evidence becomes marketable.
  await importCsv(
    page,
    "Newsletter subscribers",
    `email,consent_at,consent_source\n${news},2023-05-01T08:00:00Z,old shop checkout\n${cold},,\n`,
  );
  await apply(page);
  const outcome = page.getByRole("status").filter({ hasText: "Progress" });
  await expect(outcome.getByText("Subscribed (with consent)")).toBeVisible();
  await expect(outcome.getByText("Pending, no marketing")).toBeVisible();

  // Full export: prepared in the background, then downloaded through a short-lived link.
  await nav(page, "Export and privacy").click();
  await expect(page.getByRole("heading", { name: "Export and privacy", level: 1 })).toBeVisible();
  await expectAccessible(page, "export and privacy");
  await page.getByRole("button", { name: "Prepare export" }).click();
  const exports = page.getByRole("table", { name: "Export all shop data" });
  await expect(exports.getByText("Ready").first()).toBeVisible({ timeout: 60_000 });
  const zip = page.waitForEvent("download");
  await exports.getByRole("button", { name: "Download" }).first().click();
  expect((await zip).suggestedFilename()).toMatch(/\.zip$/);

  // GDPR: the person's data as JSON, then erasure with a typed confirmation.
  await nav(page, "Export and privacy").click();
  await page.getByLabel("Person's email").fill(anna);
  const json = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download their data" }).click();
  const doc = JSON.parse(readFileSync(await (await json).path(), "utf8")) as {
    customer: { name: string };
    archived_orders: { number: string }[];
  };
  expect(doc.customer.name).toBe("Anna Nová");
  expect(doc.archived_orders[0]?.number).toBe(`OLD-${run}`);

  await page.getByRole("button", { name: "Erase their data" }).click();
  const dialog = page.getByRole("dialog", { name: "Erase personal data?" });
  const erase = dialog.getByRole("button", { name: "Erase their data" });
  await expect(erase).toBeDisabled();
  await dialog.getByLabel("Email again").fill(anna);
  await expectAccessible(page, "erasure dialog");
  await erase.click();
  await expect(page.getByText(/^Erased: 1 orders anonymized, 0 invoices kept\.$/)).toBeVisible();

  // Nothing of Anna is left in the archive; the subscribers of this run are erased too.
  await nav(page, "Order archive").click();
  await page.getByLabel("Order number or email").fill(`OLD-${run}`);
  await expect(page.getByRole("row", { name: /erased@erased\.invalid/ })).toBeVisible();
  for (const email of [news, cold]) {
    await nav(page, "Export and privacy").click();
    await page.getByLabel("Person's email").fill(email);
    await page.getByRole("button", { name: "Erase their data" }).click();
    await page.getByRole("dialog").getByLabel("Email again").fill(email);
    await page.getByRole("dialog").getByRole("button", { name: "Erase their data" }).click();
    await expect(page.getByText(/^Erased:/).first()).toBeVisible();
  }
});
