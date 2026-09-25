/**
 * WP17 acceptance (admin): collections (create, pick products, schedule, preview) and the
 * recommendation settings with the "why recommended" view, on the seeded demo shop
 * (`make seed`). The collection is scheduled in the future, so the storefront suite running
 * in parallel never sees it on the home page.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, run, useEnglish } from "./support.ts";

async function signIn(page: Page, email: string): Promise<void> {
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(email, since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
}

const local = (d: Date) => {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
};

test("staff curate a seasonal collection, preview it and see why products are recommended", async ({
  page,
}) => {
  await signIn(page, "owner@lnen.example");
  await page.getByRole("link", { name: "Collections" }).click();
  await expect(page.getByRole("heading", { name: "Collections" })).toBeVisible();

  const name = `Zima ${run}`;
  await page.getByRole("button", { name: "New collection" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New collection" });
  await dialog.getByLabel("Name").fill(name);
  await dialog.getByLabel("CS").fill("Zimní tipy");
  const start = new Date(Date.now() + 30 * 86_400_000);
  await dialog.getByLabel("Starts").fill(local(start));
  await dialog.getByLabel("Ends").fill(local(new Date(start.getTime() + 20 * 86_400_000)));
  await dialog.getByLabel("Find products").fill("Merino");
  // Kobalte checkboxes: click the label, like a user.
  await dialog.getByText("Čepice Merino", { exact: true }).click();
  await dialog.getByText("Mikina Merino", { exact: true }).click();
  await expect(dialog.getByRole("checkbox", { name: "Čepice Merino" })).toBeChecked();
  await expect(dialog.getByText("2 products selected")).toBeVisible();
  await expectAccessible(page, "collection dialog");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeHidden();

  const row = page.getByRole("row", { name: new RegExp(name) });
  await expect(row.getByText("Scheduled")).toBeVisible();
  await row.getByRole("button", { name: new RegExp(`^Preview.*${name}`) }).click();
  const preview = page.getByRole("dialog", { name: `Preview: ${name}` });
  await expect(preview.getByRole("cell", { name: "Čepice Merino" })).toBeVisible();
  await expect(preview.getByRole("cell", { name: "Mikina Merino" })).toBeVisible();
  await expect(preview.getByText(/Strategies tried: Collection/)).toBeVisible();
  await expectAccessible(page, "collection preview");
  await page.keyboard.press("Escape");

  await page.getByRole("link", { name: "Recommendations" }).click();
  await expect(page.getByRole("heading", { name: "Recommendations", level: 1 })).toBeVisible();
  await expect(page.getByRole("checkbox", { name: /Bought together/ })).toBeChecked();
  const why = page.getByRole("region", { name: "Why recommended" });
  await why.getByLabel("Recommend for").selectOption("product");
  await why.getByLabel("Find products").fill("Tričko Basic");
  await why.getByText("Tričko Basic", { exact: true }).click();
  await expect(why.getByRole("checkbox", { name: "Tričko Basic", exact: true })).toBeChecked();
  await why.getByRole("button", { name: "Show" }).click();
  const results = why.getByRole("table", { name: "Recommended products" });
  await expect(results.getByRole("cell", { name: "Kšiltovka Classic" })).toBeVisible();
  await expect(results.getByText("Bought together").first()).toBeVisible();
  await expectAccessible(page, "recommendations settings");

  // Clean up: the collection is deleted again.
  await page.getByRole("link", { name: "Collections" }).click();
  await page
    .getByRole("row", { name: new RegExp(name) })
    .getByRole("button", { name: new RegExp(`^Delete.*${name}`) })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("row", { name: new RegExp(name) })).toHaveCount(0);
});
