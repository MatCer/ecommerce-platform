/**
 * Reviews (WP16) against the seeded demo shop: a review link of a delivered order line opens
 * the form on the checkout origin, the review is submitted once (the link is single use), the
 * merchant publishes it with a reply, and the product page shows it with the verified-purchase
 * mark, the Omnibus disclosure and the rating in the Product JSON-LD.
 *
 * WP19's invite flow issues the review link after the dev clock advances seven days.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "../admin/support";
import { CZ, mail, newPage } from "./support";

const SLUG = "tricko-henley";

/** A delivered order with one line. WP19 sends the review link through Mailpit. */
async function reviewLink(): Promise<string> {
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
          RETURNING order_id, tenant_id),
    shipment AS (INSERT INTO shipments (tenant_id, order_id, carrier, status, delivered_at, created_by)
          SELECT tenant_id, order_id, 'personal_pickup', 'delivered', now(), 'e2e' FROM l
          RETURNING tenant_id),
    consent AS (INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose,
                                            granted, text_version, source)
          SELECT tenant_id, 'email', 'wp16-${run}@example.test', 'review_invites', true,
                 '2026-09-25', 'checkout' FROM shipment RETURNING tenant_id)
    SELECT tenant_id FROM consent`);

  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString());
  expect(auth.ok()).toBeTruthy();
  const { token } = (await auth.json()) as { token: string };
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  api.pathname = "/admin/v1/flows/test-clock/advance";
  const advanced = await admin.request.post(api.toString(), {
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
    data: { hours: 8 * 24 },
  });
  expect(advanced.ok()).toBeTruthy();
  const message = await mail(`wp16-${run}@example.test`, "Ohodnoťte svůj nákup");
  const link = message.Text.match(/https?:\/\/[^\s]+\/review\?token=[0-9a-f]{64}/)?.[0];
  expect(link).toBeDefined();
  if (!link) throw new Error("review invite did not contain a link");
  return link;
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
  const link = await reviewLink();
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
