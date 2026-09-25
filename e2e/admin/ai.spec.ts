/**
 * WP22: AI helpers against the seeded demo shop (`make seed`) with the fake provider (no
 * ANTHROPIC_API_KEY): description proposal accepted per field with an AI label, translation
 * cs → sk/en with the glossary, bulk edit "raise T-shirt prices by 5 % in SK" (plan, preview,
 * confirm, prices + price history + audit), and the used-up allowance.
 *
 * Note: every run raises the demo shop's SK T-shirt prices by 5 %.
 */

import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { testContext } from "../rate-client";
import { dockerExec, expectAccessible, magicLink, sql as querySql, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });
const owner = "owner@lnen.example";
const productName = "Tričko s dlouhým rukávem";
let context: BrowserContext;
let page: Page;

/** Superadmin override of the demo shop's AI allowance (`undefined` = plan default). */
function setQuota(tokens?: number): void {
  const extra = tokens === undefined ? [] : ["--tokens", String(tokens)];
  dockerExec("api", ["/usr/local/bin/api", "admin", "set-ai-quota", "--tenant", "demo", ...extra]);
}

/** One query as the database superuser (checks only; RLS does not apply). */
function sql(query: string): string[] {
  return querySql(query)
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}

const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
const TSHIRT_PRICES = `
  SELECT vp.variant_id || '|' || vp.amount_minor FROM variant_prices vp
  JOIN price_lists pl ON pl.id = vp.price_list_id AND pl.code = 'eur'
  JOIN variants v ON v.id = vp.variant_id
  WHERE vp.tenant_id = ${DEMO} AND v.product_id IN (
    SELECT pc.product_id FROM product_categories pc
    JOIN category_translations ct ON ct.category_id = pc.category_id
    WHERE ct.locale = 'cs' AND ct.slug = 'trika')
  ORDER BY 1`;

function prices(): Map<string, number> {
  return new Map(
    sql(TSHIRT_PRICES).map((l) => {
      const [id, amount] = l.split("|");
      return [id ?? "", Number(amount)];
    }),
  );
}

const panel = () => page.getByRole("region", { name: "AI assistant" });

async function openProduct(): Promise<void> {
  await page.goto("/products");
  await page.getByLabel("Search by name or SKU").fill(productName);
  // The list shows the English name once a previous run translated the product.
  await page
    .getByRole("link", { name: new RegExp(`^${productName}( \\[en\\])?$`) })
    .first()
    .click();
  await expect(panel()).toBeVisible();
}

test.beforeAll(async ({ browser }) => {
  setQuota();
  context = await testContext(browser);
  page = await context.newPage();
  await useEnglish(page);
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await expect(page.getByRole("status")).toContainText(owner);
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
});

test.afterAll(async () => {
  setQuota();
  await context?.close();
});

test("generates a description, previews it and accepts it with an AI label", async () => {
  await openProduct();
  const ai = panel();
  await expect(ai.getByText("Demo AI")).toBeVisible();
  await ai.getByLabel("Task").selectOption({ label: "Write the description" });
  await ai.getByLabel("Language", { exact: true }).selectOption("cs");
  await ai.getByLabel("Tone").selectOption("premium");
  await ai.getByLabel("Length").selectOption("short");
  // Generating must not submit the surrounding product form.
  const saves: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "PUT" && r.url().includes("/admin/v1/products/")) saves.push(r.url());
  });
  await ai.getByRole("button", { name: "Generate proposal" }).click();
  const changes = ai.getByRole("list", { name: "Proposed changes" });
  await expect(changes).toBeVisible({ timeout: 30_000 });
  await expect(
    changes.getByText(/ukázkový popis od demo AI \(premium, short\)/).last(),
  ).toBeVisible();
  expect(saves).toEqual([]);
  await expectAccessible(page, "ai-proposal");
  // Accept the description only.
  // Kobalte checkboxes: the visually hidden input is toggled through its label.
  await ai.locator("label").filter({ hasText: "Short description (CS)" }).click();
  await expect(ai.getByRole("checkbox", { name: "Short description (CS)" })).not.toBeChecked();
  await ai.getByRole("button", { name: "Accept selected" }).click();
  await expect(page.getByText("AI proposal saved")).toBeVisible();
  await expect(ai.getByText("AI-generated: Description (CS)")).toBeVisible();
  await expect(ai.getByText("AI-generated: Short description (CS)")).toBeHidden();
  const audit = sql(
    `SELECT count(*) FROM audit_log WHERE tenant_id = ${DEMO} AND action = 'ai.proposal.accepted'`,
  );
  expect(Number(audit[0])).toBeGreaterThan(0);
});

test("translates the product cs → sk/en and keeps glossary terms", async () => {
  await page.goto("/settings/ai");
  await expect(page.getByRole("heading", { name: "AI", exact: true })).toBeVisible();
  await expect(page.getByText(/of 2,000,000 tokens/)).toBeVisible();
  const glossary = page.getByRole("region", { name: "Glossary" });
  const existing = await glossary.getByLabel(/^Term \d+$/).count();
  let found = false;
  for (let i = 1; i <= existing; i++)
    if ((await glossary.getByLabel(`Term ${i}`, { exact: true }).inputValue()) === "Lnen & Co.")
      found = true;
  if (!found) {
    await glossary.getByRole("button", { name: "Add term" }).click();
    await glossary.getByLabel(`Term ${existing + 1}`, { exact: true }).fill("Lnen & Co.");
    await glossary.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Changes saved").last()).toBeVisible();
  }
  await expectAccessible(page, "ai-settings");

  await openProduct();
  const ai = panel();
  await ai.getByLabel("Task").selectOption({ label: "Translate" });
  await ai.getByLabel("Translate from").selectOption("cs");
  await expect(ai.getByRole("checkbox", { name: "Slovak" })).toBeChecked();
  await expect(ai.getByRole("checkbox", { name: "English" })).toBeChecked();
  await ai.getByRole("button", { name: "Generate proposal" }).click();
  const changes = ai.getByRole("list", { name: "Proposed changes" });
  await expect(changes).toBeVisible({ timeout: 30_000 });
  await expect(ai.getByRole("checkbox", { name: "Name (SK)" })).toBeVisible();
  await expect(ai.getByRole("checkbox", { name: "Name (EN)" })).toBeVisible();
  // The accepted description names the brand; the glossary keeps it in both languages.
  const enDescription = changes.getByRole("listitem").filter({ hasText: /^Description \(EN\)/ });
  await expect(enDescription).toContainText("Lnen & Co.");
  await expect(ai.getByText("Check before accepting")).toBeHidden();
  // Accept the English fields only (the demo's Slovak texts stay as seeded).
  for (const label of await ai
    .locator("label")
    .filter({ hasText: /\(SK\)$/ })
    .all())
    await label.click();
  for (const box of await ai.getByRole("checkbox", { name: /\(SK\)$/ }).all())
    await expect(box).not.toBeChecked();
  await ai.getByRole("button", { name: "Accept selected" }).click();
  await expect(page.getByText("AI proposal saved")).toBeVisible();
  await page.getByRole("tab", { name: /English/ }).click();
  await expect(
    page.getByRole("tabpanel", { name: "English" }).getByRole("textbox", { name: "Name" }),
  ).toHaveValue(`${productName} [en]`);
  await expect(ai.getByText("AI-generated: Name (EN)")).toBeVisible();
});

test("bulk edit: raise T-shirt prices by 5 % in SK after preview and confirmation", async () => {
  const before = prices();
  expect(before.size).toBeGreaterThan(0);
  await page.goto("/ai/bulk-edit");
  await expect(page.getByRole("heading", { name: "AI bulk edit" })).toBeVisible();
  await expectAccessible(page, "ai-bulk-edit");
  await page.getByLabel("What should change?").fill("Raise prices of T-shirts by 5 % in SK");
  await page.getByRole("button", { name: "Create plan" }).click();
  const matching = page.getByText(/^\d+ matching products$/);
  await expect(matching).toBeVisible({ timeout: 30_000 });
  const count = Number((await matching.textContent())?.split(" ")[0]);
  const products = Number(
    sql(`SELECT count(DISTINCT pc.product_id) FROM product_categories pc
         JOIN category_translations ct ON ct.category_id = pc.category_id
         WHERE pc.tenant_id = ${DEMO} AND ct.locale = 'cs' AND ct.slug = 'trika'`)[0],
  );
  expect(count).toBe(products);
  await expect(page.getByText("Change prices in SK by +5 %")).toBeVisible();
  const preview = page.getByRole("table", { name: "Preview (first products)" });
  await expect(preview.getByRole("cell", { name: /EUR/ }).first()).toBeVisible();
  await expectAccessible(page, "ai-bulk-preview");
  // The preview is a dry run.
  expect(prices()).toEqual(before);

  await page.getByRole("button", { name: `Apply to ${count} products` }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: `Apply to ${count} products` }).click();
  await expect(page.getByText(`Done: ${count} products changed, 0 skipped.`)).toBeVisible({
    timeout: 60_000,
  });

  const after = prices();
  for (const [variant, amount] of before)
    expect(after.get(variant), variant).toBe(Math.round((amount * 105) / 100));
  // Price history: the change started a new price interval (the old one is kept).
  const [variant] = [...after.keys()];
  const fresh = sql(`SELECT count(*) FROM price_intervals pi
    JOIN price_lists pl ON pl.id = pi.price_list_id AND pl.code = 'eur'
    WHERE pi.variant_id = '${variant}' AND pi.valid_from > now() - interval '5 minutes'`);
  expect(Number(fresh[0])).toBeGreaterThan(0);
  const audit = sql(`SELECT action FROM audit_log WHERE tenant_id = ${DEMO}
    AND at > now() - interval '5 minutes' AND action IN
      ('ai.bulk_plan.confirmed', 'variant_prices.upserted', 'ai.bulk_plan.applied')`);
  expect(audit).toContain("ai.bulk_plan.confirmed");
  expect(audit).toContain("ai.bulk_plan.applied");
  expect(audit.filter((a) => a === "variant_prices.upserted").length).toBeGreaterThanOrEqual(count);
});

test("a used-up allowance stops new AI requests", async () => {
  setQuota(1);
  await openProduct();
  const ai = panel();
  await ai.getByLabel("Task").selectOption({ label: "Write the SEO title and meta description" });
  await ai.getByRole("button", { name: "Generate proposal" }).click();
  await expect(ai.getByRole("alert")).toContainText("monthly AI allowance is used up");
  await page.goto("/ai/bulk-edit");
  await page.getByLabel("What should change?").fill("Raise prices of T-shirts by 5 % in SK");
  await page.getByRole("button", { name: "Create plan" }).click();
  await expect(page.getByRole("alert")).toContainText("monthly AI allowance is used up");
  await page.goto("/settings/ai");
  await expect(page.getByRole("alert")).toContainText("monthly AI allowance is used up");
  setQuota();
});
