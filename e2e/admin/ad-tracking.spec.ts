/**
 * WP20 acceptance against the seeded demo shop (`make up && make seed`):
 * the owner configures the four ad platforms and tests a connection; orders and a beacon of a
 * visitor who allowed ads reach the vendor mocks with hashed identifiers, a visitor who refused
 * ads sends nothing; withdrawing consent cancels a delivery held by a paused platform; a vendor
 * failure is retried and ends as a failed delivery in the log.
 */

import { createHash } from "node:crypto";
import {
  type APIRequestContext,
  type Browser,
  type BrowserContext,
  expect,
  type Page,
  test,
} from "@playwright/test";
import { checkoutReady } from "../checkout/support";
import { testContext } from "../rate-client";
import { expectAccessible, magicLink, run, sql, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });

const port = process.env.HTTP_PORT ?? "8080";
const CZ = `http://demo.localhost:${port}`;
const MOCKS = `http://mocks.localhost:${port}`;
const checkoutOf = (shop: string) => shop.replace("://", "://checkout.");
const sha = (s: string) => createHash("sha256").update(s).digest("hex");

interface Recorded {
  path: string;
  status: number;
  errors: string[];
  body: Record<string, unknown>;
}

let admin: BrowserContext;
let page: Page;

async function recorded(request: APIRequestContext, platform: string): Promise<Recorded[]> {
  const res = await request.get(`${MOCKS}/ads/${platform}/requests`);
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { requests: Recorded[] }).requests;
}

/** Every JSON body the mocks accepted for `platform`, as text (to search for hashes). */
async function accepted(request: APIRequestContext, platform: string): Promise<string> {
  return (await recorded(request, platform))
    .filter((r) => r.status < 300)
    .map((r) => JSON.stringify(r.body))
    .join("\n");
}

/** A shopper who answers the theme's consent banner: ads only, or refuse everything. */
async function shopper(browser: Browser, ads: boolean): Promise<Page> {
  const ctx = await testContext(browser, { locale: "cs-CZ" });
  const p = await ctx.newPage();
  await p.goto(`${CZ}/p/tricko-henley`);
  const banner = p.getByRole("region", { name: "Souhlas s cookies" });
  await expect(banner).toBeVisible();
  if (ads) {
    const box = banner.getByRole("checkbox", { name: "Reklama" });
    if (!(await box.isVisible())) await banner.getByRole("button", { name: "Nastavení" }).click();
    await box.check();
    await banner.getByRole("button", { name: "Uložit výběr" }).click();
  } else {
    await banner.getByRole("button", { name: "Odmítnout" }).click();
  }
  await expect(banner).toBeHidden();
  // The platform recorded the choice (the edge set the anonymous subject).
  await expect
    .poll(async () => (await ctx.cookies(CZ)).some((c) => c.name === "__Secure-consent_id"))
    .toBe(true);
  return p;
}

/** Withdraws ads on the checkout origin's preferences page. */
async function withdrawAds(p: Page) {
  await p.goto(`${checkoutOf(CZ)}/consent`);
  const ads = p.getByRole("checkbox", { name: "Reklama" });
  await expect(ads).toBeChecked();
  await p.getByText("Reklama", { exact: true }).click();
  await expect(ads).not.toBeChecked();
  await p.getByRole("button", { name: "Uložit výběr" }).click();
  await expect(p.getByRole("status")).toHaveText("Uloženo.");
}

/** A paid CZ order with home delivery (Czech crowns, so Sklik takes it too). */
async function placeOrder(p: Page, email: string): Promise<string> {
  const model = (await (
    await p.request.get(`${CZ}/_p/public/pages/product/tricko-henley`)
  ).json()) as { product: { variants: { id: string }[] } };
  let added = 0;
  for (const v of model.product.variants) {
    const res = await p.request.post(`${CZ}/_p/cart/lines`, {
      headers: { origin: CZ },
      data: { variant_id: v.id, quantity: 1 },
    });
    added = res.status();
    if (added !== 409) break;
  }
  expect(added).toBe(200);
  await Promise.all([
    p.waitForURL(`${checkoutOf(CZ)}/`),
    p.evaluate(() => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = "/_p/checkout/start";
      document.body.append(form);
      form.submit();
    }),
  ]);
  await checkoutReady(p);
  await p.locator('input[autocomplete="email"]').fill(email);
  await p.locator('input[autocomplete="tel"]').first().fill("606 666 666");
  await p.locator('input[autocomplete="name"]').fill("Jana Nováková");
  await p.locator('input[autocomplete="street-address"]').fill("Dlouhá 12");
  await p.locator('input[autocomplete="postal-code"]').fill("110 00");
  await p.locator('input[autocomplete="address-level2"]').fill("Praha");
  await p.getByRole("radio", { name: /Zásilkovna – na adresu/ }).check();
  await p.getByRole("radio", { name: /Testovací platba/ }).check();
  for (const label of [/^Souhlasím s obchodn/, /^Beru na vědomí/]) {
    const box = p.getByRole("checkbox", { name: label });
    if (!(await box.isChecked())) await p.getByText(label).click();
    await expect(box).toBeChecked();
  }
  await p.getByRole("button", { name: "Objednat s povinností platby" }).click();
  await p.waitForURL(/\/_p\/fake-pay\//);
  await p.getByRole("button", { name: "Pay", exact: true }).click();
  await p.waitForURL(/\/o\/[0-9a-f]{64}$/);
  return sql(`SELECT number FROM orders WHERE email='${email}' ORDER BY placed_at DESC LIMIT 1`);
}

const platformRow = (name: RegExp) =>
  page.getByRole("table", { name: "Ad platforms" }).getByRole("row", { name });

async function openAdTracking() {
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Ad tracking", exact: true })
    .click();
  await expect(page.getByRole("heading", { name: "Ad tracking", level: 1 })).toBeVisible();
}

async function configure(name: string, fields: Record<string, string>) {
  const row = page.getByRole("table", { name: "Ad platforms" }).getByRole("row", { name });
  await row.getByRole("button", { name: /^Configure/ }).click();
  const dialog = page.getByRole("dialog", { name: `Configure ${name}` });
  for (const [label, value] of Object.entries(fields))
    await dialog.getByLabel(label, { exact: true }).fill(value);
  // Idempotent (a rerun finds the platform configured): tick only what is not ticked.
  const tick = async (label: string | RegExp) => {
    const box = dialog.getByRole("checkbox", { name: label });
    if (!(await box.isChecked())) await dialog.getByText(label, { exact: true }).click();
    await expect(box).toBeChecked();
  };
  await tick(/\(CZK\)$/);
  const test = dialog.getByRole("checkbox", { name: "Test mode" });
  if (await test.isChecked()) await dialog.getByText("Test mode", { exact: true }).click();
  await tick("Send events");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toBeHidden();
  // A rerun may find it paused by the pause test of an earlier run.
  const resume = row.getByRole("button", { name: /^Resume/ });
  if (await resume.isVisible()) await resume.click();
  await expect(row).toContainText("Sending");
}

/** Refreshes the delivery log until a row matches. */
async function expectDelivery(row: RegExp) {
  const log = page.getByRole("table", { name: "Delivery log" });
  await expect(async () => {
    await page.getByRole("button", { name: "Refresh" }).click();
    await expect(log.getByRole("row", { name: row }).first()).toBeVisible({ timeout: 2_000 });
  }).toPass({ timeout: 90_000 });
}

// A matching row from a previous run must not satisfy an asynchronous assertion.
function purchaseDelivery(email: string, platform: string) {
  return sql(`SELECT d.status || ':' || coalesce(d.response_code::text, '')
    FROM ad_deliveries d JOIN orders o ON o.id=d.order_id AND o.tenant_id=d.tenant_id
    JOIN platform.tenants t ON t.id=d.tenant_id
    WHERE t.slug='demo' AND o.email='${email}' AND d.platform='${platform}'
      AND d.event_name='purchase'`);
}

test.beforeAll(async ({ browser, request }) => {
  for (const p of ["meta", "ga4", "google", "sklik"])
    await request.delete(`${MOCKS}/ads/${p}/requests`);
  admin = await testContext(browser);
  page = await admin.newPage();
  await useEnglish(page);
  const since = new Date(Date.now() - 1000);
  await page.goto("/login");
  await page.getByLabel("Email").fill("owner@lnen.example");
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink("owner@lnen.example", since));
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
});

test.afterAll(async ({ request }) => {
  await request.delete(`${MOCKS}/ads/sklik/requests`);
  await admin.close();
});

test("the owner configures the four platforms and tests a connection", async () => {
  await openAdTracking();
  await expect(platformRow(/Sklik \(Seznam\)/)).toContainText("Not needed");
  await expectAccessible(page, "ad tracking");

  await configure("Meta (Facebook, Instagram)", {
    "Dataset (pixel) ID": "1234567890",
    "Access token": `EAA-e2e-${run}`,
  });
  await configure("Google Analytics 4", {
    "Measurement ID": "G-E2E12345",
    "Measurement Protocol API secret": "ga4-e2e-secret",
  });
  await configure("Google Ads", {
    "Customer ID": "123-456-7890",
    "Conversion action ID": "555",
    "OAuth client ID": "client.apps.googleusercontent.com",
    "OAuth client secret": "client-secret",
    "OAuth refresh token": "1//refresh-e2e",
  });
  await configure("Sklik (Seznam)", { "Server-to-server SEM ID": "sem-e2e" });

  const meta = platformRow(/Meta \(Facebook/);
  await expect(meta).toContainText(`ends in ${`EAA-e2e-${run}`.slice(-4)}`);
  // Credentials are write-only: reopening the form shows them empty.
  await meta.getByRole("button", { name: /^Configure/ }).click();
  const dialog = page.getByRole("dialog", { name: "Configure Meta (Facebook, Instagram)" });
  await expect(dialog.getByLabel("Access token", { exact: true })).toHaveValue("");
  await expect(dialog.getByText("Leave a field empty to keep")).toBeVisible();
  await expectAccessible(page, "ad platform form");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  await page
    .getByRole("row", { name: /Google Ads/ })
    .getByRole("button", { name: /^Test connection/ })
    .click();
  await expect(page.getByText("Google Ads accepted the connection")).toBeVisible();
});

test("only a visitor who allowed ads reaches the platforms, with hashed identifiers", async ({
  browser,
  request,
}) => {
  const refused = `ads-no-${run}@example.test`;
  const allowed = `ads-yes-${run}@example.test`;
  const no = await shopper(browser, false);
  const refusedOrder = await placeOrder(no, refused);
  await no.context().close();

  const yes = await shopper(browser, true);
  // The SDK beacon (ads consent alone is enough for it to send), from the page itself.
  const beacon = await yes.evaluate(async () => {
    const body = JSON.stringify({ events: [{ type: "page_view", template: "home" }], path: "/" });
    return (await fetch("/_p/e", { method: "POST", body })).status;
  });
  expect(beacon).toBe(204);
  const allowedOrder = await placeOrder(yes, allowed);
  await yes.context().close();

  for (const platform of ["sklik", "meta", "google", "ga4"]) {
    await expect
      .poll(() => purchaseDelivery(allowed, platform === "google" ? "google_ads" : platform), {
        timeout: 90_000,
        intervals: [1_000],
      })
      .toBe(`succeeded:${platform === "ga4" ? 204 : 200}`);
  }
  const purchaseFor = (platform: string, number: string) =>
    recorded(request, platform).then((rows) =>
      rows.filter((r) => r.status < 300 && JSON.stringify(r.body).includes(`"${number}"`)),
    );
  for (const platform of ["sklik", "meta", "google", "ga4"]) {
    await expect.poll(async () => (await purchaseFor(platform, allowedOrder)).length).toBe(1);
    expect(purchaseDelivery(refused, platform === "google" ? "google_ads" : platform)).toBe("");
    expect(await purchaseFor(platform, refusedOrder)).toEqual([]);
  }

  const meta = (await recorded(request, "meta")).filter((r) => r.status < 300);
  const purchase = (await purchaseFor("meta", allowedOrder))
    .map((r) => (r.body.data as Record<string, unknown>[])[0] ?? {})
    .find((e) => e.event_name === "Purchase");
  expect(purchase).toMatchObject({ action_source: "website" });
  const user = purchase?.user_data as Record<string, unknown>;
  expect(user.em).toEqual([sha(allowed)]);
  expect(user.ph).toEqual([sha("420606666666")]); // Meta: digits with the country code
  expect(user.client_user_agent).toBeTruthy();
  expect(meta.some((r) => JSON.stringify(r.body).includes('"PageView"'))).toBe(true);
  const sklik = (await purchaseFor("sklik", allowedOrder))[0];
  // Seznam: E.164 with +.
  expect(JSON.stringify(sklik?.body)).toContain(`"ph":"${sha("+420606666666")}"`);
  const ga4 = JSON.stringify((await purchaseFor("ga4", allowedOrder))[0]?.body);
  expect(ga4).toContain('"purchase"');
  expect(ga4).toContain(`"transaction_id":"${allowedOrder}"`);
  expect(ga4).not.toContain("@"); // no PII to GA4

  // Nothing of the visitor who refused ads, and no raw addresses anywhere.
  for (const p of ["meta", "google", "sklik", "ga4"]) {
    const all = await accepted(request, p);
    expect(all).not.toContain(sha(refused));
    expect(all).not.toContain(allowed);
  }

  await openAdTracking();
  await expectDelivery(/Meta \(Facebook, Instagram\).*purchases.*Sent.*200/);
  await expectAccessible(page, "ad tracking with deliveries");
});

test("withdrawing consent cancels what a paused platform still holds", async ({
  browser,
  request,
}) => {
  await openAdTracking();
  const meta = platformRow(/Meta \(Facebook/);
  await meta.getByRole("button", { name: /^Pause/ }).click();
  await expect(meta).toContainText("Paused");

  const email = `ads-withdraw-${run}@example.test`;
  const p = await shopper(browser, true);
  await placeOrder(p, email);
  await page
    .getByRole("combobox", { name: "Platform" })
    .selectOption({ label: "Meta (Facebook, Instagram)" });
  await expectDelivery(/purchases.*Paused/);

  await withdrawAds(p);
  await p.context().close();
  await expect.poll(() => purchaseDelivery(email, "meta")).toBe("cancelled:");
  await expectDelivery(/purchases.*Cancelled/);
  await meta.getByRole("button", { name: /^Resume/ }).click();
  await expect(meta).toContainText("Sending");
  // Cancellation is terminal; resume only queues paused rows. No arbitrary worker sleep.
  expect(purchaseDelivery(email, "meta")).toBe("cancelled:");
  expect(await accepted(request, "meta")).not.toContain(sha(email));
});

test("a failing platform is retried and ends as a failed delivery", async ({
  browser,
  request,
}) => {
  await request.put(`${MOCKS}/ads/sklik/config`, { data: { status: 503, fail_times: null } });
  const p = await shopper(browser, true);
  const email = `ads-fail-${run}@example.test`;
  await placeOrder(p, email);
  await p.context().close();

  await openAdTracking();
  await page.getByRole("combobox", { name: "Platform" }).selectOption({ label: "Sklik (Seznam)" });
  await expect
    .poll(() => purchaseDelivery(email, "sklik"), { timeout: 90_000 })
    .toBe("retrying:503");
  await expectDelivery(/purchases.*Retrying.*503/);
  // A rejected payload is not retried: the next attempt fails for good.
  await request.put(`${MOCKS}/ads/sklik/config`, { data: { status: 400, fail_times: null } });
  await expect.poll(() => purchaseDelivery(email, "sklik"), { timeout: 90_000 }).toBe("dead:400");
  await expectDelivery(/purchases.*Failed.*400/);
  await request.delete(`${MOCKS}/ads/sklik/requests`);
});
