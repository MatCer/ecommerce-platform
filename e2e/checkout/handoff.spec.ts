/**
 * WP26: the shop hands its cart to the checkout in every browser. Firefox sends no (or a
 * `cross-site`) Sec-Fetch-Site on the shop's 303, so the handoff is bound by a cookie instead.
 * Runs in the `chromium` and `firefox` projects.
 */

import { expect, test } from "@playwright/test";
import { checkoutOf, newPage, SK, toCheckout } from "./support";

test("the shop cart reaches the checkout, and the handoff link is single use", async ({
  browser,
}) => {
  const page = await newPage(browser, "sk-SK");
  let handoff = "";
  page.on("request", (r) => {
    if (r.url().startsWith(`${checkoutOf(SK)}/start?h=`)) handoff = r.url();
  });
  await toCheckout(page, SK, "tricko-oversize");
  await expect(page.getByRole("heading", { level: 1, name: "Pokladňa" })).toBeVisible();
  await expect(page.getByTestId("checkout-total")).toContainText("€");
  // Autofill (Bitwarden and other keyword matchers, browsers): plain names, standard tokens
  // without `section-`/`billing` (they made Bitwarden fill the city with the street), and
  // labels bound with `for`. Verified against the real extension by scripts/autofill-check.mjs.
  for (const [name, token, label] of [
    ["email", "email", "E-mail *"],
    ["tel", "tel", "Telefón (nepovinné)"],
    ["name", "name", "Meno a priezvisko"],
    ["street-address", "street-address", "Ulica a číslo domu"],
    ["postal-code", "postal-code", "PSČ"],
    ["city", "address-level2", "Mesto"],
    ["country", "country", "Krajina"],
  ] as const) {
    const field = page.locator(`[name="${name}"]`).first();
    await expect(field).toHaveAttribute("autocomplete", token);
    const labelled = page.locator("label").filter({ hasText: label }).first();
    expect(
      await labelled.evaluate((l) => (l as HTMLLabelElement).control?.getAttribute("name")),
    ).toBe(name);
  }

  expect(handoff).not.toBe("");
  await page.goto(handoff);
  await expect(page.getByText("Odkaz na pokladnu vypršel nebo už byl použit.")).toBeVisible();
  await page.context().close();
});
