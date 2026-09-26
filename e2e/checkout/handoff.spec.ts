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
  // Password-manager autofill (Bitwarden) matches on names, not `section-*` autocomplete.
  for (const [name, token] of [
    ["street-address", "street-address"],
    ["postal-code", "postal-code"],
    ["city", "address-level2"],
  ])
    await expect(page.locator(`input[name="${name}"]`).first()).toHaveAttribute(
      "autocomplete",
      new RegExp(`${token}$`),
    );

  expect(handoff).not.toBe("");
  await page.goto(handoff);
  await expect(page.getByText("Odkaz na pokladnu vypršel nebo už byl použit.")).toBeVisible();
  await page.context().close();
});
