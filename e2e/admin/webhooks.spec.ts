/**
 * WP14 acceptance: an owner adds a webhook, copies the one-time signing secret, and the worker
 * delivers signed events to the mock receiver; a failing receiver shows up as a retrying
 * delivery (HTTP 500) in the log, and a succeeded delivery can be redelivered.
 */
import {
  type APIRequestContext,
  type BrowserContext,
  expect,
  type Page,
  test,
} from "@playwright/test";
import { createTenant, expectAccessible, magicLink, run, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });

const port = process.env.HTTP_PORT ?? "8080";
const MOCKS = `http://mocks.localhost:${port}`;
const owner = `webhooks-${run}@example.test`;
const bucket = `wh-${run}`;
// The worker reaches the mock by its compose service name.
const endpoint = `http://mocks:4010/webhooks/${bucket}`;

interface Received {
  event: string | null;
  webhook_id: string | null;
  signature_valid: boolean | null;
}

let context: BrowserContext;
let page: Page;
let secret = "";

async function received(request: APIRequestContext): Promise<Received[]> {
  const res = await request.get(`${MOCKS}/webhooks/${bucket}`);
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { deliveries: Received[] }).deliveries;
}

async function configure(request: APIRequestContext, config: Record<string, unknown>) {
  const res = await request.put(`${MOCKS}/webhooks/${bucket}/config`, { data: config });
  expect(res.ok()).toBe(true);
}

/** A draft product is enough to publish `product.created`. */
async function createProduct(name: string) {
  await page.goto("/products/new");
  await expect(page.getByRole("heading", { name: "New product" })).toBeVisible();
  await page.getByLabel("Name *").fill(name);
  await page.getByLabel("URL slug").click(); // filled from the name on focus
  await page.getByRole("button", { name: "Save product" }).click();
  await expect(page.getByText("Product created")).toBeVisible();
}

async function openWebhooks() {
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Webhooks", exact: true })
    .click();
  await expect(page.getByRole("heading", { name: "Webhooks", level: 1 })).toBeVisible();
}

/** Refreshes the delivery log until a row matches. */
async function expectDelivery(row: RegExp) {
  const log = page.getByRole("table", { name: "Delivery log" });
  await expect(async () => {
    await page.getByRole("button", { name: "Refresh" }).click();
    await expect(log.getByRole("row", { name: row }).first()).toBeVisible({ timeout: 2_000 });
  }).toPass({ timeout: 60_000 });
}

test.beforeAll(async ({ browser, request }) => {
  createTenant(`wh-${run}`, `Webhooks ${run}`, owner);
  await request.delete(`${MOCKS}/webhooks/${bucket}`);
  context = await browser.newContext();
  page = await context.newPage();
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
});

test.afterAll(async () => {
  await context.close();
});

test("an owner adds a webhook and sees the signing secret once", async () => {
  await openWebhooks();
  await expect(page.getByText("No webhooks yet")).toBeVisible();
  await expectAccessible(page, "webhooks empty");

  await page.getByRole("button", { name: "Add webhook" }).first().click();
  const form = page.getByRole("dialog", { name: "Add webhook" });
  await form.getByLabel("Endpoint URL").fill(endpoint);
  await form.getByLabel("Description (optional)").fill("E2E receiver");
  await form.getByRole("checkbox", { name: "product.created" }).check();
  await expectAccessible(page, "webhook form");
  await form.getByRole("button", { name: "Create" }).click();

  const dialog = page.getByRole("dialog", { name: "Copy the signing secret" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("only time the secret is shown")).toBeVisible();
  await expect(dialog.getByText(/X-Signature: t=/)).toBeVisible();
  secret = await dialog.getByLabel("Signing secret").inputValue();
  expect(secret.length).toBeGreaterThan(20);
  await expectAccessible(page, "webhook secret");
  await dialog.getByRole("button", { name: "I have stored it" }).click();
  await expect(dialog).toBeHidden();

  const row = page.getByRole("row", { name: new RegExp(endpoint) });
  await expect(row).toContainText("product.created");
  await expect(row).toContainText("Active");
  await expect(row).toContainText(`ends in ${secret.slice(-4)}`);
});

test("a product event is delivered with a valid signature and logged as succeeded", async ({
  request,
}) => {
  await configure(request, { secret, status: 200 });
  await createProduct(`Webhook E2E ${run} A`);
  await expect
    .poll(
      async () =>
        (await received(request)).filter(
          (d) => d.event === "product.created" && d.signature_valid === true,
        ).length,
      { timeout: 60_000, intervals: [1_000] },
    )
    .toBeGreaterThanOrEqual(1);

  await openWebhooks();
  await expectDelivery(/product\.created.*Succeeded.*200/);
  const row = page.getByRole("row", { name: /product\.created.*Succeeded/ }).first();
  await row.getByRole("button", { name: /^Payload/ }).click();
  const payload = page.getByRole("dialog", { name: "Payload of product.created" });
  await expect(payload).toContainText("product.created");
  await payload.getByRole("button", { name: "Close" }).first().click();
  await expectAccessible(page, "webhooks with deliveries");
});

test("a failing receiver shows a retrying delivery; a succeeded one can be redelivered", async ({
  request,
}) => {
  await configure(request, { status: 500 });
  const before = (await received(request)).length;
  await createProduct(`Webhook E2E ${run} B`);
  // The mock records the failed attempt too.
  await expect
    .poll(async () => (await received(request)).length, { timeout: 60_000, intervals: [1_000] })
    .toBeGreaterThan(before);

  await openWebhooks();
  await expectDelivery(/product\.created.*Retrying.*500/);
  const retrying = page.getByRole("row", { name: /Retrying/ }).first();
  await expect(retrying.getByRole("button", { name: /^Redeliver/ })).toHaveCount(0);

  await configure(request, { status: 200 });
  const first = (await received(request)).find(
    (d) => d.event === "product.created" && d.signature_valid === true,
  );
  expect(first?.webhook_id).toBeTruthy();
  const copies = async () =>
    (await received(request)).filter((d) => d.webhook_id === first?.webhook_id).length;
  expect(await copies()).toBe(1);

  const succeeded = page.getByRole("row", { name: /Succeeded/ }).last();
  await succeeded.getByRole("button", { name: /^Redeliver/ }).click();
  await expect(page.getByText("Redelivery queued")).toBeVisible();
  await expect.poll(copies, { timeout: 60_000, intervals: [1_000] }).toBe(2);
});
