/** WP19 watchdog through the shop edge, checkout-origin confirmation and inventory outbox. */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "../admin/support";
import { CZ, checkoutOf, mail, newPage } from "./support";

let admin: Page;

test.beforeAll(async ({ browser }) => {
  admin = await signInOwner(browser);
});

test.afterAll(async () => {
  await admin.context().close();
});

test("back-in-stock watch confirms, fires once and can unsubscribe", async ({ browser }) => {
  // A product no other spec orders: parallel checkouts would otherwise hold reservations on it
  // and setting the stock to 0 would be refused (`below_reserved`).
  const variant =
    sql(`SELECT v.id FROM variants v JOIN product_translations t ON t.product_id=v.product_id
    WHERE t.slug='cepice-s-bambuli' AND t.locale='cs' ORDER BY v.position LIMIT 1`);
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString());
  expect(auth.ok()).toBeTruthy();
  const { token } = (await auth.json()) as { token: string };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  const adjust = async (on_hand: number) => {
    api.pathname = `/admin/v1/inventory/${variant}/adjustments`;
    const result = await admin.request.post(api.toString(), {
      headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
      data: { on_hand },
    });
    expect(result.status()).toBe(201);
  };

  await adjust(0);
  const email = `watch-${run}@example.test`;
  const shopper = await newPage(browser);
  const subscribed = await shopper.request.post(`${CZ}/_p/watch`, {
    headers: { origin: CZ, "content-type": "application/json" },
    data: { variant_id: variant, kind: "back_in_stock", email },
  });
  expect(subscribed.status()).toBe(202);
  const confirmation = await mail(email, "Potvrďte upozornění na produkt");
  const link = confirmation.Text.match(
    /https?:\/\/[^\s]+\/watch\/confirm\?token=[0-9a-f]{64}/,
  )?.[0];
  if (!link) throw new Error("watch confirmation link missing");
  await shopper.goto(link);
  await shopper.getByRole("button", { name: "Potvrdit" }).click();
  await expect(shopper.getByRole("status")).toContainText("potvrzené");

  await adjust(1);
  const alert = await mail(email, "Upozornění na produkt");
  expect(alert.Text).toContain("cepice-s-bambuli");
  const unsubscribe = alert.Text.match(
    /https?:\/\/[^\s]+\/watch\/unsubscribe\?token=[0-9a-f]{64}/,
  )?.[0];
  if (!unsubscribe) throw new Error("watch unsubscribe link missing");
  await shopper.goto(unsubscribe);
  await expect(shopper.getByRole("heading", { name: "Upozornění zrušeno" })).toBeVisible();
  await expect(shopper.getByRole("status")).toContainText("nedostanete");
  await shopper.context().close();
});

test("abandoned cart mail restores checkout once through the dev clock", async ({ browser }) => {
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  const market = sql(`SELECT id FROM markets WHERE tenant_id='${tenant}' AND is_default`);
  const variant =
    sql(`SELECT v.id FROM variants v JOIN product_translations t ON t.product_id=v.product_id
    WHERE t.slug='tricko-henley' AND t.locale='cs' ORDER BY v.position LIMIT 1`);
  const email = `cart-${run}@example.test`;
  const cart = sql(`INSERT INTO carts(tenant_id,market_id,email,locale,currency,last_activity_at)
    VALUES('${tenant}','${market}','${email}','cs','CZK',now()-interval '2 hours') RETURNING id`)
    .split("\n")[0]
    ?.trim();
  if (!cart) throw new Error("cart fixture was not created");
  sql(`INSERT INTO cart_lines(tenant_id,cart_id,variant_id,quantity)
    VALUES('${tenant}','${cart}','${variant}',1)`);
  sql(`INSERT INTO consent_records(tenant_id,subject_type,subject_id,purpose,granted,text_version,source)
    VALUES('${tenant}','email','${email}','email_marketing',true,'2026-09-25','checkout')`);

  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString());
  expect(auth.ok()).toBeTruthy();
  const { token } = (await auth.json()) as { token: string };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  api.pathname = "/admin/v1/flows/test-clock/advance";
  const advanced = await admin.request.post(api.toString(), {
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
    data: { hours: 1 },
  });
  expect(advanced.ok()).toBeTruthy();

  const reminder = await mail(email, "Zboží čeká v košíku");
  const link = reminder.Text.match(/https?:\/\/[^\s]+\/restore-cart\?token=[0-9a-f]{64}/)?.[0];
  if (!link) throw new Error("cart restore link missing");
  const shopper = await newPage(browser);
  await shopper.goto(link);
  await shopper.getByRole("button", { name: "Pokračovat k pokladně" }).click();
  await expect(shopper).toHaveURL(`${checkoutOf(CZ)}/`);
  const cookies = await shopper.context().cookies(checkoutOf(CZ));
  expect(cookies.some((cookie) => cookie.name === "__Host-cart")).toBeTruthy();
  await shopper.context().close();

  const replay = await newPage(browser);
  await replay.goto(link);
  await replay.getByRole("button", { name: "Pokračovat k pokladně" }).click();
  await expect(replay).toHaveURL(/restore-cart\?invalid=1/);
  await replay.context().close();
});

test("price-drop watch form works without JavaScript and asks for confirmation", async ({
  browser,
}) => {
  const context = await browser.newContext({ javaScriptEnabled: false, locale: "cs-CZ" });
  const shopper = await context.newPage();
  await shopper.goto(`${CZ}/p/tricko-henley`);
  const watch = shopper.locator("#watch");
  await watch.getByText("Hlídat produkt").click();
  await expect(watch.getByText(/Nejdřív vám pošleme e-mail s potvrzovacím odkazem/)).toBeVisible();

  // A malformed price comes back as an error on the same page.
  await watch.getByLabel("klesne cena").check();
  await watch.getByLabel(/Cena klesne na/).fill("12,345");
  await watch.getByLabel("E-mail").fill(`watch-form-${run}@example.test`);
  await watch.getByRole("button", { name: "Hlídat" }).click();
  await expect(shopper).toHaveURL(/[?&]watch=invalid#watch$/);
  await expect(shopper.getByRole("alert")).toContainText("Zkontrolujte e-mail a cenu");

  const email = `watch-form-${run}@example.test`;
  // Headless Chromium without JS keeps a stale hit-test map after the hash jump; a reload of the
  // same GET (what a shopper's scroll would do) settles it.
  await shopper.reload();
  await watch.getByLabel("klesne cena").check();
  await watch.getByLabel(/Cena klesne na/).fill("1,50");
  await watch.getByLabel("E-mail").fill(email);
  await watch.getByRole("button", { name: "Hlídat" }).click();
  await expect(shopper).toHaveURL(/[?&]watch=ok#watch$/);
  await expect(shopper.getByRole("status")).toContainText(
    "poslali jsme na ni e-mail s potvrzovacím odkazem",
  );
  expect(
    sql(`SELECT kind || ':' || target_minor || ':' || status FROM flow_watches
      WHERE email='${email}'`),
  ).toBe("price_drop:150:pending");

  const confirmation = await mail(email, "Potvrďte upozornění na produkt");
  const link = confirmation.Text.match(
    /https?:\/\/[^\s]+\/watch\/confirm\?token=[0-9a-f]{64}/,
  )?.[0];
  if (!link) throw new Error("watch confirmation link missing");
  await shopper.goto(link);
  await expect(shopper.getByRole("heading", { name: "Potvrďte hlídání" })).toBeVisible();
  await shopper.getByRole("button", { name: "Potvrdit hlídání" }).click();
  await expect(shopper.getByRole("status")).toContainText("Hlídání je potvrzené");
  expect(sql(`SELECT status FROM flow_watches WHERE email='${email}'`)).toBe("confirmed");
  await context.close();

  // axe needs JavaScript: check the open form and an outcome message in a normal browser.
  const scripted = await newPage(browser);
  await scripted.goto(`${CZ}/p/tricko-henley?watch=invalid#watch`);
  await expect(scripted.locator("#watch").getByRole("alert")).toBeVisible();
  await expectAccessible(scripted, "product watch form");
  await scripted.goto(link);
  await expectAccessible(scripted, "watch confirmation");
  await scripted.context().close();
});
