/**
 * SHOP-37: AI alt texts on the seeded demo shop with the fake provider. The owner switches them
 * on, uploads a product image with a cs alt text, and the worker fills the other market language
 * (sk) through the catalog save; the editor labels that field until the owner edits it.
 */

import { expect, type Locator, type Page, test } from "@playwright/test";
import { expectAccessible, pngFixture, run, screenshot, signInOwner, sql } from "./support.ts";

const NAME = "Tričko Slub";
const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
const PRODUCT = `(SELECT product_id FROM product_translations
  WHERE tenant_id = ${DEMO} AND locale = 'cs' AND name = '${NAME}')`;
const UPLOADS = `(SELECT id FROM assets WHERE tenant_id = ${DEMO} AND filename LIKE 'shop-37-%')`;
const CONTENT_LOCALES = ["cs", "sk", "en"];
const MERCHANT_ALT = "Šedé tričko Slub na ramínku";
const AI_ALT = `[sk] ${NAME}, from cs: ${MERCHANT_ALT}`;
const EDITED_ALT = "Sivé tričko Slub na vešiaku";

function cleanUp(): void {
  sql(`UPDATE ai_settings SET alt_texts_since = NULL WHERE tenant_id = ${DEMO}`);
  sql(`DELETE FROM ai_marks WHERE tenant_id = ${DEMO} AND entity_type = 'product'
    AND field IN (SELECT 'alt_' || replace(id::text, '-', '') FROM ${UPLOADS})`);
  sql(`DELETE FROM product_media WHERE tenant_id = ${DEMO} AND asset_id IN ${UPLOADS}`);
  sql(`DELETE FROM assets WHERE id IN ${UPLOADS}`);
}

async function setSwitch(admin: Page, on: boolean): Promise<Locator> {
  await admin.goto("/settings/ai");
  await expect(admin.getByRole("heading", { name: "AI", exact: true })).toBeVisible();
  const card = admin.getByRole("region", { name: "AI on the shop" });
  const box = card.getByRole("checkbox", { name: "Alt texts for new product images" });
  await expect(box).toBeChecked({ checked: !on });
  // Kobalte checkboxes: the visually hidden input is toggled through its label.
  await card.locator("label").filter({ hasText: "Alt texts for new product images" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(box).toBeChecked({ checked: on });
  return card;
}

/** One locale's alt text field and the AI label shown next to it (not next to its neighbours). */
function altText(image: Locator, locale: string): { field: Locator; badge: Locator } {
  const page = image.page();
  const field = page.getByLabel(`Alt text (${locale})`);
  const others = CONTENT_LOCALES.filter((l) => l !== locale).join("|");
  const box = image
    .locator("div")
    .filter({ has: field })
    .filter({ hasNot: page.getByLabel(new RegExp(`^Alt text \\((${others})\\)$`)) })
    .first();
  return { field: box.getByLabel(`Alt text (${locale})`), badge: box.getByText("AI-generated") };
}

test.beforeAll(() => {
  cleanUp();
});

test.afterAll(() => {
  cleanUp();
});

test("switch on → upload with a cs alt text → AI sk alt text labelled until edited", async ({
  browser,
}) => {
  test.setTimeout(300_000);
  const productId = sql(`SELECT ${PRODUCT}`);
  expect(productId).toMatch(/^[0-9a-f-]{36}$/);
  const productUrl = `/products/${productId}`;
  const before = Number(
    sql(`SELECT count(*) FROM product_media WHERE product_id = '${productId}'`),
  );
  const since = sql("SELECT now()");

  const admin = await signInOwner(browser);
  const card = await setSwitch(admin, true);
  await expect(card.getByRole("button", { name: "Generate missing alt texts" })).toBeEnabled();
  await expectAccessible(admin, "ai-settings-alt-texts");
  await card.scrollIntoViewIfNeeded();
  await screenshot(admin, "shop-37-ai-settings-desktop");
  await admin.setViewportSize({ width: 390, height: 844 });
  await card.scrollIntoViewIfNeeded();
  await screenshot(admin, "shop-37-ai-settings-phone");
  await admin.setViewportSize({ width: 1280, height: 860 });

  await admin.goto(productUrl);
  await expect(admin.getByRole("heading", { name: "Edit product" })).toBeVisible();
  await admin.locator("#media-upload").setInputFiles({
    name: `shop-37-${run}.png`,
    mimeType: "image/png",
    buffer: pngFixture(),
  });
  const image = admin.getByRole("listitem", { name: `Image ${before + 1}`, exact: true });
  await expect(image.getByText("Ready")).toBeVisible({ timeout: 30_000 });
  await image.getByLabel("Alt text (cs)").fill(MERCHANT_ALT);
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();

  const cs = altText(image, "cs");
  const sk = altText(image, "sk");
  const en = altText(image, "en");
  await expect
    .poll(
      async () => {
        await admin.goto(productUrl);
        return sk.field.inputValue();
      },
      { timeout: 180_000, intervals: [2_000, 5_000] },
    )
    .toBe(AI_ALT);
  await expect(sk.badge).toBeVisible();
  await expect(cs.field).toHaveValue(MERCHANT_ALT);
  await expect(cs.badge).toHaveCount(0);
  // en is a content language but no market sells in it.
  await expect(en.field).toHaveValue("");
  await expect(en.badge).toHaveCount(0);
  expect(
    Number(
      sql(`SELECT count(*) FROM audit_log WHERE tenant_id = ${DEMO} AND actor = 'system'
        AND action = 'product.updated' AND entity_id = '${productId}' AND at >= '${since}'`),
    ),
  ).toBeGreaterThan(0);

  await expectAccessible(admin, "product-editor-ai-alt-text");
  await image.scrollIntoViewIfNeeded();
  await screenshot(admin, "shop-37-media-editor-desktop");
  await admin.setViewportSize({ width: 390, height: 844 });
  await sk.badge.scrollIntoViewIfNeeded();
  await screenshot(admin, "shop-37-media-editor-phone");
  await admin.setViewportSize({ width: 1280, height: 860 });

  await sk.field.fill(EDITED_ALT);
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(sk.badge).toHaveCount(0);
  await admin.goto(productUrl);
  await expect(sk.field).toHaveValue(EDITED_ALT);
  await expect(sk.badge).toHaveCount(0);

  await image.getByRole("button", { name: `Remove: Image ${before + 1}` }).click();
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(image).toHaveCount(0);

  await admin.context().close();
});
