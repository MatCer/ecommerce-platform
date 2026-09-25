/**
 * Checkout and order placement (WP10) against the seeded demo shop
 * (`make up && make seed && make theme-build`): guest checkout with a Packeta pickup point from
 * the mock widget and the fake gateway, the confirmation page and email, SK home delivery,
 * a failed payment retried, coupon + sale totals, a signed-in customer's account, and a guest
 * order linked after an email-link sign-in (A5).
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, mailpit, run, useEnglish } from "../admin/support";
import {
  acceptAndPlace,
  type CartModel,
  CZ,
  checkoutOf,
  choosePickupPoint,
  fakePay,
  fillContactAndAddress,
  type MailSummary,
  mail,
  newPage,
  type OrderModel,
  order,
  SK,
  toCheckout,
} from "./support";

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
  const ctx = await browser.newContext();
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
  // Newest first: the list starts with this run's latest order.
  const first = page.getByRole("link", { name: /^\d{6,10}$/ }).first();
  const number = (await first.textContent()) ?? "";
  await first.click();
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
