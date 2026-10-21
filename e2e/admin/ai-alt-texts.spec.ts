import { expect, type Locator, type Page, test } from "@playwright/test";
import { expectAccessible, pngFixture, run, screenshot, signInOwner, sql } from "./support.ts";

const NAME = "Tričko Slub";
const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
const PRODUCT = `(SELECT product_id FROM product_translations
  WHERE tenant_id = ${DEMO} AND locale = 'cs' AND name = '${NAME}')`;
const UPLOADS = `(SELECT id FROM assets WHERE tenant_id = ${DEMO} AND filename LIKE 'shop-37-%')`;
const AI_LABEL = "AI-generated";
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

function altText(image: Locator, locale: string): Locator {
  return image.getByLabel(`Alt text (${locale})`, { exact: true });
}

test.beforeAll(() => {
  cleanUp();
});

test.afterAll(() => {
  cleanUp();
});

test("switch on → upload with a cs alt text → AI sk alt text survives a save from the open form, labelled until edited", async ({
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
  const unsold = altText(image, "en");
  const storedSk = () =>
    sql(`SELECT coalesce(alt_i18n ->> 'sk', '') FROM product_media
      WHERE tenant_id = ${DEMO} AND asset_id IN ${UPLOADS}`);
  await expect.poll(storedSk, { timeout: 180_000, intervals: [2_000, 5_000] }).toBe(AI_ALT);
  await expect(sk).toHaveValue("");
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  expect(storedSk()).toBe(AI_ALT);
  await expect(sk).toHaveValue(AI_ALT);
  await expect(sk).toHaveAccessibleDescription(AI_LABEL);
  await admin.goto(productUrl);
  await expect(sk).toHaveValue(AI_ALT);
  await expect(sk).toHaveAccessibleDescription(AI_LABEL);
  await expect(cs).toHaveValue(MERCHANT_ALT);
  await expect(cs).not.toHaveAccessibleDescription(AI_LABEL);
  await expect(unsold).toHaveValue("");
  await expect(unsold).not.toHaveAccessibleDescription(AI_LABEL);
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
  await sk.scrollIntoViewIfNeeded();
  await screenshot(admin, "shop-37-media-editor-phone");
  await admin.setViewportSize({ width: 1280, height: 860 });

  await sk.fill(EDITED_ALT);
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(sk).not.toHaveAccessibleDescription(AI_LABEL);
  await admin.goto(productUrl);
  await expect(sk).toHaveValue(EDITED_ALT);
  await expect(sk).not.toHaveAccessibleDescription(AI_LABEL);

  await sk.fill("");
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  expect(storedSk()).toBe("");

  await image.getByRole("button", { name: `Remove: Image ${before + 1}` }).click();
  await admin.getByRole("button", { name: "Save product" }).click();
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(image).toHaveCount(0);

  await admin.context().close();
});

test("a save still checking for AI alt texts writes the product it started on", async ({
  browser,
}) => {
  const productId = sql(`SELECT ${PRODUCT}`);
  const otherId = sql(`SELECT product_id FROM product_translations
    WHERE tenant_id = ${DEMO} AND locale = 'cs' AND name = 'Batoh Mini'`);
  const admin = await signInOwner(browser);
  const name = admin
    .getByRole("tabpanel", { name: "Czech" })
    .getByRole("textbox", { name: "Name", exact: true });
  await admin.goto(`/products/${productId}`);
  await expect(name).toHaveValue(NAME);

  await admin.route(`**/admin/v1/products/${productId}`, async (r) => {
    if (r.request().method() === "GET") await new Promise((done) => setTimeout(done, 3_000));
    await r.continue();
  });
  const puts: string[] = [];
  await admin.route("**/admin/v1/products/*", async (r) => {
    if (r.request().method() !== "PUT") return r.fallback();
    puts.push(new URL(r.request().url()).pathname);
    await r.abort();
  });

  await admin.getByRole("button", { name: "Save product" }).click();
  await admin.evaluate((url) => {
    history.pushState(null, "", url);
    dispatchEvent(new PopStateEvent("popstate"));
  }, `/products/${otherId}`);
  await expect(name).toHaveValue("Batoh Mini");
  await expect.poll(() => puts, { timeout: 10_000 }).toEqual([`/admin/v1/products/${productId}`]);

  await admin.context().close();
});
