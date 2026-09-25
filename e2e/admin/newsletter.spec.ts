/**
 * WP18 acceptance (admin): subscribers, a segment with a live preview, a campaign built from
 * blocks, its sandboxed preview and a test send (checked in Mailpit), the email log,
 * suppressions and the editable email texts, on the seeded demo shop (`make seed`).
 * Nothing is scheduled: the storefront suite running in parallel never receives this campaign,
 * and everything created here is removed again.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, mailpit, run, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });

async function signIn(page: Page, email: string): Promise<void> {
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(email, since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
}

const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name, exact: true });

async function addBlock(page: Page, name: string): Promise<void> {
  await page.getByRole("button", { name: "Add block", exact: true }).click();
  await page.getByRole("menuitem", { name, exact: true }).click();
  // The menu hands focus back to its trigger as it closes; type only after that.
  await expect(page.getByRole("menu")).toBeHidden();
  await expect(page.getByRole("button", { name: "Add block", exact: true })).toBeFocused();
}

/** Subjects of the mail Mailpit holds for `to`. */
async function subjectsFor(to: string): Promise<string[]> {
  const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
  const body = (await res.json()) as { messages: { Subject: string }[] };
  return body.messages.map((m) => m.Subject);
}

test("staff segment subscribers, build a campaign, preview it and send a test", async ({
  page,
}) => {
  const segment = `Czech ${run}`;
  const campaign = `Autumn ${run}`;
  const tester = `test-${run}@example.com`;
  await signIn(page, "owner@lnen.example");

  await nav(page, "Subscribers").click();
  await expect(page.getByRole("heading", { name: "Subscribers", level: 1 })).toBeVisible();
  await expect(page.getByLabel("Search by email")).toBeVisible();
  await expectAccessible(page, "subscribers");

  // A segment of Czech-speaking subscribers, previewed before it is saved.
  await nav(page, "Segments").click();
  await expect(page.getByRole("heading", { name: "Segments", level: 1 })).toBeVisible();
  await page.getByRole("button", { name: "New segment" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New segment" });
  await dialog.getByLabel("Name", { exact: true }).fill(segment);
  await dialog.getByLabel("New condition").selectOption("locale");
  await dialog.getByRole("button", { name: "Add condition" }).click();
  // Kobalte checkboxes: click the label, like a user.
  await dialog.getByText("Czech", { exact: true }).click();
  await expect(dialog.getByRole("checkbox", { name: "Czech" })).toBeChecked();
  await dialog.getByRole("button", { name: "Preview", exact: true }).click();
  await expect(dialog.getByText(/^\d+ subscribers match$/)).toBeVisible();
  await expectAccessible(page, "segment dialog");
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("row", { name: new RegExp(segment) })).toBeVisible();

  // A campaign with a heading, a text and personalized products, sent to the segment.
  await nav(page, "Campaigns").click();
  await expect(page.getByRole("heading", { name: "Campaigns", level: 1 })).toBeVisible();
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await expect(page.getByRole("heading", { name: "New campaign", level: 1 })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill(campaign);
  await page.getByLabel("Recipients").selectOption({ label: segment });
  await page.getByLabel("Subject", { exact: true }).fill(`Podzimní novinky ${run}`);
  await addBlock(page, "Heading");
  await page.getByLabel("Heading text", { exact: true }).fill("Podzim je tu");
  await addBlock(page, "Text");
  await page
    .getByRole("textbox", { name: "Body text", exact: true })
    .fill("Vybrali jsme pro vás teplé kousky na chladné dny.");
  await addBlock(page, "Personalized products");
  await expectAccessible(page, "campaign editor");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("heading", { name: campaign, level: 1 })).toBeVisible();
  await expect(page.getByText("Draft", { exact: true })).toBeVisible();

  // The saved version rendered in a sandboxed frame.
  const frame = page.locator('iframe[title="Email preview"]');
  await expect(frame).toBeVisible();
  await expect(frame).toHaveAttribute("sandbox", "");
  await expect(page.frameLocator('iframe[title="Email preview"]').getByText("Podzim je tu").first()).toBeVisible();

  await page.getByRole("button", { name: "Send test", exact: true }).click();
  const test_ = page.getByRole("dialog", { name: "Send test" });
  await test_.getByLabel("Email addresses").fill(tester);
  await test_.getByRole("button", { name: "Send", exact: true }).click();
  await expect(test_).toBeHidden();
  await expect(page.getByText("Test emails queued: 1")).toBeVisible();
  await expect
    .poll(() => subjectsFor(tester), { timeout: 30_000 })
    .toContainEqual(expect.stringMatching(/^\[TEST\] Podzimní novinky/));

  // The email log shows the test message.
  await nav(page, "Emails").click();
  await expect(page.getByRole("heading", { name: "Emails", level: 1 })).toBeVisible();
  await page.getByLabel("Recipient").fill(tester);
  const log = page.getByRole("table", { name: "Sent emails" });
  await expect(log.getByRole("cell", { name: tester })).toBeVisible();
  await expectAccessible(page, "email log");
  await log.getByRole("button", { name: /^Details/ }).first().click();
  const detail = page.getByRole("dialog");
  await expect(detail.getByText(tester)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(detail).toBeHidden();

  // Clean up: the draft campaign, then its segment.
  await nav(page, "Campaigns").click();
  await page
    .getByRole("row", { name: new RegExp(campaign) })
    .getByRole("button", { name: new RegExp(`^Delete.*${campaign}`) })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("row", { name: new RegExp(campaign) })).toHaveCount(0);
  await nav(page, "Segments").click();
  await page
    .getByRole("row", { name: new RegExp(segment) })
    .getByRole("button", { name: new RegExp(`^Delete.*${segment}`) })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("row", { name: new RegExp(segment) })).toHaveCount(0);
});

test("owners block an address and edit an email text", async ({ page }) => {
  const blocked = `blocked-${run}@example.com`;
  await signIn(page, "owner@lnen.example");

  await nav(page, "Emails").click();
  await page.getByRole("tab", { name: "Suppressions" }).click();
  await page.getByRole("button", { name: "Block address", exact: true }).click();
  const add = page.getByRole("dialog", { name: "Block address" });
  await add.getByLabel("Address").fill(blocked);
  await add.getByLabel("Note").fill("e2e");
  await add.getByRole("button", { name: "Add", exact: true }).click();
  await expect(add).toBeHidden();
  const table = page.getByRole("table", { name: "Suppressions" });
  const row = table.getByRole("row", { name: new RegExp(blocked) });
  await expect(row.getByText("Blocked by hand")).toBeVisible();
  await expectAccessible(page, "suppressions");
  await row.getByRole("button", { name: new RegExp(`^Remove.*${blocked}`) }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Remove", exact: true }).click();
  await expect(table.getByRole("row", { name: new RegExp(blocked) })).toHaveCount(0);

  // A Czech subject for the newsletter confirmation, then back to the platform default.
  await nav(page, "Email branding").click();
  await expect(page.getByRole("heading", { name: "Email branding", level: 1 })).toBeVisible();
  await expectAccessible(page, "email branding");
  const texts = page.getByRole("region", { name: "Email texts" });
  await texts.getByLabel("Email template").selectOption("newsletter_confirm");
  await texts.getByLabel("Language").selectOption("cs");
  const subject = texts.getByLabel("Subject", { exact: true });
  await subject.fill(`Potvrďte odběr {shop} ${run}`);
  await texts.getByRole("button", { name: "Save texts" }).click();
  await expect(page.getByText("Changes saved").last()).toBeVisible();
  await page.reload();
  await texts.getByLabel("Email template").selectOption("newsletter_confirm");
  await texts.getByLabel("Language").selectOption("cs");
  await expect(subject).toHaveValue(`Potvrďte odběr {shop} ${run}`);
  await subject.fill("");
  await texts.getByRole("button", { name: "Save texts" }).click();
  await expect(page.getByText("Changes saved").last()).toBeVisible();
  await page.reload();
  await texts.getByLabel("Email template").selectOption("newsletter_confirm");
  await texts.getByLabel("Language").selectOption("cs");
  await expect(subject).toHaveValue("");
});
