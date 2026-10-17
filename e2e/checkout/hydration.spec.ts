import { randomBytes } from "node:crypto";
import { expect, test } from "@playwright/test";
import { run, sql } from "../admin/support";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  checkoutReady,
  chooseCashOnDelivery,
  deferHydration,
  detailsForm,
  FRESH_HTTPS,
  fakePay,
  fillAddress,
  fillContactAndAddress,
  isPlacement,
  JANA,
  newPage,
  pageTokenMeta,
  postDetails,
  receiptNumber,
  sameOriginReview,
  securityContext,
  startSameOriginCheckout,
  toCheckout,
} from "./support";

test("the details forms post natively before hydration", async ({ browser }) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  const release = await deferHydration(page);
  try {
    await page.reload({ waitUntil: "commit" });
    const contact = detailsForm(page, "contact");
    const email = contact.locator('[name="email"]');
    await expect(email).toBeEnabled();
    await expect(contact.locator('[name="phone"]')).toBeEnabled();
    await expect(page.locator("astro-island[ssr]"), "no island has hydrated").not.toHaveCount(0);
    const token = await pageTokenMeta(page).getAttribute("content");
    const typed = `pred-hydrataci-${run}@example.test`;
    await email.fill(typed);
    const [posted, status] = await Promise.all([
      page.waitForRequest(
        (r) => r.method() === "POST" && new URL(r.url()).pathname === "/checkout/contact",
      ),
      postDetails(page, "contact", () => email.press("Enter")),
    ]);
    expect(status).toBe(303);
    expect(await posted.headerValue("content-type")).toBe("application/x-www-form-urlencoded");
    const body = new URLSearchParams(posted.postData() ?? "");
    expect(body.get("_pt"), "the token of the platform HTML").toBe(token);
    expect(body.get("email")).toBe(typed);

    const city = detailsForm(page, "addresses").locator('[name="billing_city"]');
    await expect(city).toBeEnabled();
    await fillAddress(page, "billing", JANA);
    expect(await postDetails(page, "addresses", () => city.press("Enter"))).toBe(303);
    const place = page.getByRole("button", { name: "Objednat s povinností platby" });
    const ppl = page.getByRole("radio", { name: /PPL – kurýr/ });
    await expect(place).toBeDisabled();
    await expect(ppl).toBeDisabled();
    release();
    await expect(place).toBeEnabled();
    await expect(ppl).toBeEnabled();
  } finally {
    release();
    await context.close();
  }
});

test("the review's place form posts nothing before hydration and places once after", async ({
  browser,
}) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const email = `pred-objednanim-${randomBytes(6).toString("hex")}@example.test`;
  await sameOriginReview(page, FRESH_HTTPS, email);
  await chooseCashOnDelivery(page);
  const placements: string[] = [];
  page.on("request", (r) => {
    if (isPlacement(r)) placements.push(r.url());
  });
  const release = await deferHydration(page);
  try {
    await page.reload({ waitUntil: "commit" });
    const place = page.getByRole("button", { name: "Objednat s povinností platby" });
    const note = page.getByRole("textbox", { name: /Poznámka k objednávce/ });
    await expect(place).toBeDisabled();
    await expect(note).toBeDisabled();
    await expect(page.locator("astro-island[ssr]"), "no island has hydrated").not.toHaveCount(0);
    await place.click({ force: true });
    await note.press("Enter");
    await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout`);
    release();
    await checkoutReady(page);
    const typed = `Po hydrataci ${run}`;
    await note.fill(typed);
    await acceptAndPlace(page);
    const number = await receiptNumber(page);
    expect(placements, "one placement post, none before hydration").toHaveLength(1);
    expect(
      sql(`SELECT concat_ws('|', string_agg(number::text, ','), string_agg(notes, ','))
           FROM orders WHERE email = '${email}'`),
      "one order, with the note typed after hydration",
    ).toBe(`${number}|${typed}`);
  } finally {
    release();
    await context.close();
  }
});

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
