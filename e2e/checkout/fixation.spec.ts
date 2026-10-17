import { randomBytes } from "node:crypto";
import { type Browser, type BrowserContext, expect, type Page, test } from "@playwright/test";
import { sql } from "../admin/support";
import {
  acceptAndPlace,
  checkoutReady,
  chooseCashOnDelivery,
  cookieValue,
  detailsForm,
  FRESH_HTTPS,
  JANA,
  pageTokenMeta,
  postDetails,
  RESTARTED,
  receiptNumber,
  restartNotice,
  sameOriginReview,
  saveAddresses,
  saveContact,
  securityContext,
  shopTab,
  startSameOriginCheckout,
} from "./support";

const CART = "__Host-Http-cart";
const CAPABILITY = /^[0-9a-f]{64}$/;
const TOKEN = /^[A-Za-z0-9_-]{43}$/;
const STALE =
  "Tato stránka pokladny už nebyla aktuální, proto se nic neuložilo. Zkontrolujte prosím své údaje a volby.";
const PLANTING =
  "only WebKit 26.6 accepts a script write of a __Host-Http- cookie; Chromium and Firefox refuse it";

const marker = () => `pii${randomBytes(6).toString("hex")}`;
const literal = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const jarOf = async (context: BrowserContext) =>
  (await context.cookies()).map((c) => `${c.name}=${c.value}`).sort();
const snapshotOf = (cap: string) => `checkout_token_hash = sha256(convert_to('${cap}', 'UTF8'))`;

interface Holder {
  context: BrowserContext;
  page: Page;
  cap: string;
  token: string;
  email: string;
}

async function attackerCheckout(browser: Browser, filled: boolean): Promise<Holder> {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const email = `${marker()}@attacker.test`;
  await startSameOriginCheckout(page, FRESH_HTTPS);
  if (filled) {
    await saveContact(page, email);
    await saveAddresses(page, { ...JANA, name: "Attacker Planted" });
    await checkoutReady(page);
    await chooseCashOnDelivery(page);
  }
  const cap = (await cookieValue(context, CART)) ?? "";
  expect(cap, "the attacker holds a capability").toMatch(CAPABILITY);
  const token = (await pageTokenMeta(page).getAttribute("content")) ?? "";
  expect(token, "and the page token of its own platform HTML").toMatch(TOKEN);
  return { context, page, cap, token, email };
}

function plant(theme: Page, cap: string): Promise<string> {
  return theme.evaluate((cap) => {
    document.cookie = `__Host-Http-cart=${cap}; Path=/; Secure; SameSite=Lax`;
    return document.cookie;
  }, cap);
}

async function plantedVictim(browser: Browser, planter: Holder): Promise<Page> {
  const context = await securityContext(browser);
  const page = await context.newPage();
  await page.goto(`${FRESH_HTTPS}/`);
  await plant(page, planter.cap);
  expect(await cookieValue(context, CART), "the theme planted the capability").toBe(planter.cap);
  await page.goto(`${FRESH_HTTPS}/checkout`);
  await expect(pageTokenMeta(page), "the planted checkout opens").toHaveAttribute(
    "content",
    planter.token,
  );
  return page;
}

async function expectPlanterReadsNothing(attacker: Holder, pii: string, placed?: string) {
  const html = (await (await attacker.page.goto(`${FRESH_HTTPS}/checkout`))?.text()) ?? "";
  if (placed) {
    const receipt = attacker.page.getByRole("status", { name: "Děkujeme za objednávku!" });
    await expect(
      receipt,
      "the planter's value reads the receipt of the order it planted",
    ).toContainText(`Objednávka ${placed}`);
    await expect(receipt, "which names only the planter's own email").toContainText(
      `Potvrzení pošleme na ${attacker.email}`,
    );
  } else await expect(restartNotice(attacker.page)).toHaveText(RESTARTED);
  expect(html, "the planter's platform HTML").not.toContain(pii);
  const api = await attacker.page.evaluate(async (token) => {
    const res = await fetch("/_p/checkout", { headers: { "x-page-token": token } });
    return { status: res.status, body: await res.text() };
  }, attacker.token);
  expect(api.status, "the planter's capability reads no checkout").toBe(404);
  expect(api.body).not.toContain(pii);
}

test("a script cannot plant the checkout capability", async ({ browser, browserName }) => {
  test.skip(browserName === "webkit", PLANTING);
  const attacker = await attackerCheckout(browser, false);
  const context = await securityContext(browser);
  const theme = await context.newPage();
  await theme.goto(`${FRESH_HTTPS}/`);
  expect(await plant(theme, attacker.cap)).not.toContain(attacker.cap);
  expect(await cookieValue(context, CART), "the jar refused the script's write").toBeUndefined();
  await context.close();
  await attacker.context.close();
});

test("a capability planted before checkout reads nothing the customer enters", async ({
  browser,
  browserName,
}) => {
  test.skip(browserName !== "webkit", PLANTING);
  const attacker = await attackerCheckout(browser, false);
  const page = await plantedVictim(browser, attacker);
  const pii = marker();
  await saveContact(page, `${pii}@example.test`);
  expect(await cookieValue(page.context(), CART), "the save replaced the planted value").toMatch(
    CAPABILITY,
  );
  expect(await cookieValue(page.context(), CART)).not.toBe(attacker.cap);
  await expectPlanterReadsNothing(attacker, pii);
  await saveAddresses(page, { ...JANA, name: `${pii} Jana`, street: `${pii} 12` });
  await expectPlanterReadsNothing(attacker, pii);
  await checkoutReady(page);
  await chooseCashOnDelivery(page);
  const note = `${pii} poznámka`;
  await page.getByRole("textbox", { name: /Poznámka k objednávce/ }).fill(note);
  await acceptAndPlace(page);
  const number = await receiptNumber(page);
  await expectPlanterReadsNothing(attacker, pii);
  expect(sql(`SELECT notes FROM orders WHERE number = '${number}'`), "the order's note").toBe(note);
  await page.context().close();
  await attacker.context.close();
});

for (const edit of ["contact", "addresses", "note"] as const) {
  const title =
    edit === "note"
      ? "a planted filled checkout placed with only a note shows its planter nothing but its own receipt"
      : `a planted filled checkout stops answering its planter at the customer's first edit: ${edit}`;
  test(title, async ({ browser, browserName }) => {
    test.skip(browserName !== "webkit", PLANTING);
    const attacker = await attackerCheckout(browser, true);
    const page = await plantedVictim(browser, attacker);
    await checkoutReady(page);
    const confirmation = `Potvrzení pošleme na ${attacker.email}`;
    await expect(page.locator("#co-confirmation"), "the planted email is in view").toHaveText(
      confirmation,
    );
    await expect(
      page.getByRole("button", { name: "Objednat s povinností platby" }),
    ).toHaveAccessibleDescription(new RegExp(`^${literal(confirmation)}`));
    const pii = marker();
    if (edit === "contact") {
      await page.getByRole("link", { name: "Upravit Kontakt" }).click();
      await saveContact(page, `${pii}@example.test`);
      await expectPlanterReadsNothing(attacker, pii);
    } else if (edit === "addresses") {
      await page.getByRole("link", { name: "Upravit Adresa" }).click();
      await saveAddresses(page, { ...JANA, name: `${pii} Jana`, street: `${pii} 12` });
      await expectPlanterReadsNothing(attacker, pii);
    } else {
      await page.getByRole("textbox", { name: /Poznámka k objednávce/ }).fill(`${pii} poznámka`);
      await acceptAndPlace(page);
      const number = await receiptNumber(page);
      expect(sql(`SELECT notes FROM orders WHERE number = '${number}'`)).toBe(`${pii} poznámka`);
      await expectPlanterReadsNothing(attacker, pii, number);
    }
    await page.context().close();
    await attacker.context.close();
  });
}

test("theme code writes no details and gets no token", async ({ browser, browserName }) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const email = `${marker()}@example.test`;
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await saveContact(page, email);
  const cap = (await cookieValue(context, CART)) ?? "";
  const token = (await pageTokenMeta(page).getAttribute("content")) ?? "";
  expect(token).toMatch(TOKEN);
  const snapshot = () =>
    sql(`SELECT concat_ws('|', email, phone, billing_address) FROM carts WHERE ${snapshotOf(cap)}`);
  const before = snapshot();
  expect(before).toBe(email);
  const jar = await jarOf(context);

  const theme = await shopTab(context, `${FRESH_HTTPS}/`);
  const attempts = await theme.evaluate(
    async ({ token, email }) => {
      const readable = (res: Response, body: string) => {
        const lines: string[] = [];
        res.headers.forEach((value, name) => {
          lines.push(`${name}: ${value}`);
        });
        return `${lines.join("\n")}\n${body}`;
      };
      const form = (pt?: string) =>
        new URLSearchParams({ ...(pt ? { _pt: pt } : {}), email, phone: "" }).toString();
      const json = (headers: Record<string, string> = {}) => ({
        "content-type": "application/json",
        ...headers,
      });
      const tries: [string, RequestInit][] = [
        [
          "/checkout/contact",
          {
            method: "POST",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body: form(),
          },
        ],
        [
          "/checkout/contact",
          {
            method: "POST",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body: form(token),
          },
        ],
        [
          "/_p/checkout/contact",
          { method: "PUT", headers: json(), body: JSON.stringify({ email, phone: null }) },
        ],
        [
          "/_p/checkout/contact",
          {
            method: "PUT",
            headers: json({ "x-page-token": token }),
            body: JSON.stringify({ email, phone: null }),
          },
        ],
        [
          "/_p/checkout/place-order",
          {
            method: "POST",
            headers: json({ "idempotency-key": crypto.randomUUID() }),
            body: JSON.stringify({ notes: email }),
          },
        ],
      ];
      const out: { path: string; status: number; read: string; body: string }[] = [];
      for (const [path, init] of tries) {
        const res = await fetch(path, init);
        const body = await res.text();
        out.push({ path, status: res.status, read: readable(res, body), body });
      }
      return out;
    },
    { token, email: `theme-${marker()}@example.test` },
  );
  expect(attempts.map((a) => `${a.path} ${a.status}`)).toEqual([
    "/checkout/contact 403",
    "/checkout/contact 403",
    "/_p/checkout/contact 403",
    "/_p/checkout/contact 404",
    "/_p/checkout/place-order 403",
  ]);
  expect(
    attempts.filter((a) => a.status === 403 && a.body !== ""),
    "403s are empty",
  ).toEqual([]);
  for (const a of attempts) {
    expect(a.read, `what theme code reads of ${a.path}`).not.toContain(cap);
    expect(a.read).not.toContain(token);
  }
  expect(await jarOf(context), "no answer set a cookie").toEqual(jar);
  expect(snapshot(), "the customer's snapshot").toBe(before);
  await context.close();

  if (browserName !== "webkit") return;
  const attacker = await attackerCheckout(browser, true);
  const holder = await securityContext(browser);
  const planted = await holder.newPage();
  await planted.goto(`${FRESH_HTTPS}/`);
  await plant(planted, attacker.cap);
  expect(await cookieValue(holder, CART)).toBe(attacker.cap);
  const plantedJar = await jarOf(holder);
  const note = `${marker()} theme`;
  const [answer, placed] = await Promise.all([
    planted.waitForResponse((r) => new URL(r.url()).pathname === "/_p/checkout/place-order"),
    planted.evaluate(
      async ({ token, note }) => {
        const readable = (res: Response, body: string) => {
          const lines: string[] = [];
          res.headers.forEach((value, name) => {
            lines.push(`${name}: ${value}`);
          });
          return `${lines.join("\n")}\n${body}`;
        };
        const headers = { "x-page-token": token };
        const view = (await (await fetch("/_p/checkout", { headers })).json()) as {
          cart: { version: number };
          totals: { total: { amount_minor: number } };
        };
        const res = await fetch("/_p/checkout/place-order", {
          method: "POST",
          headers: {
            ...headers,
            "content-type": "application/json",
            "idempotency-key": crypto.randomUUID(),
          },
          body: JSON.stringify({
            version: view.cart.version,
            total_minor: view.totals.total.amount_minor,
            accept_terms: true,
            accept_withdrawal: true,
            email_marketing: false,
            review_invites: false,
            notes: note,
          }),
        });
        return { status: res.status, read: readable(res, await res.text()) };
      },
      { token: attacker.token, note },
    ),
  ]);
  expect(placed.status, "the shop's JSON placement is closed, to the planted capability too").toBe(
    404,
  );
  const headers = await answer.allHeaders();
  expect(headers["content-type"], "the answer's headers are observed").toContain("json");
  expect(headers["x-cart-token"]).toBeUndefined();
  expect(placed.read).not.toMatch(/^x-cart-token:/m);
  expect(placed.read).not.toContain(attacker.cap);
  expect(placed.read).not.toContain(attacker.token);
  expect(sql(`SELECT count(*) FROM orders WHERE notes = '${note}'`), "no order with the note").toBe(
    "0",
  );
  expect(await jarOf(holder), "the jar is unchanged").toEqual(plantedJar);
  await holder.close();
  await attacker.context.close();
});

test("a stale tab reloads by navigation and repeats no write", async ({ browser }) => {
  const context = await securityContext(browser);
  const review = await context.newPage();
  await sameOriginReview(review, FRESH_HTTPS, `${marker()}@example.test`);
  const old = (await pageTokenMeta(review).getAttribute("content")) ?? "";
  const editor = await shopTab(context, `${FRESH_HTTPS}/checkout?edit=contact`);
  await expect(detailsForm(editor, "contact")).toBeVisible();
  const saver = await shopTab(context, `${FRESH_HTTPS}/checkout?edit=contact`);
  const saved = `${marker()}@example.test`;
  await saveContact(saver, saved);
  const cap = (await cookieValue(context, CART)) ?? "";
  const current = (await pageTokenMeta(saver).getAttribute("content")) ?? "";
  expect(current).not.toBe(old);

  const puts: string[] = [];
  review.on("request", (r) => {
    if (r.method() === "PUT") puts.push(new URL(r.url()).pathname);
  });
  const [refused] = await Promise.all([
    review.waitForResponse((r) => new URL(r.url()).pathname === "/_p/checkout/payment"),
    review.getByRole("radio", { name: /Bankovní převod/ }).click(),
  ]);
  expect(refused.status(), "the old page's payment save").toBe(403);
  await review.waitForURL(`${FRESH_HTTPS}/checkout?error=stale_page`);
  await expect(review.getByRole("status").filter({ hasText: STALE })).toBeVisible();
  await expect(pageTokenMeta(review), "a fresh token").toHaveAttribute("content", current);
  await checkoutReady(review);
  await expect(review.getByRole("radio", { name: /Bankovní převod/ })).not.toBeChecked();
  expect(puts, "one PUT, never repeated").toEqual(["/_p/checkout/payment"]);

  const typed = `${marker()}@example.test`;
  await detailsForm(editor, "contact").locator('[name="email"]').fill(typed);
  expect(await postDetails(editor, "contact"), "the old page's contact post").toBe(403);
  await editor.waitForURL(`${FRESH_HTTPS}/checkout?error=stale_page`);
  await expect(editor.getByRole("status").filter({ hasText: STALE })).toBeVisible();
  expect(await cookieValue(context, CART)).toBe(cap);
  expect(
    sql(`SELECT concat_ws('|', email, coalesce(payment_method::text, 'none'))
         FROM carts WHERE ${snapshotOf(cap)}`),
    "the snapshot holds tab 3's contact and no payment",
  ).toBe(`${saved}|none`);
  expect(sql(`SELECT count(*) FROM carts WHERE email = '${typed}'`), "nothing tab 2 typed").toBe(
    "0",
  );
  await context.close();
});
