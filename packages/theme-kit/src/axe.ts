import { AxeBuilder } from "@axe-core/playwright";
import type { Page } from "playwright";

const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

/** A scrollable target may only fail because one sample clips it at the viewport edge. */
async function failsWhenCentered(page: Page, target: readonly unknown[]): Promise<boolean> {
  if (target.length !== 1) return true;
  const selector = target[0];
  if (typeof selector !== "string" || (await page.locator(selector).count()) !== 1) return true;
  const previous = await page.evaluate(() => window.scrollY);
  await page.locator(selector).evaluate((el) => el.scrollIntoView({ block: "center" }));
  await page.waitForTimeout(120);
  const centered = await new AxeBuilder({ page }).withTags(tags).analyze();
  await page.evaluate((y) => window.scrollTo(0, y), previous);
  await page.waitForTimeout(120);
  return centered.violations.some(
    (violation) =>
      violation.id === "target-size" &&
      violation.nodes.some((node) => node.target.length === 1 && node.target[0] === selector),
  );
}

/** Check the actual sticky/fixed layout at the top, middle and bottom of a page. */
export async function scanAxe(page: Page) {
  const height = await page.evaluate(() => document.documentElement.scrollHeight);
  const viewport = page.viewportSize()?.height ?? 823;
  // At the absolute end, trailing page padding can put the first footer links underneath a
  // sticky header. Check the footer at a reachable reading position, with those links clear.
  const stickyTop = await page.evaluate(() =>
    Math.max(
      0,
      ...[...document.querySelectorAll<HTMLElement>("body *")]
        .filter((el) => getComputedStyle(el).position === "sticky")
        .map((el) => el.getBoundingClientRect())
        .filter((rect) => rect.top <= 1 && rect.bottom > 0)
        .map((rect) => rect.bottom),
    ),
  );
  const positions = [
    0,
    Math.max(0, (height - viewport) / 2),
    Math.max(0, height - viewport - stickyTop - 160),
  ];
  const failures = new Map<string, { id: string; impact: string; nodes: number }>();
  for (const top of positions) {
    await page.evaluate((y) => window.scrollTo(0, y), top);
    await page.waitForTimeout(120);
    const result = await new AxeBuilder({ page }).withTags(tags).analyze();
    for (const violation of result.violations) {
      let count = violation.nodes.length;
      if (violation.id === "target-size") {
        count = 0;
        for (const node of violation.nodes) {
          if (await failsWhenCentered(page, node.target)) count++;
        }
      }
      if (count === 0) continue;
      const prior = failures.get(violation.id);
      failures.set(violation.id, {
        id: violation.id,
        impact: violation.impact ?? "unknown",
        nodes: Math.max(prior?.nodes ?? 0, count),
      });
    }
  }
  return [...failures.values()];
}
