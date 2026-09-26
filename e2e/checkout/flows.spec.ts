/** WP19 watchdog through the shop edge, checkout-origin confirmation and inventory outbox. */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "../admin/support";
import { rateHeaders, testContext } from "../rate-client";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  choosePickupPoint,
  fakePay,
  fillContactAndAddress,
  mail,
  newPage,
  order,
  toCheckout,
} from "./support";

let admin: Page;

async function advance(hours: number, tenant: string): Promise<void> {
  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
  expect(auth.ok()).toBe(true);
  const { token } = (await auth.json()) as { token: string };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  api.pathname = "/admin/v1/flows/test-clock/advance";
  const response = await admin.request.post(api.toString(), {
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
    data: { hours },
  });
  expect(response.status(), await response.text()).toBe(200);
}

async function openCart(page: Page, email: string): Promise<string> {
  await toCheckout(page, CZ, "tricko-henley");
  const contact = await page.request.put(`${checkoutOf(CZ)}/_p/checkout/contact`, {
    headers: { origin: checkoutOf(CZ), ...rateHeaders(page) },
    data: { email, phone: null },
  });
  expect(contact.status(), await contact.text()).toBe(200);
  const cart = sql(
    `SELECT id FROM carts WHERE email='${email}' AND status='open' ORDER BY created_at DESC LIMIT 1`,
  );
  expect(cart).toMatch(/^[0-9a-f-]{36}$/);
  return cart;
}

async function consentOrder(
  page: Page,
  email: string,
  marketing: boolean,
  reviews = false,
): Promise<string> {
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, email);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  if (marketing) await page.getByText(/novinky a akce/).click();
  if (reviews) await page.getByText(/hodnocení/).click();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  return (await order(page, CZ, token)).number;
}

function runState(cart: string): string {
  return sql(`SELECT status || ':' || next_step || ':' || coalesce(exit_reason,'')
    FROM flow_runs WHERE source_kind='cart' AND source_id='${cart}' ORDER BY created_at DESC LIMIT 1`);
}

function stepState(cart: string, step: number): string {
  return sql(`SELECT s.status || ':' || coalesce(m.status,'') FROM flow_steps s
    JOIN flow_runs r ON r.id=s.run_id LEFT JOIN email_messages m ON m.id=s.message_id
    WHERE r.source_kind='cart' AND r.source_id='${cart}' AND s.step_number=${step}`);
}

function resetClock(tenant: string): void {
  // Repeated runs retain the dev clock; a fresh cart must not start 72 hours overdue.
  sql(`INSERT INTO flow_test_clocks(tenant_id,offset_seconds) VALUES('${tenant}',0)
    ON CONFLICT(tenant_id) DO UPDATE SET offset_seconds=0`);
}

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
  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
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
    headers: { origin: CZ, "content-type": "application/json", ...rateHeaders(shopper) },
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

test("confirmed price-drop watch fires from an admin price change", async ({ browser }) => {
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  const variant = sql(`SELECT v.id FROM variants v JOIN product_translations t
    ON t.product_id=v.product_id WHERE t.slug='cepice-s-bambuli' AND t.locale='cs'
    ORDER BY v.position LIMIT 1`);
  const priceList = sql(
    `SELECT price_list_id FROM markets WHERE tenant_id='${tenant}' AND is_default`,
  );
  const original = Number(
    sql(`SELECT amount_minor FROM variant_prices WHERE tenant_id='${tenant}'
    AND price_list_id='${priceList}' AND variant_id='${variant}'`),
  );
  expect(original).toBeGreaterThan(100);
  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
  expect(auth.ok()).toBe(true);
  const { token } = (await auth.json()) as { token: string };
  const headers = { authorization: `Bearer ${token}`, "x-tenant-id": tenant };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  api.pathname = `/admin/v1/price-lists/${priceList}/prices`;
  const email = `price-watch-${run}@example.test`;
  const shopper = await newPage(browser);
  try {
    const subscribed = await shopper.request.post(`${CZ}/_p/watch`, {
      headers: { origin: CZ, ...rateHeaders(shopper) },
      data: { variant_id: variant, kind: "price_drop", email },
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

    const lowered = await admin.request.put(api.toString(), {
      headers,
      data: { items: [{ variant_id: variant, amount_minor: original - 100 }] },
    });
    expect(lowered.status()).toBe(200);
    const alert = await mail(email, "Upozornění na produkt");
    expect(alert.Text).toContain("cepice-s-bambuli");
    expect(
      sql(`SELECT status FROM flow_watches WHERE tenant_id='${tenant}' AND email='${email}'`),
    ).toBe("fired");
  } finally {
    const restored = await admin.request.put(api.toString(), {
      headers,
      data: { items: [{ variant_id: variant, amount_minor: original }] },
    });
    expect(restored.status()).toBe(200);
    await shopper.context().close();
  }
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

  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
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

test("abandoned cart follows 1/24/72 hours and redeems its final-step coupon once", async ({
  browser,
}) => {
  test.setTimeout(180_000);
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  resetClock(tenant);
  const auth = await admin.request.get(new URL("/api/auth/token", admin.url()).toString(), {
    headers: rateHeaders(admin),
  });
  expect(auth.ok()).toBe(true);
  const { token } = (await auth.json()) as { token: string };
  const api = new URL(admin.url());
  api.hostname = api.hostname.replace(/^admin\./, "api.");
  const headers = { authorization: `Bearer ${token}`, "x-tenant-id": tenant };
  api.pathname = "/admin/v1/flows";
  const definitions = (await (await admin.request.get(api.toString(), { headers })).json()) as {
    items: {
      kind: string;
      enabled: boolean;
      config: { delays_hours: number[]; coupon_percent: number | null };
    }[];
  };
  const original = definitions.items.find((item) => item.kind === "abandoned_cart");
  expect(original).toBeDefined();
  api.pathname = "/admin/v1/flows/abandoned_cart";
  try {
    const configured = await admin.request.put(api.toString(), {
      headers,
      data: { enabled: true, config: { delays_hours: [1, 24, 72], coupon_percent: 15 } },
    });
    expect(configured.status()).toBe(200);

    const email = `cart-coupon-${run}@example.test`;
    const consent = await newPage(browser);
    await consentOrder(consent, email, true);
    await consent.context().close();
    expect(
      sql(`SELECT granted || ':' || source FROM consent_records WHERE subject_id='${email}'
      AND purpose='email_marketing' ORDER BY at DESC,id DESC LIMIT 1`),
    ).toBe("true:checkout");
    const shopper = await newPage(browser);
    const cart = await openCart(shopper, email);
    await advance(1, tenant);
    for (const [step, hours] of [0, 23, 48].entries()) {
      if (step > 0) await advance(hours, tenant);
      await expect.poll(() => stepState(cart, step), { timeout: 30_000 }).toBe("sent:accepted");
      expect(
        sql(`SELECT count(*) FROM flow_steps s JOIN flow_runs r ON r.id=s.run_id
        WHERE r.source_id='${cart}'`),
      ).toBe(String(step + 1));
      expect(runState(cart)).toBe(step === 2 ? "completed:2:all_steps" : `active:${step + 1}:`);
      if (step < 2)
        expect(
          sql(`SELECT abs(extract(epoch FROM (r.due_at - c.last_activity_at -
        interval '${[1, 24, 72][step + 1]} hours'))) < 1 FROM flow_runs r
        JOIN carts c ON c.id=r.source_id WHERE r.source_id='${cart}'`),
        ).toBe("t");
      const message = await mail(email, "Zboží čeká v košíku");
      if (step < 2) expect(message.Text).not.toMatch(/FLOW[0-9A-F]{28}/);
    }
    const reminder = await mail(email, "Zboží čeká v košíku");
    const code = sql(`SELECT coupon_code FROM flow_runs WHERE tenant_id='${tenant}'
      AND source_kind='cart' AND source_id='${cart}'`);
    expect(code).toMatch(/^FLOW[0-9A-F]{28}$/);
    expect(reminder.Text).toContain(code);
    expect(
      sql(`SELECT kind || ':' || value || ':' || usage_limit FROM coupons
      WHERE tenant_id='${tenant}' AND code='${code}'`),
    ).toBe("percent:1500:1");

    const link = reminder.Text.match(/https?:\/\/[^\s]+\/restore-cart\?token=[0-9a-f]{64}/)?.[0];
    if (!link) throw new Error("cart restore link missing");
    await shopper.context().close();
    const redeemer = await newPage(browser);
    try {
      const discounted = await toCheckout(redeemer, CZ, "tricko-henley", 1, code);
      expect(discounted.subtotal.amount_minor).toBeGreaterThan(0);
      expect(discounted.discount.amount_minor).toBe(
        Math.round(discounted.subtotal.amount_minor * 0.15),
      );
      await fillContactAndAddress(redeemer, email);
      await choosePickupPoint(redeemer, /Z-BOX Praha 1/);
      await redeemer.getByRole("radio", { name: /Testovací platba/ }).check();
      await acceptAndPlace(redeemer);
      const token = await fakePay(redeemer, "Pay");
      expect((await order(redeemer, CZ, token)).discount.amount_minor).toBe(
        discounted.discount.amount_minor,
      );
      expect(sql(`SELECT used_count FROM coupons WHERE code='${code}'`)).toBe("1");
      const second = await newPage(browser);
      try {
        await second.goto(`${CZ}/`);
        const model = (await (
          await second.request.get(`${CZ}/_p/public/pages/product/tricko-henley`, {
            headers: rateHeaders(second),
          })
        ).json()) as { product: { variants: { id: string }[] } };
        const added = await second.request.post(`${CZ}/_p/cart/lines`, {
          headers: { origin: CZ, ...rateHeaders(second) },
          data: { variant_id: model.product.variants[0]?.id, quantity: 1 },
        });
        expect(added.status()).toBe(200);
        const reuse = await second.request.post(`${CZ}/_p/cart/coupons`, {
          headers: { origin: CZ, ...rateHeaders(second) },
          data: { code },
        });
        expect(reuse.status()).toBe(422);
        expect(
          (await (
            await second.request.get(`${CZ}/_p/cart`, {
              headers: rateHeaders(second),
            })
          ).json()) as { discount: { amount_minor: number } },
        ).toMatchObject({ discount: { amount_minor: 0 } });
        const secondCart = await toCheckout(second, CZ, "tricko-henley");
        expect(secondCart.discount.amount_minor).toBe(0);
        await fillContactAndAddress(second, `cart-reuse-${run}@example.test`);
        await choosePickupPoint(second, /Z-BOX Praha 1/);
        await second.getByRole("radio", { name: /Testovací platba/ }).check();
        await acceptAndPlace(second);
        const secondToken = await fakePay(second, "Pay");
        expect((await order(second, CZ, secondToken)).discount.amount_minor).toBe(0);
        expect(
          sql(`SELECT count(*) FROM coupon_redemptions WHERE coupon_id=(
          SELECT id FROM coupons WHERE code='${code}')`),
        ).toBe("1");
      } finally {
        await second.context().close();
      }
      await advance(24, tenant);
      expect(runState(cart)).toBe("completed:2:all_steps");
      expect(
        sql(`SELECT count(*) FROM flow_steps s JOIN flow_runs r ON r.id=s.run_id
        WHERE r.source_id='${cart}'`),
      ).toBe("3");
    } finally {
      await redeemer.context().close();
    }
  } finally {
    if (original) {
      api.pathname = "/admin/v1/flows/abandoned_cart";
      expect(
        (
          await admin.request.put(api.toString(), {
            headers,
            data: { enabled: original.enabled, config: original.config },
          })
        ).status(),
      ).toBe(200);
    }
  }
});

test("abandoned cart respects refused consent and stops on withdrawal, emptying and conversion", async ({
  browser,
}) => {
  test.setTimeout(300_000);
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  resetClock(tenant);
  const refusedEmail = `cart-refused-${run}@example.test`;
  const refusedOrder = await newPage(browser);
  await consentOrder(refusedOrder, refusedEmail, false);
  await refusedOrder.context().close();
  expect(
    sql(`SELECT count(*) FROM consent_records WHERE subject_id='${refusedEmail}'
    AND purpose='email_marketing'`),
  ).toBe("0");
  const refused = await newPage(browser);
  const refusedCart = await openCart(refused, refusedEmail);
  await advance(1, tenant);
  expect(runState(refusedCart)).toBe("");
  expect(
    sql(`SELECT count(*) FROM email_messages WHERE to_email='${refusedEmail}'
    AND subject LIKE '%Zboží čeká%'`),
  ).toBe("0");
  await refused.context().close();

  for (const reason of ["withdrawn", "empty", "converted"] as const) {
    const email = `cart-${reason}-${run}@example.test`;
    const consent = await newPage(browser);
    await consentOrder(consent, email, true);
    await consent.context().close();
    const shopper = await newPage(browser);
    try {
      const cart = await openCart(shopper, email);
      await advance(1, tenant);
      await expect.poll(() => stepState(cart, 0), { timeout: 30_000 }).toBe("sent:accepted");
      const reminder = await mail(email, "Zboží čeká v košíku");
      if (reason === "withdrawn") {
        const link = reminder.Text.match(
          /https?:\/\/[^\s]+\/flows\/unsubscribe\?token=[0-9a-f]{64}/,
        )?.[0];
        if (!link) throw new Error("flow unsubscribe link missing");
        await shopper.goto(link);
        await expect(shopper).toHaveURL(/\/flows\/unsubscribed$/);
        expect(
          sql(`SELECT granted FROM consent_records WHERE subject_id='${email}'
          AND purpose='email_marketing' ORDER BY at DESC,id DESC LIMIT 1`),
        ).toBe("f");
      } else if (reason === "empty") {
        // Checkout contact rotates the shop capability; the checkout has no line-removal
        // endpoint. Remove the line as a supplemental integration fixture.
        sql(`DELETE FROM cart_lines WHERE cart_id='${cart}'`);
      } else {
        await fillContactAndAddress(shopper, email);
        await choosePickupPoint(shopper, /Z-BOX Praha 1/);
        await shopper.getByRole("radio", { name: /Testovací platba/ }).check();
        await acceptAndPlace(shopper);
        await fakePay(shopper, "Pay");
      }
      await advance(23, tenant);
      await expect.poll(() => runState(cart)).toMatch(/^cancelled:1:/);
      expect(
        sql(`SELECT count(*) FROM flow_steps s JOIN flow_runs r ON r.id=s.run_id
        WHERE r.source_id='${cart}'`),
      ).toBe("1");
      expect(
        sql(`SELECT count(*) FROM email_messages WHERE to_email='${email}'
        AND subject='Zboží čeká v košíku'`),
      ).toBe("1");
    } finally {
      await shopper.context().close();
    }
  }
});

test("price-drop watch form works without JavaScript and asks for confirmation", async ({
  browser,
}) => {
  const context = await testContext(browser, { javaScriptEnabled: false, locale: "cs-CZ" });
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
