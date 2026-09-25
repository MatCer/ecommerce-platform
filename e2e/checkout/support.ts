/**
 * Shared checkout-origin helpers for the checkout e2e suites (WP10 orders, WP11 payments):
 * a shop cart handed to checkout, the address form, the pickup-point widget, placing the
 * order, the fake gateway and the order model.
 */
import { type Browser, expect, type Page } from "@playwright/test";
import { mailpit } from "../admin/support";

const port = process.env.HTTP_PORT ?? "8080";
export const CZ = `http://demo.localhost:${port}`;
export const SK = `http://demo-sk.localhost:${port}`;
export const checkoutOf = (shop: string) => shop.replace("://", "://checkout.");

export interface Money {
  amount_minor: number;
  formatted: string;
}
export interface CartModel {
  subtotal: Money;
  discount: Money;
  total: Money;
}
export interface OrderModel {
  number: string;
  status: string;
  subtotal: Money;
  discount: Money;
  shipping_total: Money;
  total: Money;
  payment: { status: string };
}

export async function newPage(browser: Browser, locale = "cs-CZ"): Promise<Page> {
  const ctx = await browser.newContext({ locale });
  // A decided consent keeps the theme's banner closed.
  await ctx.addCookies([
    { name: "consent", value: "", url: CZ },
    { name: "consent", value: "", url: SK },
  ]);
  return ctx.newPage();
}

/** Fills a shop cart through the edge (optionally with a coupon) and hands it to checkout. */
export async function toCheckout(
  page: Page,
  shop: string,
  slug: string,
  quantity = 1,
  coupon?: string,
): Promise<CartModel> {
  await page.goto(`${shop}/`);
  const model = (await (
    await page.request.get(`${shop}/_p/public/pages/product/${slug}`)
  ).json()) as {
    product: { variants: { id: string }[] };
  };
  // Every placed order reserves stock (A13): take the first variant that still has enough.
  let added = 0;
  for (const v of model.product.variants) {
    const res = await page.request.post(`${shop}/_p/cart/lines`, {
      headers: { origin: shop },
      data: { variant_id: v.id, quantity },
    });
    added = res.status();
    if (added !== 409) break;
  }
  expect(added).toBe(200);
  if (coupon) {
    const applied = await page.request.post(`${shop}/_p/cart/coupons`, {
      headers: { origin: shop },
      data: { code: coupon },
    });
    expect(applied.status()).toBe(200);
  }
  const cart = (await (await page.request.get(`${shop}/_p/cart`)).json()) as CartModel;
  await Promise.all([
    page.waitForURL(`${checkoutOf(shop)}/`),
    page.evaluate(() => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = "/_p/checkout/start";
      document.body.append(form);
      form.submit();
    }),
  ]);
  return cart;
}

export async function fillContactAndAddress(page: Page, email: string | null, city = "Praha") {
  if (email !== null) await page.locator('input[autocomplete="email"]').fill(email);
  await page.locator('input[autocomplete="section-billing name"]').fill("Jana Nováková");
  await page.locator('input[autocomplete="section-billing street-address"]').fill("Dlouhá 12");
  await page.locator('input[autocomplete="section-billing postal-code"]').fill("110 00");
  await page.locator('input[autocomplete="section-billing address-level2"]').fill(city);
}

/** Chooses the Packeta pickup method and a point in the (mock) widget. */
export async function choosePickupPoint(page: Page, point: RegExp, keyboard = false) {
  await page.getByRole("radio", { name: /Zásilkovna – výdejní místo/ }).check();
  const widget = page.frameLocator('iframe[title="Packeta"]');
  const target = widget.getByRole("button", { name: point });
  await expect(target).toBeVisible();
  // A modal: the checkout behind it is inert (out of the tab order) while it is open.
  await expect(page.locator("main")).toHaveJSProperty("inert", true);
  if (keyboard) {
    // Tab cycles inside the dialog; Enter chooses (A26: keyboard pickup-point selection).
    const first = widget.getByRole("button").first();
    await first.focus();
    await page.keyboard.press("Shift+Tab");
    await expect(widget.getByRole("button", { name: "Zavřít" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(first).toBeFocused();
    await target.focus();
    await page.keyboard.press("Enter");
  } else {
    // The widget removes its frame right after the choice; a plain click would wait on it.
    await target.dispatchEvent("click");
  }
  await expect(page.getByTestId("pickup-point")).toContainText(point);
}

export async function acceptAndPlace(page: Page, placeLabel = "Objednat s povinností platby") {
  for (const label of [/^(Souhlasím|Súhlasím) s obchodn/, /^Ber(u|iem) na v[ěe]dom/]) {
    const box = page.getByRole("checkbox", { name: label });
    if (!(await box.isChecked())) await page.getByText(label).click();
    await expect(box).toBeChecked();
  }
  await page.getByRole("button", { name: placeLabel }).click();
}

/** On the fake gateway's page: pay or fail; returns the order token from the order page. */
export async function fakePay(page: Page, button: "Pay" | "Fail the payment"): Promise<string> {
  await page.waitForURL(/\/_p\/fake-pay\//);
  await expect(page.getByRole("heading", { name: "Test payment" })).toBeVisible();
  await page.getByRole("button", { name: button, exact: true }).click();
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
  return new URL(page.url()).pathname.slice(3);
}

export async function order(page: Page, shop: string, token: string): Promise<OrderModel> {
  return (await (
    await page.request.get(`${checkoutOf(shop)}/_p/orders/${token}`)
  ).json()) as OrderModel;
}

export interface MailSummary {
  ID: string;
  Subject: string;
  Created: string;
}

/** The newest email to `to` whose subject contains `subject` (polls Mailpit). */
export async function mail(to: string, subject: string): Promise<{ Text: string; HTML: string }> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const body = (await res.json()) as { messages: MailSummary[] };
    const hit = body.messages.find((m) => m.Subject.includes(subject));
    if (hit)
      return (await (await fetch(`${mailpit}/api/v1/message/${hit.ID}`)).json()) as {
        Text: string;
        HTML: string;
      };
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no email "${subject}" to ${to}`);
}
