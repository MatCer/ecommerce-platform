import { expect, test } from "@playwright/test";
import { CZ, checkoutOf, deferHydration, newPage } from "./support";

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
