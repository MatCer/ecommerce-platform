import { expect, type Locator, type Page } from "@playwright/test";

/** Reach a control through sequential keyboard navigation, never programmatic focus. */
export async function tabTo(page: Page, target: Locator, limit = 100): Promise<void> {
  for (let i = 0; i < limit; i++) {
    if (await target.evaluate((el) => el === document.activeElement)) {
      await expect(target).toBeFocused();
      return;
    }
    await page.keyboard.press("Tab");
  }
  throw new Error(`Control was not reachable with Tab: ${target}`);
}
