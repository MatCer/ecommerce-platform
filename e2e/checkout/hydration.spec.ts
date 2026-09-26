import { expect, test } from "@playwright/test";
import { run } from "../admin/support";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  deferHydration,
  fakePay,
  fillContactAndAddress,
  newPage,
  toCheckout,
} from "./support";

test("checkout cannot lose contact input before hydration", async ({ browser }) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  const release = await deferHydration(page);
  try {
    await page.reload({ waitUntil: "commit" });
    const email = page.locator('input[autocomplete="email"]');
    await expect(email).toBeVisible();
    await expect(email).toBeDisabled();
    await expect(page.getByRole("button", { name: "Objednat s povinností platby" })).toBeDisabled();
    release();
    await email.fill("hydration@example.test");
    await page.getByRole("button", { name: "Objednat s povinností platby" }).click();
    await expect(email).toHaveValue("hydration@example.test");
    await expect(email).not.toHaveAttribute("aria-invalid", "true");
    await expect(page.getByRole("alert")).toContainText("Vyplňte prosím");
  } finally {
    release();
    await page.context().close();
  }
});

test("checkout waits for the payment save before accepting an order", async ({ browser }) => {
  const page = await newPage(browser);
  let finishSave!: () => void;
  const saving = new Promise<void>((resolve) => {
    finishSave = resolve;
  });
  try {
    await toCheckout(page, CZ, "tricko-henley");
    await fillContactAndAddress(page, `save-race-${run}@example.test`);
    await page.getByRole("radio", { name: /Zásilkovna – na adresu/ }).check();
    await page.route("**/_p/checkout/payment", async (route) => {
      await saving;
      await route.fallback();
    });
    await page.getByRole("radio", { name: /Testovací platba/ }).check();
    const place = page.getByRole("button", { name: "Objednat s povinností platby" });
    await expect(place).toBeDisabled();
    finishSave();
    await expect(place).toBeEnabled();
    await acceptAndPlace(page);
    await fakePay(page, "Pay");
  } finally {
    finishSave();
    await page.context().close();
  }
});

for (const path of ["/account", "/consent", "/withdraw"]) {
  test(`${path} controls wait for hydration`, async ({ browser }) => {
    const page = await newPage(browser);
    const release = await deferHydration(page);
    try {
      await page.goto(`${checkoutOf(CZ)}${path}`, { waitUntil: "commit" });
      const controls = page.locator("main input:visible, main button:visible");
      await expect(controls.first()).toBeVisible();
      for (const control of await controls.all()) await expect(control).toBeDisabled();
      release();
      for (const control of await controls.all()) await expect(control).toBeEnabled();
    } finally {
      release();
      await page.context().close();
    }
  });
}

test("email-link confirmation waits for its event handler before accepting clicks", async ({
  browser,
}) => {
  const page = await newPage(browser);
  const release = await deferHydration(page);
  try {
    await page.goto(`${checkoutOf(CZ)}/account/verify?token=${"a".repeat(64)}`, {
      waitUntil: "commit",
    });
    const confirm = page.getByRole("button", { name: "Přihlásit se", exact: true });
    await expect(confirm).toBeVisible();
    await expect(confirm).toBeDisabled();
    release();
    await expect(confirm).toBeEnabled();
    const consumed = page.waitForResponse(
      (response) =>
        response.url().endsWith("/_p/account/magic-link/consume") &&
        response.request().method() === "POST",
    );
    await confirm.click();
    expect((await consumed).status()).toBe(400);
    await expect(page.getByRole("alert")).toContainText("Odkaz vypršel");
  } finally {
    release();
    await page.context().close();
  }
});
