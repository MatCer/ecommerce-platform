/**
 * Reviews (WP16) against the seeded demo shop: a review link of a delivered order line opens
 * the form on the checkout origin, the review is submitted once (the link is single use), the
 * merchant publishes it with a reply, and the product page shows it with the verified-purchase
 * mark, the Omnibus disclosure and the rating in the Product JSON-LD.
 *
 * WP19's invite flow issues the review link after the dev clock advances seven days.
 */
import { type Browser, expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "../admin/support";
import { rateHeaders } from "../rate-client";
import {
  acceptAndPlace,
  CZ,
  choosePickupPoint,
  fakePay,
  fillContactAndAddress,
  mail,
  newPage,
  order,
  toCheckout,
} from "./support";

const SLUG = "tricko-henley";

/** A paid, delivered order with checkout consent. WP19 sends its review link through Mailpit. */
async function reviewLink(browser: Browser): Promise<string> {
  const email = `wp16-${run}@example.test`;
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  sql(`INSERT INTO flow_test_clocks(tenant_id,offset_seconds) VALUES('${tenant}',0)
    ON CONFLICT(tenant_id) DO UPDATE SET offset_seconds=0`);
  const shopper = await newPage(browser);
  await toCheckout(shopper, CZ, SLUG);
  await fillContactAndAddress(shopper, email);
  await choosePickupPoint(shopper, /Z-BOX Praha 1/);
  await shopper.getByRole("radio", { name: /Testovací platba/ }).check();
  await shopper.getByText(/hodnocení/).click();
  await acceptAndPlace(shopper);
  const orderToken = await fakePay(shopper, "Pay");
  const placed = await order(shopper, CZ, orderToken);
  expect(
    sql(`SELECT granted || ':' || source FROM consent_records WHERE subject_id='${email}'
    AND purpose='review_invites' ORDER BY at DESC,id DESC LIMIT 1`),
  ).toBe("true:checkout");
  await admin.goto("/orders");
  await admin.getByRole("link", { name: placed.number, exact: true }).click();
  for (const [action, dialog] of [
    ["Create label", true],
    ["Mark shipped", false],
    ["Mark delivered", false],
  ] as const) {
    await admin.getByRole("button", { name: action, exact: true }).first().click();
    if (dialog) await admin.getByRole("dialog").getByRole("button", { name: action }).click();
  }
  await expect.poll(async () => (await order(shopper, CZ, orderToken)).status).toBe("delivered");
  await shopper.context().close();

  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
  expect(auth.ok()).toBeTruthy();
  const { token } = (await auth.json()) as { token: string };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  const fixtureId = sql(
    `SELECT id FROM orders WHERE number=${placed.number} AND tenant_id='${tenant}'`,
  );
  api.pathname = `/admin/v1/orders/${fixtureId}`;
  const detail = await admin.request.get(api.toString(), {
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
  });
  expect(detail.status()).toBe(200);
  api.pathname = "/admin/v1/flows/test-clock/advance";
  const advanced = await admin.request.post(api.toString(), {
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
    data: { hours: 8 * 24 },
  });
  expect(advanced.ok()).toBeTruthy();
  await expect
    .poll(
      () =>
        sql(`SELECT r.status || ':' || s.status FROM flow_runs r
    JOIN flow_steps s ON s.run_id=r.id WHERE r.source_id='${fixtureId}'`),
      { timeout: 30_000 },
    )
    .toBe("completed:sent");
  const message = await mail(email, "Ohodnoťte svůj nákup");
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
  const link = await reviewLink(browser);
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
