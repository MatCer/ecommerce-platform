/**
 * AI review summaries (SHOP-36) on the seeded demo shop with the fake provider: the owner
 * switches them on in the admin, the worker summarizes a product with five verified reviews, and
 * its CZ and SK pages show the summary in the market language with the AI label. Switched off,
 * the summary is gone from the shop.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible as expectAdminAccessible, run, signInOwner, sql } from "../admin/support";
import { testContext } from "../rate-client";
import { CZ, decideConsent, expectAccessible, SK, screenshot } from "./support";

const SLUG = "tricko-raglan";
const NAME = "Tričko Raglan";
const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
const PRODUCT = `(SELECT product_id FROM product_translations
  WHERE tenant_id = ${DEMO} AND locale = 'cs' AND slug = '${SLUG}')`;
const AUTHOR = "SHOP-36 e2e";

function cleanUp(): void {
  sql(`UPDATE ai_settings SET review_summaries = false WHERE tenant_id = ${DEMO}`);
  sql(`DELETE FROM reviews WHERE product_id = ${PRODUCT} AND customer_name LIKE '${AUTHOR} %'`);
  sql(`DELETE FROM review_summaries WHERE product_id = ${PRODUCT}`);
}

/** The rating the job summarizes, as the fake provider prints it (one decimal, decimal comma). */
function published(): { average: string; count: number } {
  const [count, total] = sql(`SELECT count(*) || '|' || sum(rating) FROM reviews
    WHERE product_id = ${PRODUCT} AND status = 'published' AND verified`)
    .split("|")
    .map(Number);
  if (!count || !total) throw new Error("no published reviews");
  const average = Math.round((total / count) * 10) / 10;
  return { average: average.toFixed(1).replace(".", ","), count };
}

async function setSwitch(admin: Page, on: boolean): Promise<void> {
  await admin.goto("/settings/ai");
  await expect(admin.getByRole("heading", { name: "AI", exact: true })).toBeVisible();
  const card = admin.getByRole("region", { name: "AI on the shop" });
  const box = card.getByRole("checkbox", { name: "Review summaries" });
  await expect(box).toBeChecked({ checked: !on });
  // Kobalte checkboxes: the visually hidden input is toggled through its label.
  await card.locator("label").filter({ hasText: "Review summaries" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(box).toBeChecked({ checked: on });
}

async function pollSummary(page: Page, base: string, title: string, shown: boolean) {
  await expect
    .poll(
      async () => {
        await page.goto(`${base}/p/${SLUG}`);
        return page.getByRole("heading", { level: 3, name: title }).count();
      },
      { timeout: 90_000, intervals: [1_000, 2_000, 5_000] },
    )
    .toBe(shown ? 1 : 0);
}

test.beforeAll(() => {
  cleanUp();
  sql(`INSERT INTO reviews (tenant_id, product_id, customer_name, rating, body, status, verified,
                            locale, published_at)
    SELECT ${DEMO}, ${PRODUCT}, '${AUTHOR} ${run} ' || n, CASE WHEN n <= 3 THEN 5 ELSE 4 END,
           'Pohodlné tričko, recenze ' || n || ' pro shrnutí.', 'published', true, 'cs', now()
    FROM generate_series(1, 5) n`);
});

test.afterAll(() => {
  cleanUp();
});

test("admin switch → worker summary → labelled summary on the CZ and SK product pages", async ({
  browser,
}) => {
  test.setTimeout(300_000);
  const { average, count } = published();
  const admin = await signInOwner(browser);
  await setSwitch(admin, true);
  await expectAdminAccessible(admin, "ai-settings-review-summaries");

  const shop = await testContext(browser, { viewport: { width: 390, height: 844 } });
  await decideConsent(shop, CZ);
  await decideConsent(shop, SK);
  const page = await shop.newPage();

  await pollSummary(page, CZ, "Co říkají zákazníci", true);
  const cs = page.getByRole("region", { name: "Recenze" });
  const csText = cs.getByText(
    `Ukázkové shrnutí od demo AI: zákazníci hodnotí „${NAME}“ průměrně ${average} z 5.`,
    { exact: true },
  );
  await expect(csText).toBeVisible();
  await expect(csText).toHaveAttribute("lang", "cs");
  await expect(
    cs.getByText(`Shrnutí vytvořila AI na základě ${count} ověřených recenzí`, { exact: true }),
  ).toBeVisible();
  await expectAccessible(page, "review summary (phone)");
  await screenshot(page, "review-summary-phone");

  await page.setViewportSize({ width: 1280, height: 900 });
  await expect(csText).toBeVisible();
  await expectAccessible(page, "review summary (desktop)");
  await screenshot(page, "review-summary-desktop");

  await pollSummary(page, SK, "Čo hovoria zákazníci", true);
  const sk = page.getByRole("region", { name: "Recenzie" });
  const skText = sk.getByText(
    `Ukážkové zhrnutie od demo AI: zákazníci hodnotia „${NAME}“ priemerne ${average} z 5.`,
    { exact: true },
  );
  await expect(skText).toBeVisible();
  await expect(skText).toHaveAttribute("lang", "sk");
  await expect(
    sk.getByText(`Zhrnutie vytvorila AI na základe ${count} overených recenzií`, { exact: true }),
  ).toBeVisible();

  await setSwitch(admin, false);
  await pollSummary(page, CZ, "Co říkají zákazníci", false);
  await expect(page.getByRole("region", { name: "Recenze" })).toBeVisible();
  await expect(page.getByText(/Ukázkové shrnutí od demo AI/)).toHaveCount(0);

  await shop.close();
  await admin.context().close();
});
