/**
 * Checkout and order placement (WP10) against the seeded demo shop
 * (`make up && make seed`): guest checkout with a Packeta pickup point from
 * the mock widget and the fake gateway, the confirmation page and email, SK home delivery,
 * a failed payment retried, coupon + sale totals, a signed-in customer's account, and a guest
 * order linked after an email-link sign-in (A5).
 */

import { expect, type Locator, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, mailpit, run, sql, useEnglish } from "../admin/support";
import { tabTo } from "../keyboard";
import { testContext } from "../rate-client";
import { hydrated } from "../storefront/support";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  checkoutReady,
  chooseCashOnDelivery,
  choosePickupPoint,
  cookieValue,
  detailsForm,
  FRESH_HTTPS,
  FRESH_SK,
  fakePay,
  fillContactAndAddress,
  JANA,
  type MailSummary,
  mail,
  newPage,
  order,
  pageTokenMeta,
  postDetails,
  receiptNumber,
  SK,
  saveAddresses,
  saveContact,
  securityContext,
  startSameOriginCheckout,
  toCheckout,
} from "./support";

declare global {
  interface Window {
    reportStorageWrite?: (entry: string) => void;
  }
}

/** Email-link sign-in on the checkout origin; lands on `next`. */
async function signIn(page: Page, shop: string, email: string, next: string) {
  const since = new Date(Date.now() - 1000);
  await page.goto(`${checkoutOf(shop)}/account?next=${next}`);
  const form = page.locator("form").first();
  await form.getByLabel("E-mail").fill(email);
  await form.getByRole("button", { name: "Poslat přihlašovací odkaz" }).click();
  await expect(page.getByRole("status")).toContainText(email);
  const deadline = Date.now() + 20_000;
  let link: string | undefined;
  while (!link && Date.now() < deadline) {
    const res = await fetch(
      `${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${email}"`)}`,
    );
    const body = (await res.json()) as { messages: MailSummary[] };
    // Only the sign-in email: the order confirmation may arrive at the same moment.
    const fresh = body.messages.find(
      (m) => new Date(m.Created) >= since && m.Subject.startsWith("Přihlášení"),
    );
    if (fresh) {
      const msg = (await (await fetch(`${mailpit}/api/v1/message/${fresh.ID}`)).json()) as {
        Text: string;
      };
      link = msg.Text.match(/https?:\/\/\S+\/account\/verify\?token=[0-9a-f]{64}/)?.[0];
    }
    if (!link) await new Promise((r) => setTimeout(r, 500));
  }
  if (!link) throw new Error(`no sign-in link for ${email}`);
  await page.goto(link);
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await page.waitForURL(`${checkoutOf(shop)}${next}`);
}

test("guest checkout in CZ: Packeta pickup point, fake payment, confirmation page and email", async ({
  browser,
}) => {
  const page = await newPage(browser);
  const email = `host-${run}@example.test`;
  await toCheckout(page, CZ, "tricko-henley", 2);
  await expect(page.getByRole("heading", { level: 1, name: "Pokladna" })).toBeVisible();
  await expectAccessible(page, "checkout");

  // Nothing chosen yet: the order cannot be placed, and the reason is announced.
  await acceptAndPlace(page);
  await expect(page.getByRole("alert")).toContainText("Vyplňte prosím");

  await fillContactAndAddress(page, email);
  await choosePickupPoint(page, /Z-BOX Praha 1/, true);
  // Keyboard: the payment choice is a native radio group.
  await page.getByRole("radio", { name: /Testovací platba/ }).focus();
  await page.keyboard.press("Space");
  await expect(page.getByRole("radio", { name: /Testovací platba/ })).toBeChecked();
  // Optional consents start unchecked (A20).
  await expect(page.getByRole("checkbox", { name: /novinky a akce/ })).not.toBeChecked();
  await expect(page.getByRole("checkbox", { name: /hodnocení/ })).not.toBeChecked();
  await expectAccessible(page, "checkout filled");
  await acceptAndPlace(page);

  const token = await fakePay(page, "Pay");
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");
  await expect(page.getByTestId("payment-status")).toHaveText("Zaplaceno");
  await expect(page.getByTestId("order-pickup-point")).toContainText("Z-BOX Praha 1");
  await expectAccessible(page, "order page");
  const o = await order(page, CZ, token);
  expect(o).toMatchObject({ status: "confirmed", payment: { status: "paid" } });

  const confirmation = await mail(email, `Potvrzení objednávky ${o.number}`);
  expect(confirmation.Text).toContain("Z-BOX Praha 1");
  expect(confirmation.Text).toContain("Sazba DPH 21 %");
  expect(confirmation.Text).toMatch(/\/o\/[0-9a-f]{64}/);
  await page.context().close();
});

test("keyboard only: product to cart to COD checkout", async ({ browser }) => {
  const page = await newPage(browser);
  await page.goto(`${CZ}/p/mikina-oversize`);
  await hydrated(page);
  const variant = page.getByRole("radio", { name: "Popelavá" });
  await tabTo(page, page.getByRole("radio", { name: "Tyrkysová" }));
  await page.keyboard.press("ArrowRight");
  await expect(variant).toBeFocused();
  await page.keyboard.press("Space");
  const add = page.getByRole("button", { name: "Přidat do košíku" }).first();
  await tabTo(page, add);
  await page.keyboard.press("Enter");
  const drawer = page.getByRole("dialog", { name: /Košík/ });
  await expect(drawer).toBeVisible();
  const checkout = drawer.getByRole("button", { name: "K pokladně" });
  await tabTo(page, checkout);
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(`${checkoutOf(CZ)}/`);
  // The checkout form is SSR HTML until its client:load island attaches input handlers.
  await checkoutReady(page);

  const typeInto = async (selector: string, value: string) => {
    await tabTo(page, page.locator(selector));
    await page.keyboard.type(value);
  };
  await typeInto('input[autocomplete="email"]', `keyboard-${run}@example.test`);
  await typeInto('input[autocomplete="name"]', "Jana Nováková");
  await typeInto('input[autocomplete="street-address"]', "Dlouhá 12");
  await typeInto('input[autocomplete="postal-code"]', "110 00");
  await typeInto('input[autocomplete="address-level2"]', "Praha");
  const shipping = page.getByRole("radio", { name: /PPL/ }).first();
  await tabTo(page, page.getByRole("radio", { name: /Zásilkovna – výdejní místo/ }));
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: /Zásilkovna – na adresu/ })).toBeFocused();
  await page.keyboard.press("ArrowRight");
  await expect(shipping).toBeFocused();
  await page.keyboard.press("Space");
  await expect(shipping).toBeChecked();
  const cod = page.getByRole("radio", { name: /Dobírka/ });
  await expect(cod).toBeEnabled();
  await tabTo(page, page.getByRole("radio", { name: /Testovací platba/ }));
  for (let i = 0; i < 5 && !(await cod.evaluate((el) => el === document.activeElement)); i++) {
    const previous = await page.locator('input[name="payment"]:focus').inputValue();
    await page.keyboard.press("ArrowRight");
    await expect
      .poll(() => page.locator('input[name="payment"]:focus').inputValue())
      .not.toBe(previous);
  }
  await expect(cod).toBeFocused();
  await page.keyboard.press("Space");
  await expect(cod).toBeChecked();
  for (const name of [/^Souhlasím s obchodn/, /^Beru na vědom/]) {
    const checkbox = page.getByRole("checkbox", { name });
    await tabTo(page, checkbox);
    await page.keyboard.press("Space");
    await expect(checkbox).toBeChecked();
  }
  const place = page.getByRole("button", { name: "Objednat s povinností platby" });
  await tabTo(page, place);
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/o\/[0-9a-f]{64}$/);
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");
  await page.context().close();
});

test("checkout input survives delayed island hydration", async ({ browser }) => {
  const page = await newPage(browser);
  await page.route(/checkout\.demo\.localhost.*\.(?:js|mjs)(?:\?|$)/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1200));
    await route.fallback();
  });
  await toCheckout(page, CZ, "tricko-henley");
  const email = `hydration-${run}@example.test`;
  await fillContactAndAddress(page, email);
  await expect(page.locator('input[autocomplete="email"]')).toHaveValue(email);
  await expect(page.locator('input[autocomplete="name"]')).toHaveValue("Jana Nováková");
  await page.context().close();
});

test("keyboard reaches pickup selection and payment after modal focus returns", async ({
  browser,
}) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, `pickup-keyboard-${run}@example.test`);
  const pickup = page.getByRole("radio", { name: /Zásilkovna – výdejní místo/ });
  await tabTo(page, pickup);
  await page.keyboard.press("Space");
  await choosePickupPoint(page, /Z-BOX Praha 1/, true);
  const payment = page.getByRole("radio", { name: /Testovací platba/ });
  await tabTo(page, payment);
  await page.keyboard.press("Space");
  await expect(payment).toBeChecked();
  await expect(page.getByTestId("pickup-point")).toContainText("Z-BOX Praha 1");
  await page.context().close();
});

test("SK checkout with home delivery", async ({ browser }) => {
  const page = await newPage(browser, "sk-SK");
  await toCheckout(page, SK, "tricko-oversize");
  await expect(page.getByRole("heading", { level: 1, name: "Pokladňa" })).toBeVisible();
  await fillContactAndAddress(page, `sk-${run}@example.test`, "Bratislava");
  await page.getByRole("radio", { name: /Packeta – na adresu/ }).check();
  await page.getByRole("radio", { name: /Testovacia platba/ }).check();
  await expect(page.getByTestId("checkout-total")).toContainText("€");
  await acceptAndPlace(page, "Objednať s povinnosťou platby");
  const token = await fakePay(page, "Pay");
  await expect(page.getByTestId("order-status")).toHaveText("Potvrdená");
  const o = await order(page, SK, token);
  expect(o.total.formatted).toContain("€");
  expect(o.payment.status).toBe("paid");
  await page.context().close();
});

test("a lost placement response is replayed after a reload with the same key: one order (A12)", async ({
  browser,
}) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, `ztraceno-${run}@example.test`);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  // The first response is lost after the server committed the order.
  const lost: { number?: string; done?: boolean } = {};
  const keys: string[] = [];
  await page.route("**/_p/checkout/place-order", async (route) => {
    keys.push(route.request().headers()["idempotency-key"] ?? "");
    if (lost.done) return route.continue();
    Object.assign(lost, await (await route.fetch()).json(), { done: true });
    await route.abort("failed");
  });
  await acceptAndPlace(page);
  await expect(page.getByRole("alert")).toContainText("nepodařilo");
  // A reload shows no checkout (the cart is converted); the kept placement is replayed.
  await page.reload();
  const token = await fakePay(page, "Pay");
  expect(keys).toHaveLength(2);
  expect(keys[1]).toBe(keys[0]);
  expect((await order(page, CZ, token)).number).toBe(lost.number);
  await page.context().close();
});

test("a failed payment is retried with a new attempt and succeeds", async ({ browser }) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, `retry-${run}@example.test`);
  await choosePickupPoint(page, /Trafika Vinohrady/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Fail the payment");
  await expect(page.getByTestId("payment-status")).toHaveText("Platba se nezdařila");
  await expect(page.getByTestId("order-status")).toHaveText("Čeká na platbu");
  await page.getByRole("button", { name: "Zaplatit znovu" }).click();
  expect(await fakePay(page, "Pay")).toBe(token);
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");
  await expect(page.getByTestId("payment-status")).toHaveText("Zaplaceno");
  await page.context().close();
});

test("coupon and sale: the order totals match the cart", async ({ browser }) => {
  const page = await newPage(browser);
  // The seeded sale (20 % on hoodies) and the published coupon VITEJTE10.
  const cart = await toCheckout(page, CZ, "mikina-crew", 1, "VITEJTE10");
  expect(cart.discount.amount_minor).toBeGreaterThan(0);
  await fillContactAndAddress(page, `sleva-${run}@example.test`);
  await choosePickupPoint(page, /Z-BOX Brno/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await expect(page.getByRole("complementary")).toContainText("VITEJTE10");
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  const o = await order(page, CZ, token);
  expect(o.subtotal.amount_minor).toBe(cart.subtotal.amount_minor);
  expect(o.discount.amount_minor).toBe(cart.discount.amount_minor);
  expect(o.total.amount_minor).toBe(cart.total.amount_minor + o.shipping_total.amount_minor);
  await page.context().close();
});

test("a signed-in customer finds the order in the account", async ({ browser }) => {
  const page = await newPage(browser);
  const email = `ucet-${run}@example.test`;
  await toCheckout(page, CZ, "tricko-henley");
  await signIn(page, CZ, email, "/");
  await expect(page.getByText(`Nakupujete jako ${email}`)).toBeVisible();
  await expect(page.locator('input[autocomplete="email"]')).toHaveValue(email);
  await fillContactAndAddress(page, null);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  const o = await order(page, CZ, token);
  await page.goto(`${checkoutOf(CZ)}/account`);
  await expect(page.getByTestId("account-orders")).toContainText(o.number);
  await page.getByRole("link", { name: `Zobrazit ${o.number}` }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText(o.number);
  await expectAccessible(page, "account order");
  await page.context().close();
});

test("a guest order joins the account after an email-link sign-in (A5)", async ({ browser }) => {
  const page = await newPage(browser);
  const email = `pozdeji-${run}@example.test`;
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, email);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  const o = await order(page, CZ, token);

  await signIn(page, CZ, email, "/account");
  // `customer.email_verified` → the worker links the guest orders.
  await expect
    .poll(
      async () => {
        await page.reload();
        // Not `textContent()`: it would wait for a list that does not exist yet.
        return (await page.getByTestId("account-orders").allTextContents()).join(" ");
      },
      { timeout: 20_000 },
    )
    .toContain(o.number);
  await page.context().close();
});

test("the admin lists the order and shows its detail; shipping and payment settings render", async ({
  browser,
}) => {
  const shopper = await newPage(browser);
  await toCheckout(shopper, CZ, "mikina-oversize");
  await fillContactAndAddress(shopper, `admin-order-${run}@example.test`);
  await choosePickupPoint(shopper, /Z-BOX Praha 1/);
  await shopper.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(shopper);
  const token = await fakePay(shopper, "Pay");
  const number = (await order(shopper, CZ, token)).number;
  await shopper.context().close();

  const ctx = await testContext(browser);
  const page = await ctx.newPage();
  await useEnglish(page);
  const owner = "owner@lnen.example";
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(owner, since));
  const menu = page.getByRole("navigation", { name: "Main navigation" });
  await menu.getByRole("link", { name: "Orders", exact: true }).click();
  await page.getByRole("link", { name: number, exact: true }).click();
  await expect(page.getByRole("heading", { name: number })).toBeVisible();
  await expect(page.getByText("placed", { exact: true }).first()).toBeVisible();
  await expectAccessible(page, "admin order detail");
  await menu.getByRole("link", { name: "Shipping", exact: true }).click();
  await expect(page.getByText("Packeta pickup point").first()).toBeVisible();
  await expectAccessible(page, "admin shipping");
  await menu.getByRole("link", { name: "Payments", exact: true }).click();
  await expect(page.getByText("Test payment").first()).toBeVisible();
  await expectAccessible(page, "admin payments");
  await ctx.close();
});

const literal = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function expectShopOnly(page: Page) {
  const paths = await page.evaluate(() =>
    Array.from(document.links, (a) => new URL(a.href).pathname),
  );
  expect(paths.filter((p) => /^\/(account|withdraw)(\/|$)/.test(p))).toEqual([]);
  await expect(page.locator('input[type="password"]')).toHaveCount(0);
}

test("same-origin checkout in CZ: native forms, review and a cash-on-delivery order", async ({
  browser,
}) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const writes: string[] = [];
  await page.exposeFunction("reportStorageWrite", (entry: string) => {
    writes.push(entry);
  });
  await page.addInitScript(() => {
    const { setItem, removeItem, clear } = Storage.prototype;
    const report = (what: string) => window.reportStorageWrite?.(`${location.pathname} ${what}`);
    Storage.prototype.setItem = function (this: Storage, key: string, value: string) {
      report(`setItem ${key}`);
      setItem.call(this, key, value);
    };
    Storage.prototype.removeItem = function (this: Storage, key: string) {
      report(`removeItem ${key}`);
      removeItem.call(this, key);
    };
    Storage.prototype.clear = function (this: Storage) {
      report("clear");
      clear.call(this);
    };
  });
  const requests: { method: string; path: string; body: string | null }[] = [];
  page.on("request", (r) =>
    requests.push({ method: r.method(), path: new URL(r.url()).pathname, body: r.postData() }),
  );
  const typeInto = async (field: Locator, value: string) => {
    await tabTo(page, field);
    await page.keyboard.type(value);
  };
  const email = `so-${run}@example.test`;

  await page.goto(`${FRESH_HTTPS}/`);
  await page.evaluate(() => localStorage.setItem("spy", "1"));
  await expect.poll(() => writes, "the storage spy sees a write").toContain("/ setItem spy");
  await startSameOriginCheckout(page, FRESH_HTTPS);
  const contact = detailsForm(page, "contact");
  await expect(contact).toBeVisible();
  await expect(detailsForm(page, "addresses")).toHaveCount(0);
  await expectAccessible(page, "same-origin contact");
  await expectShopOnly(page);
  const emailInput = contact.getByRole("textbox", { name: "E-mail", exact: true });
  await typeInto(emailInput, `so-${run}@example`);
  await page.keyboard.press("Enter");
  await expect(emailInput).toBeFocused();
  expect(
    await emailInput.evaluate((el: HTMLInputElement) => el.validity.patternMismatch),
    "the browser refuses an email without a dot",
  ).toBe(true);
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type(email);
  const cap = await cookieValue(context, "__Host-Http-cart");
  const token = await pageTokenMeta(page).getAttribute("content");
  expect(await postDetails(page, "contact", () => page.keyboard.press("Enter"))).toBe(303);
  const contactPosts = requests.filter(
    (r) => r.method === "POST" && r.path === "/checkout/contact",
  );
  expect(contactPosts, "one contact post, none for the refused email").toHaveLength(1);
  expect(new URLSearchParams(contactPosts[0]?.body ?? "").get("email")).toBe(email);
  const addresses = detailsForm(page, "addresses");
  await expect(addresses).toBeVisible();
  await expect(page.getByRole("region", { name: "1. Kontakt" })).toContainText(email);
  expect(
    await cookieValue(context, "__Host-Http-cart"),
    "the save rotated the capability",
  ).not.toBe(cap);
  await expect(pageTokenMeta(page), "and the page token").not.toHaveAttribute(
    "content",
    token ?? "",
  );

  const field = (name: string) => addresses.locator(`[name="${name}"]`);
  for (const [key, value] of Object.entries(JANA)) await typeInto(field(`billing_${key}`), value);
  const elsewhere = addresses.getByRole("checkbox", { name: "Doručit na jinou adresu" });
  await tabTo(page, elsewhere);
  await page.keyboard.press("Space");
  await expect(elsewhere).toBeChecked();
  const delivery = { name: "Petr Novák", postal_code: "120 00", city: "Praha" };
  for (const [key, value] of Object.entries(delivery))
    await typeInto(field(`shipping_${key}`), value);
  await tabTo(page, addresses.getByRole("button", { name: "Uložit a pokračovat" }));
  expect(
    await postDetails(page, "addresses", () => page.keyboard.press("Enter")),
    "a delivery address without a street",
  ).toBe(422);
  await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout/addresses`);
  const alert = page.getByRole("alert");
  await expect(alert).toHaveText("Zkontrolujte vyplněné údaje.");
  await expect(alert).toBeFocused();
  for (const [key, value] of Object.entries(JANA))
    await expect(field(`billing_${key}`)).toHaveValue(value);
  await expect(elsewhere).toBeChecked();
  for (const [key, value] of Object.entries(delivery))
    await expect(field(`shipping_${key}`)).toHaveValue(value);
  await expect(field("shipping_street")).toHaveValue("");
  await expectAccessible(page, "same-origin addresses with a rejected delivery address");
  await expectShopOnly(page);
  await typeInto(field("shipping_street"), "Krátká 3");
  expect(await postDetails(page, "addresses", () => page.keyboard.press("Enter"))).toBe(303);

  await checkoutReady(page);
  await tabTo(page, page.getByRole("radio", { name: /Zásilkovna – výdejní místo/ }));
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: /Zásilkovna – na adresu/ })).toBeFocused();
  await page.keyboard.press("ArrowRight");
  const ppl = page.getByRole("radio", { name: /PPL – kurýr/ });
  await expect(ppl).toBeFocused();
  await page.keyboard.press("Space");
  await expect(ppl).toBeChecked();
  const cod = page.getByRole("radio", { name: /Dobírka/ });
  await expect(cod).toBeEnabled();
  await tabTo(page, page.getByRole("radio", { name: /Testovací platba/ }));
  for (let i = 0; i < 5 && !(await cod.evaluate((el) => el === document.activeElement)); i++) {
    const previous = await page.locator('input[name="payment"]:focus').inputValue();
    await page.keyboard.press("ArrowRight");
    await expect
      .poll(() => page.locator('input[name="payment"]:focus').inputValue())
      .not.toBe(previous);
  }
  await expect(cod).toBeFocused();
  await page.keyboard.press("Space");
  await expect(cod).toBeChecked();
  const place = page.getByRole("button", { name: "Objednat s povinností platby" });
  await expect(place).toHaveAccessibleDescription(
    new RegExp(
      `^Potvrzení pošleme na ${literal(email)}.*PPL – kurýr.*Doručovací adresa.*Petr Novák, Krátká 3, 120 00 Praha, Česko.*Dobírka`,
    ),
  );
  await expectAccessible(page, "same-origin review");
  await expectShopOnly(page);

  for (const name of [/^Souhlasím s obchodn/, /^Beru na vědom/]) {
    const checkbox = page.getByRole("checkbox", { name });
    await tabTo(page, checkbox);
    await page.keyboard.press("Space");
    await expect(checkbox).toBeChecked();
  }
  const note = `Zazvoňte prosím ${run}`;
  await typeInto(page.getByRole("textbox", { name: /Poznámka k objednávce/ }), note);
  await tabTo(page, place);
  await page.keyboard.press("Enter");
  const number = await receiptNumber(page);
  await expect(page.getByRole("status", { name: "Děkujeme za objednávku!" })).toContainText(
    `Potvrzení pošleme na ${email}`,
  );
  await expectShopOnly(page);
  expect(
    sql(
      `SELECT concat_ws('|', notes, payment_method, email) FROM orders WHERE number = '${number}'`,
    ),
  ).toBe(`${note}|cod|${email}`);
  expect(requests.filter((r) => /^\/(o|_p\/fake-pay)\//.test(r.path))).toEqual([]);
  expect(
    writes.filter((w) => w.startsWith("/checkout")),
    "platform storage writes",
  ).toEqual([]);
  await context.close();
});

test("a server-rejected contact keeps what was typed", async ({ browser }) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  const email = `odmitnuto-${run}@example.test`;
  const contact = detailsForm(page, "contact");
  const emailInput = contact.locator('[name="email"]');
  const phone = contact.locator('[name="phone"]');
  await emailInput.fill(email);
  await phone.fill("abc");
  expect(await postDetails(page, "contact"), "a phone only the server refuses").toBe(422);
  await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout/contact`);
  await expect(phone).toHaveValue("abc");
  await expect(emailInput).toHaveValue(email);
  await expect(phone).toHaveAttribute("aria-invalid", "true");
  await expect(phone).toBeFocused();
  await expect(phone).toHaveAccessibleDescription("Zkontrolujte vyplněné údaje.");
  await expect(emailInput).not.toHaveAttribute("aria-invalid", "true");

  const refused = `odmitnuto-${run}@example`;
  await phone.fill("");
  await emailInput.evaluate((el) => el.removeAttribute("pattern"));
  await emailInput.fill(refused);
  expect(await postDetails(page, "contact"), "an email only the server refuses").toBe(422);
  await expect(emailInput).toHaveValue(refused);
  await expect(emailInput).toHaveAttribute("aria-invalid", "true");
  await expect(emailInput).toBeFocused();
  await expect(emailInput).toHaveAccessibleDescription(/^Zadejte platný e-mail\./);
  await expect(phone).not.toHaveAttribute("aria-invalid", "true");
  await context.close();
});

test("same-origin checkout at phone and tablet widths", async ({ browser }) => {
  for (const viewport of [
    { width: 390, height: 844 },
    { width: 768, height: 1024 },
  ]) {
    const context = await securityContext(browser, { viewport });
    const page = await context.newPage();
    const fits = async (target: Locator, what: string) => {
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow, `${what} at ${viewport.width}px: no horizontal overflow`).toBe(0);
      await tabTo(page, target);
      await expect(target).toBeInViewport();
      const height = (await target.boundingBox())?.height ?? 0;
      expect(height, `${what} at ${viewport.width}px: a 44 px target`).toBeGreaterThanOrEqual(44);
    };
    await startSameOriginCheckout(page, FRESH_HTTPS);
    await fits(detailsForm(page, "contact").getByRole("button"), "contact");
    await saveContact(page, `so-${viewport.width}-${run}@example.test`);
    await fits(detailsForm(page, "addresses").getByRole("button"), "addresses");
    await saveAddresses(page);
    await checkoutReady(page);
    await fits(page.getByRole("button", { name: "Objednat s povinností platby" }), "review");
    await context.close();
  }
});

test("same-origin checkout in Slovak", async ({ browser }) => {
  const context = await securityContext(browser, { locale: "sk-SK" });
  const page = await context.newPage();
  const noRawKeys = async () =>
    expect(await page.locator("body").innerText()).not.toMatch(
      /\b(checkout|address|account|order|cart|legal|consent)\.[a-z_]+/,
    );
  const email = `so-sk-${run}@example.test`;
  await startSameOriginCheckout(page, FRESH_SK, "tricko-oversize");
  await expect(page.getByRole("heading", { level: 1, name: "Pokladňa" })).toBeVisible();
  const contact = detailsForm(page, "contact");
  await expect(contact.getByRole("textbox", { name: "Telefón (nepovinné)" })).toBeVisible();
  await expect(contact.getByRole("button", { name: "Uložiť a pokračovať" })).toBeVisible();
  await noRawKeys();
  await saveContact(page, email);
  const addresses = detailsForm(page, "addresses");
  for (const label of ["Meno a priezvisko", "Ulica a číslo domu", "PSČ", "Mesto"])
    await expect(addresses.getByRole("textbox", { name: label, exact: true })).toBeVisible();
  await expect(addresses.getByRole("combobox", { name: "Krajina" })).toBeVisible();
  await expect(addresses.getByRole("checkbox", { name: "Doručiť na inú adresu" })).toBeVisible();
  await expect(addresses.getByRole("button", { name: "Uložiť a pokračovať" })).toBeVisible();
  await noRawKeys();
  await saveAddresses(page, {
    name: "Ján Novák",
    street: "Hlavná 1",
    postal_code: "811 01",
    city: "Bratislava",
  });
  await checkoutReady(page);
  await chooseCashOnDelivery(page, /Packeta – na adresu/, /Dobierka/);
  await expect(page.locator("#co-confirmation")).toHaveText(`Potvrdenie pošleme na ${email}`);
  await expect(
    page.getByRole("button", { name: "Objednať s povinnosťou platby" }),
  ).toHaveAccessibleDescription(new RegExp(`^Potvrdenie pošleme na ${literal(email)}.*Dobierka`));
  await noRawKeys();
  await acceptAndPlace(page, "Objednať s povinnosťou platby");
  const number = await receiptNumber(page, "Ďakujeme za objednávku!");
  await noRawKeys();
  expect(
    sql(`SELECT concat_ws('|', payment_method, currency, email) FROM orders
         WHERE number = '${number}'`),
  ).toBe(`cod|EUR|${email}`);
  await context.close();
});
