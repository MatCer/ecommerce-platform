/**
 * Customer account on the checkout origin (WP9, spec A1, A4, A5, A20) against the seeded demo
 * shop (`make up && make seed && make theme-build`): email-link sign-in via Mailpit, addresses,
 * password, sign-out, password sign-in, cart merge, consent from the shop origin.
 */
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, mailpit, run } from "../admin/support";

const port = process.env.HTTP_PORT ?? "8080";
const shop = `http://demo.localhost:${port}`;
const checkout = `http://checkout.demo.localhost:${port}`;
const email = `zakaznik-${run}@example.test`;
const password = "správné koňské heslo";

test.describe.configure({ mode: "serial" });

let page: Page;

test.beforeAll(async ({ browser }) => {
  page = await (await browser.newContext({ locale: "cs-CZ" })).newPage();
});

test.afterAll(async () => {
  await page.context().close();
});

/** The newest sign-in link for `to` (the checkout's own mail, through the worker). */
async function accountLink(to: string, since: Date): Promise<string> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const body = (await res.json()) as { messages: { ID: string; Created: string }[] };
    const fresh = body.messages
      .filter((m) => new Date(m.Created) >= since)
      .sort((a, b) => b.Created.localeCompare(a.Created))[0];
    if (fresh) {
      const msg = (await (await fetch(`${mailpit}/api/v1/message/${fresh.ID}`)).json()) as {
        Text: string;
      };
      const link = msg.Text.match(/https?:\/\/\S+\/account\/verify\?token=[0-9a-f]{64}/);
      if (link) return link[0];
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no sign-in link for ${to}`);
}

/** Puts `quantity` of a seeded variant into a new shop-origin cart and hands it to checkout. */
async function cartToCheckout(quantity: number): Promise<void> {
  await page.goto(`${shop}/`);
  const product = await page.request.get(`${shop}/_p/public/pages/product/tricko-basic`);
  expect(product.ok()).toBe(true);
  const model = (await product.json()) as { product: { variants: { id: string }[] } };
  const variant = model.product.variants[0]?.id;
  const added = await page.request.post(`${shop}/_p/cart/lines`, {
    headers: { origin: shop },
    data: { variant_id: variant, quantity },
  });
  expect(added.status()).toBe(200);
  // The handoff is a same-site navigation (A1): a form post from the shop page.
  await Promise.all([
    page.waitForURL(`${checkout}/`),
    page.evaluate(() => {
      const form = document.createElement("form");
      form.method = "post";
      form.action = "/_p/checkout/start";
      document.body.append(form);
      form.submit();
    }),
  ]);
}

test("email-link sign-in creates the account and attaches the cart", async () => {
  await cartToCheckout(1);
  await page.goto(`${checkout}/account`);
  await expect(page.getByRole("heading", { level: 1, name: "Přihlášení" })).toBeVisible();
  await expectAccessible(page, "sign-in");
  const since = new Date(Date.now() - 1000);
  const linkForm = page.locator("form").first();
  await linkForm.getByLabel("E-mail").fill(email);
  await linkForm.getByRole("button", { name: "Poslat přihlašovací odkaz" }).click();
  await expect(page.getByRole("status")).toContainText(email);

  const link = await accountLink(email, since);
  expect(link.startsWith(`${checkout}/account/verify?token=`)).toBe(true);
  const verify = await page.goto(link);
  expect(verify?.headers()["referrer-policy"]).toBe("no-referrer");
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await page.waitForURL(`${checkout}/account`);
  await expect(page.getByTestId("signed-in-as")).toContainText(email);
  await expectAccessible(page, "account");

  // The link was single use.
  await page.goto(link);
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await expect(page.getByRole("alert")).toContainText("Odkaz vypršel");

  // The session cookie is host-only on the checkout origin and invisible to scripts.
  const cookies = await page.context().cookies(checkout);
  const sid = cookies.find((c) => c.name === "__Host-sid");
  expect(sid?.httpOnly).toBe(true);
  expect(sid?.sameSite).toBe("Lax");
  expect((await page.context().cookies(shop)).some((c) => c.name === "__Host-sid")).toBe(false);
});

test("adds an address", async () => {
  await page.goto(`${checkout}/account`);
  await page.getByRole("link", { name: /Adresy/ }).click();
  await expect(page.getByText("Zatím nemáte uloženou žádnou adresu.")).toBeVisible();
  await page.getByRole("button", { name: "Přidat adresu" }).click();
  await page.getByLabel("Jméno a příjmení").fill("Jana Nováková");
  await page.getByLabel("Ulice a číslo domu").fill("Dlouhá 12");
  await page.getByLabel("PSČ").fill("110 00");
  await page.getByLabel("Město").fill("Praha");
  await page.getByRole("button", { name: "Uložit adresu" }).click();
  await expect(page.getByRole("status")).toHaveText("Adresa byla uložena.");
  await expect(page.locator("address")).toContainText("Dlouhá 12");
  await expect(page.locator("address")).toContainText("Výchozí");
  await expectAccessible(page, "addresses");
  await page.reload();
  await expect(page.locator("address")).toContainText("110 00 Praha");
});

test("sets a password right after the email-link sign-in, then signs out", async () => {
  await page.goto(`${checkout}/account/security`);
  await expect(page.getByLabel("Současné heslo")).toHaveCount(0);
  await page.getByLabel("Nové heslo").fill(password);
  await page.getByRole("button", { name: "Uložit heslo" }).click();
  await expect(page.getByRole("status")).toContainText("Heslo je uložené");
  await expectAccessible(page, "security");

  await page.goto(`${checkout}/account`);
  await page.getByRole("button", { name: "Odhlásit se" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Přihlášení" })).toBeVisible();
  // Account pages need a session again.
  await page.goto(`${checkout}/account/addresses`);
  await expect(page).toHaveURL(`${checkout}/account?next=/account/addresses`);
});

test("password sign-in merges the new cart with the account's earlier one", async () => {
  await cartToCheckout(2);
  await page.goto(`${checkout}/account?next=/`);
  await page.locator("summary", { hasText: "Heslem" }).click();
  const form = page.locator("details form");
  await form.getByLabel("E-mail").fill(email);
  await form.getByLabel("Heslo").fill("wrong password!!");
  await form.getByRole("button", { name: "Přihlásit se" }).click();
  await expect(page.getByRole("alert")).toHaveText("E-mail nebo heslo nesedí.");
  await form.getByLabel("Heslo").fill(password);
  await form.getByRole("button", { name: "Přihlásit se" }).click();
  // Redirected to the checkout (`next=/`) with both carts merged by variant: 1 + 2.
  await page.waitForURL(`${checkout}/`);
  await expect(page.getByRole("complementary")).toContainText("3× Tričko Basic");
});

test("consent from the shop origin, then changed on the preferences page", async () => {
  await page.goto(`${shop}/`);
  const res = await page.request.post(`${shop}/_p/consent`, {
    headers: { origin: shop },
    data: {
      purposes: { analytics: true, ads: false, personalization: false },
      text_version: "2026-09-25",
    },
  });
  expect(res.status()).toBe(200);
  const cookies = await page.context().cookies(shop);
  expect(cookies.find((c) => c.name === "consent")?.value).toBe("2026-09-25.100--");
  expect(cookies.find((c) => c.name === "__Secure-consent_id")?.httpOnly).toBe(true);

  await page.goto(`${checkout}/consent`);
  await expect(page.getByRole("heading", { level: 1, name: "Nastavení souhlasů" })).toBeVisible();
  await expect(page.getByRole("checkbox", { name: "Měření návštěvnosti" })).toBeChecked();
  await expectAccessible(page, "consent");
  await page.getByText("Personalizace", { exact: true }).click();
  await page.getByRole("button", { name: "Uložit výběr" }).click();
  await expect(page.getByRole("status")).toHaveText("Uloženo.");
  const after = await page.context().cookies(shop);
  expect(after.find((c) => c.name === "consent")?.value).toBe("2026-09-25.10100");
});
