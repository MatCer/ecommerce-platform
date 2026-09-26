/**
 * WP14 acceptance: the analytics dashboard. A new shop shows the empty state and working range
 * controls; the seeded demo shop (`make seed`) shows sales after an order is placed, the
 * "consented sessions" funnel label (A20), charts with text alternatives, and passes axe.
 */

import { expect, type Page, test } from "@playwright/test";
import { checkoutReady } from "../checkout/support";
import { rateHeaders, testContext } from "../rate-client";
import { createTenant, expectAccessible, magicLink, run, useEnglish } from "./support.ts";

const port = process.env.HTTP_PORT ?? "8080";
const SK = `http://demo-sk.localhost:${port}`;
const checkoutOf = (shop: string) => shop.replace("://", "://checkout.");

async function signIn(page: Page, email: string): Promise<void> {
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(email, since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
}

/** A paid SK order through the storefront (minimal copy of e2e/checkout/orders.spec.ts). */
async function placeSkOrder(page: Page): Promise<void> {
  await page.context().addCookies([{ name: "consent", value: "", url: SK }]);
  await page.goto(`${SK}/`);
  const model = (await (
    await page.request.get(`${SK}/_p/public/pages/product/tricko-oversize`, {
      headers: rateHeaders(page),
    })
  ).json()) as { product: { variants: { id: string }[] } };
  let added = 0;
  for (const v of model.product.variants) {
    const res = await page.request.post(`${SK}/_p/cart/lines`, {
      headers: { origin: SK, ...rateHeaders(page) },
      data: { variant_id: v.id, quantity: 1 },
    });
    added = res.status();
    if (added !== 409) break;
  }
  expect(added).toBe(200);
  await Promise.all([
    page.waitForURL(`${checkoutOf(SK)}/`),
    page.evaluate(() => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = "/_p/checkout/start";
      document.body.append(form);
      form.submit();
    }),
  ]);
  await checkoutReady(page);
  await page.locator('input[autocomplete="email"]').fill(`analytics-${run}@example.test`);
  await page.locator('input[autocomplete="name"]').fill("Jana Nováková");
  await page.locator('input[autocomplete="street-address"]').fill("Dlhá 12");
  await page.locator('input[autocomplete="postal-code"]').fill("811 01");
  await page.locator('input[autocomplete="address-level2"]').fill("Bratislava");
  await page.getByRole("radio", { name: /Packeta – na adresu/ }).check();
  await page.getByRole("radio", { name: /Testovacia platba/ }).check();
  for (const label of [/^Súhlasím s obchodn/, /^Beriem na vedomie/]) {
    const box = page.getByRole("checkbox", { name: label });
    if (!(await box.isChecked())) await page.getByText(label).click();
    await expect(box).toBeChecked();
  }
  await page.getByRole("button", { name: "Objednať s povinnosťou platby" }).click();
  await page.waitForURL(/\/_p\/fake-pay\//);
  await page.getByRole("button", { name: "Pay", exact: true }).click();
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
}

test("a new shop shows the empty state; the range controls drive the URL", async ({ page }) => {
  const owner = `analytics-owner-${run}@example.test`;
  createTenant(`an-${run}`, `Analytics ${run}`, owner);
  await signIn(page, owner);
  await expect(page.getByText("No data in this range")).toBeVisible();
  await expect(page.getByRole("link", { name: "Invite your team" })).toBeVisible();
  await expectAccessible(page, "dashboard empty");

  const period = page.getByLabel("Period");
  await expect(period).toHaveValue("30");
  await period.selectOption("7");
  await expect(page).toHaveURL(/range=7/);
  await expect(page.getByText("No data in this range")).toBeVisible();

  await period.selectOption("custom");
  await expect(page).toHaveURL(/from=\d{4}-\d{2}-\d{2}&to=\d{4}-\d{2}-\d{2}|to=.*from=/);
  await page.getByLabel("From").fill("2026-01-01");
  await page.getByLabel("To").fill("2026-01-31");
  await expect(page).toHaveURL(/from=2026-01-01/);
  await expect(page).toHaveURL(/to=2026-01-31/);
  // An inverted range is refused in the form, not sent to the API.
  await page.getByLabel("From").fill("2026-02-10");
  await expect(page.getByRole("alert")).toContainText("Choose a start on or before the end");
  await expect(page).toHaveURL(/from=2026-01-01/);
  await expectAccessible(page, "dashboard custom range");

  // The URL is shareable: reloading keeps the range.
  await page.reload();
  await expect(page.getByLabel("Period")).toHaveValue("custom");
  await expect(page.getByLabel("From")).toHaveValue("2026-01-01");
});

test("the demo shop dashboard shows sales, the consented-sessions funnel and passes axe", async ({
  browser,
}) => {
  const shopper = await (await testContext(browser, { locale: "sk-SK" })).newPage();
  await placeSkOrder(shopper);
  await shopper.context().close();

  const ctx = await testContext(browser);
  const page = await ctx.newPage();
  await signIn(page, "owner@lnen.example");

  const sales = page.getByRole("region", { name: "Sales", exact: true });
  await expect(sales.getByText("Revenue", { exact: true }).first()).toBeVisible();
  await expect(sales.getByText("Average order value", { exact: true }).first()).toBeVisible();
  // Amounts are per currency: the paid SK order shows up in euros, never summed with CZK.
  await expect(sales.getByText(/€/).first()).toBeVisible();
  await expect(page.getByRole("img", { name: /Daily revenue \(EUR\)/ })).toBeVisible();

  const traffic = page.getByRole("region", { name: "Traffic" });
  await expect(traffic.getByText("Page requests")).toBeVisible();
  await expect(traffic.getByText("Consented sessions", { exact: true }).first()).toBeVisible();

  const funnel = page.getByRole("region", { name: "Funnel (consented sessions)" });
  await expect(funnel).toBeVisible();
  await expect(funnel.getByText(/Visitors who declined are not in the funnel/)).toBeVisible();
  await expect(funnel.getByRole("listitem")).toHaveCount(5);

  await expect(page.getByRole("region", { name: "Web Vitals (75th percentile)" })).toBeVisible();
  await expectAccessible(page, "dashboard demo");

  // The market filter narrows the dashboard and lands in the URL.
  await page.getByLabel("Market").selectOption({ index: 1 });
  await expect(page).toHaveURL(/market=[0-9a-f-]{36}/);
  await expect(page.getByRole("region", { name: "Sales", exact: true })).toBeVisible();
  await page.getByLabel("Market").selectOption({ label: "All markets" });
  await expect(page).not.toHaveURL(/market=/);

  // Phone width: the filters and tiles stay usable.
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByLabel("Period")).toBeVisible();
  await expect(sales).toBeVisible();
  await ctx.close();
});
