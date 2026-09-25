/**
 * Reviews (WP16) against the seeded demo shop: a review link of a delivered order line opens
 * the form on the checkout origin, the review is submitted once (the link is single use), the
 * merchant publishes it with a reply, and the product page shows it with the verified-purchase
 * mark, the Omnibus disclosure and the rating in the Product JSON-LD.
 *
 * Review links are issued by WP19's invites (`commerce::reviews::issue_tokens`); until then
 * the test writes a delivered order line and its token straight into the database.
 */
import { createHash, randomBytes } from "node:crypto";
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "../admin/support";
import { CZ, checkoutOf, newPage } from "./support";

const SLUG = "tricko-henley";

/** A delivered order with one line of the product and a review token for it. */
function reviewLink(): string {
  const token = randomBytes(32).toString("hex");
  const hash = createHash("sha256").update(token).digest("hex");
  // Far above the shop's own order numbers (allocated from `order_numbers`).
  const number = 9_000_000_000 + Math.floor(Math.random() * 900_000_000);
  sql(`
    WITH t AS (SELECT id FROM platform.tenants WHERE slug = 'demo'),
    m AS (SELECT id FROM markets WHERE tenant_id = (SELECT id FROM t) AND is_default),
    p AS (SELECT p.id AS product_id, v.id AS variant_id, v.sku FROM products p
          JOIN product_translations pt ON pt.product_id = p.id
          JOIN variants v ON v.product_id = p.id
          WHERE p.tenant_id = (SELECT id FROM t) AND pt.slug = '${SLUG}' LIMIT 1),
    c AS (INSERT INTO carts (tenant_id, market_id, locale, currency, status)
          SELECT (SELECT id FROM t), (SELECT id FROM m), 'cs', 'CZK', 'converted' RETURNING id),
    o AS (INSERT INTO orders (tenant_id, number, market_id, cart_id, email, locale, currency,
                              status, payment_status, fulfillment_status, ship_to_country,
                              vat_payer, subtotal_minor, discount_minor, shipping_minor,
                              payment_fee_minor, tax_minor, rounding_minor, total_minor,
                              vat_recap, shipping_method_snapshot, payment_method)
          SELECT (SELECT id FROM t), ${number}, (SELECT id FROM m), (SELECT id FROM c),
                 'wp16-${run}@example.test', 'cs', 'CZK', 'delivered', 'paid', 'delivered', 'CZ',
                 true, 49900, 0, 0, 0, 0, 0, 49900, '[]', '{}', 'cod'
          RETURNING id, tenant_id),
    l AS (INSERT INTO order_lines (tenant_id, order_id, position, variant_id, product_id, sku,
                                   name, quantity, unit_gross_minor, base_minor, discount_minor,
                                   total_minor, tax_rate, tax_minor, net_minor)
          SELECT o.tenant_id, o.id, 1, p.variant_id, p.product_id, p.sku, 'Tričko Henley', 1,
                 49900, 49900, 0, 49900, '21', 0, 49900 FROM o, p
          RETURNING id, order_id, tenant_id, product_id)
    INSERT INTO review_tokens (tenant_id, order_line_id, order_id, product_id, token_hash, expires_at)
    SELECT tenant_id, id, order_id, product_id, decode('${hash}', 'hex'), now() + interval '90 days'
    FROM l`);
  return `${checkoutOf(CZ)}/review?token=${token}`;
}

let admin: Page;

test.beforeAll(async ({ browser }) => {
  admin = await signInOwner(browser);
});

test.afterAll(async () => {
  await admin.context().close();
});

test("review link → form → moderation → product page with rating JSON-LD", async ({ browser }) => {
  const name = `Jana ${run}`;
  const body = `Pohodlné tričko, sedí přesně. <b>${run}</b>`;
  const link = reviewLink();
  const page = await newPage(browser);

  await page.goto(link);
  await expect(page.getByRole("heading", { name: "Napsat recenzi" })).toBeVisible();
  await expect(page.getByText("Tričko Henley")).toBeVisible();
  await expectAccessible(page, "review form");
  await page.getByRole("radio", { name: "5 z 5 hvězdiček" }).check();
  await page.getByLabel("Jméno zobrazené u recenze").fill(name);
  await page.getByLabel("Nadpis (nepovinné)").fill("Doporučuji");
  await page.getByLabel("Vaše recenze").fill(body);
  await page.getByRole("button", { name: "Odeslat recenzi" }).click();
  await expect(page.getByRole("status")).toContainText("Děkujeme!");
  // Single use: the same link now only explains itself.
  await page.goto(link);
  await expect(page.getByRole("alert")).toContainText("neplatný");
  await expect(page.getByRole("button", { name: "Odeslat recenzi" })).toHaveCount(0);

  // The merchant finds it in the queue, replies and publishes it.
  await admin.goto("/marketing/reviews");
  const card = admin.getByRole("listitem").filter({ hasText: name });
  await expect(card).toBeVisible();
  await expect(card.getByText("Verified purchase")).toBeVisible();
  await card.getByLabel("Public reply").fill(`Děkujeme, ${name}!`);
  await card.getByRole("button", { name: "Save reply" }).click();
  await expect(admin.getByText("Reply saved")).toBeVisible();
  await card.getByRole("button", { name: /^Publish/ }).click();
  await expect(admin.getByText("Review published")).toBeVisible();

  // The shop shows it (the publish purges the cached page) with the disclosure and JSON-LD.
  const shop = await newPage(browser);
  await expect
    .poll(
      async () => {
        await shop.goto(`${CZ}/p/${SLUG}`);
        return shop.getByText(body).count();
      },
      { timeout: 30_000 },
    )
    .toBe(1);
  const reviews = shop.getByRole("region", { name: "Recenze" });
  const mine = reviews.getByRole("listitem").filter({ hasText: body });
  await expect(mine.getByText("Ověřený nákup")).toBeVisible();
  await expect(mine.getByText(`Děkujeme, ${name}!`)).toBeVisible();
  await expect(reviews.getByText(/Recenzi může napsat jen zákazník/)).toBeVisible();
  await expect(reviews.getByRole("link", { name: "Jak ověřujeme recenze" })).toHaveAttribute(
    "href",
    /\/pages\//,
  );
  await expectAccessible(shop, "product reviews");
  const ld = await shop
    .locator('script[type="application/ld+json"]')
    .evaluateAll((els) => els.map((e) => JSON.parse(e.textContent ?? "null")));
  const product = ld.find((d) => d?.["@type"] === "Product");
  expect(product.aggregateRating.reviewCount).toBeGreaterThan(0);
  expect(product.review.some((r: { reviewBody: string }) => r.reviewBody === body)).toBe(true);
  expect(
    await shop.locator('script[type="application/ld+json"]').first().innerHTML(),
  ).not.toContain("<b>");

  // Hidden again: gone from the shop.
  await admin.getByLabel("Status").selectOption("published");
  await card.getByRole("button", { name: /^Hide/ }).click();
  await expect(admin.getByText("Review hidden")).toBeVisible();
  await expect
    .poll(
      async () => {
        await shop.goto(`${CZ}/p/${SLUG}`);
        return shop.getByText(body).count();
      },
      { timeout: 30_000 },
    )
    .toBe(0);
  await shop.context().close();
  await page.context().close();
});
