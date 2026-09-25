/**
 * WP19 acceptance (admin): automated email settings with validation, the dev-only test clock,
 * the run list and a run's detail with cancellation, on the seeded demo shop (`make seed`).
 * Other suites run in parallel on the same shop (the cart and review flows): settings are
 * only changed in ways that keep their schedules, and restored at the end.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, run, signInOwner, sql } from "./support.ts";

test.describe.configure({ mode: "serial" });

let page: Page;

/** The defaults the parallel cart and review suites rely on. */
function resetDefaults(): void {
  sql(`UPDATE flow_definitions d SET enabled=true, config=CASE d.kind
      WHEN 'abandoned_cart' THEN '{"delays_hours":[1,24,72],"coupon_percent":null}'::jsonb
      ELSE '{"delays_hours":[168],"coupon_percent":null}'::jsonb END
    FROM platform.tenants t WHERE t.id=d.tenant_id AND t.slug='demo'
      AND d.kind IN ('abandoned_cart','review_invite')`);
}

test.beforeAll(async ({ browser }) => {
  resetDefaults();
  page = await signInOwner(browser);
});

test.afterAll(async () => {
  resetDefaults();
  await page.context().close();
});

const card = (name: string) => page.getByRole("form", { name });

test("flow settings validate, save and persist", async () => {
  await page.goto("/");
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Automated emails", exact: true })
    .click();
  await expect(page.getByRole("heading", { name: "Automated emails", level: 1 })).toBeVisible();
  for (const name of ["Abandoned cart", "Stock and price alerts", "Review invitation"])
    await expect(card(name)).toBeVisible();
  await expectAccessible(page, "flows");

  const cart = card("Abandoned cart");
  await expect(cart.getByLabel("Email 1 after (hours)")).toHaveValue("1");
  // Decreasing delays are refused before anything is sent.
  await cart.getByLabel("Email 2 after (hours)").fill("0");
  await cart.getByRole("button", { name: /^Save/ }).click();
  await expect(cart.getByRole("alert")).toContainText("increasing hours");
  await cart.getByLabel("Email 2 after (hours)").fill("24");
  // The coupon only touches the last reminder; the first stays at 1 h for the checkout suite.
  // Kobalte checkboxes: the visually hidden input is toggled through its label.
  await cart.locator("label").filter({ hasText: "one-use discount" }).click();
  await expect(cart.getByRole("checkbox", { name: /one-use discount/ })).toBeChecked();
  await cart.getByLabel("Discount (%)").fill("15");
  await cart.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Abandoned cart: settings saved")).toBeVisible();

  const review = card("Review invitation");
  await review.getByLabel("Send after delivery (hours)").fill("150");
  await review.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Review invitation: settings saved")).toBeVisible();

  await page.reload();
  await expect(card("Abandoned cart").getByLabel("Discount (%)")).toHaveValue("15");
  await expect(card("Review invitation").getByLabel("Send after delivery (hours)")).toHaveValue(
    "150",
  );

  // Restore the defaults the other suites rely on.
  await card("Abandoned cart").locator("label").filter({ hasText: "one-use discount" }).click();
  await expect(card("Abandoned cart").getByLabel("Discount (%)")).toBeHidden();
  await card("Abandoned cart").getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Abandoned cart: settings saved")).toBeVisible();
  await card("Review invitation").getByLabel("Send after delivery (hours)").fill("168");
  await card("Review invitation").getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Review invitation: settings saved").first()).toBeVisible();
  const saved = sql(`SELECT config->>'coupon_percent' IS NULL AND
    (SELECT config->'delays_hours'->>0 FROM flow_definitions d2
     WHERE d2.tenant_id=d.tenant_id AND d2.kind='review_invite') = '168'
    FROM flow_definitions d JOIN platform.tenants t ON t.id=d.tenant_id
    WHERE t.slug='demo' AND d.kind='abandoned_cart'`);
  expect(saved).toBe("t");
});

test("the dev test clock moves flow time forward", async () => {
  await page.goto("/marketing/flows");
  const clock = page.getByRole("region", { name: "Test clock" });
  await expect(clock).toBeVisible();
  const before = await clock.getByTestId("flow-clock-now").textContent();
  await clock.getByLabel("Advance by (hours)").fill("0");
  await clock.getByRole("button", { name: "Advance" }).click();
  await expect(clock.getByText("Enter whole hours from 1 to 2160.")).toBeVisible();
  await clock.getByLabel("Advance by (hours)").fill("1");
  await clock.getByRole("button", { name: "Advance" }).click();
  await expect(page.getByText(/^Flow time is now/)).toBeVisible();
  await expect(clock.getByTestId("flow-clock-now")).not.toHaveText(before ?? "");
});

test("a run shows its details and can be cancelled", async () => {
  // A cart run due far beyond anything the parallel suites advance the clock to.
  const tenant = sql("SELECT id FROM platform.tenants WHERE slug='demo'");
  const market = sql(`SELECT id FROM markets WHERE tenant_id='${tenant}' AND is_default`);
  const cart = sql(`INSERT INTO carts(tenant_id,market_id,email,locale,currency,last_activity_at)
    VALUES('${tenant}','${market}','flows-admin-${run}@example.test','cs','CZK',now())
    RETURNING id`)
    .split("\n")[0]
    ?.trim();
  const id =
    sql(`INSERT INTO flow_runs(tenant_id,definition_id,source_kind,source_id,due_at,config_snapshot)
    SELECT '${tenant}',id,'cart','${cart}',now()+interval '300 days',config FROM flow_definitions
    WHERE tenant_id='${tenant}' AND kind='abandoned_cart' RETURNING id`)
      .split("\n")[0]
      ?.trim();

  await page.goto("/marketing/flows");
  const runs = page.getByRole("region", { name: "Recent runs" });
  await expect(runs.getByRole("columnheader", { name: "Trigger" })).toBeVisible();
  await page.goto(`/marketing/flows/runs/${id}`);
  await expect(page.getByRole("heading", { name: "Flow run" })).toBeVisible();
  await expect(page.getByText("Active", { exact: true })).toBeVisible();
  await expect(page.getByText("1 of 3")).toBeVisible();
  await expect(page.getByText("No email of this run has been processed yet.")).toBeVisible();
  await expectAccessible(page, "flow run");

  await page.getByRole("button", { name: "Cancel run" }).click();
  const dialog = page.getByRole("dialog", { name: "Cancel this run?" });
  await dialog.getByRole("button", { name: "Cancel run" }).click();
  await expect(page.getByText("Run cancelled").first()).toBeVisible();
  await expect(page.getByText("Cancelled", { exact: true })).toBeVisible();
  await expect(page.getByText("Cancelled by staff")).toBeVisible();
  await expect(page.getByRole("button", { name: "Cancel run" })).toBeHidden();
  expect(sql(`SELECT status || ':' || exit_reason FROM flow_runs WHERE id='${id}'`)).toBe(
    "cancelled:manual",
  );
});
