/**
 * Payment adapters (WP11) against the seeded demo shop (`make up && make seed &&
 * make theme-build`): Stripe through the test simulator (a declined payment, a retry, the
 * success, and a redelivered webhook that changes nothing), bank transfer with the SPAYD and
 * PAY by square QR codes paid by a camt.053 statement uploaded in the admin, a short payment
 * in the exceptions queue, and cash on delivery delivered → collected in cash (rounding) →
 * remitted in the order detail.
 */
import { createHmac } from "node:crypto";
import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, magicLink, run, useEnglish } from "../admin/support";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  choosePickupPoint,
  fillContactAndAddress,
  newPage,
  type OrderModel,
  order,
  SK,
  toCheckout,
} from "./support";

const port = process.env.HTTP_PORT ?? "8080";
const API = `http://api.localhost:${port}`;
/** The compose default (`STRIPE_WEBHOOK_SECRET`) of the local simulator. */
const WEBHOOK_SECRET = process.env.STRIPE_WEBHOOK_SECRET ?? "whsec_local_simulator_0123456789";
const OWNER = "owner@lnen.example";

/** Places the checkout with `method` and home delivery; returns the order token. */
async function place(page: Page, method: RegExp, placeLabel?: string): Promise<string> {
  await page.getByRole("radio", { name: /na adresu/ }).check();
  await page.getByRole("radio", { name: method }).check();
  await acceptAndPlace(page, placeLabel);
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
  return new URL(page.url()).pathname.slice(3);
}

async function payment(page: Page, token: string) {
  return (await (
    await page.request.get(`${checkoutOf(CZ)}/_p/orders/${token}/payment`)
  ).json()) as {
    status: string;
    attempt: { id: string } | null;
  };
}

/** A camt.053 statement with one credit to the demo CZ account. */
function camt053(id: string, amountMinor: number, vs: string): Buffer {
  const amount = (amountMinor / 100).toFixed(2);
  return Buffer.from(`<?xml version="1.0" encoding="UTF-8"?>
<Document xmlns="urn:iso:std:iso:20022:tech:xsd:camt.053.001.02"><BkToCstmrStmt>
<GrpHdr><MsgId>${id}</MsgId><CreDtTm>2026-09-25T18:00:00</CreDtTm></GrpHdr>
<Stmt><Id>${id}</Id><Acct><Id><IBAN>CZ6508000000192000145399</IBAN></Id><Ccy>CZK</Ccy></Acct>
<Ntry><Amt Ccy="CZK">${amount}</Amt><CdtDbtInd>CRDT</CdtDbtInd><Sts>BOOK</Sts>
<BookgDt><Dt>2026-09-25</Dt></BookgDt><AcctSvcrRef>${id}</AcctSvcrRef>
<NtryDtls><TxDtls><RltdPties><Dbtr><Nm>Jana Nováková</Nm></Dbtr></RltdPties>
<RmtInf><Strd><CdtrRefInf><Ref>${vs}</Ref></CdtrRefInf></Strd></RmtInf></TxDtls></NtryDtls>
</Ntry></Stmt></BkToCstmrStmt></Document>`);
}

/** One admin sign-in for the whole file: the auth service rate-limits magic links. */
let admin: Page;

test.beforeAll(async ({ browser }) => {
  const ctx = await browser.newContext();
  admin = await ctx.newPage();
  await useEnglish(admin);
  await admin.goto("/login");
  const since = new Date(Date.now() - 1000);
  await admin.getByLabel("Email").fill(OWNER);
  await admin.getByRole("button", { name: "Email me a sign-in link" }).click();
  await admin.goto(await magicLink(OWNER, since));
});

test.afterAll(async () => {
  await admin.context().close();
});

const nav = (p: Page, name: string) =>
  p.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name, exact: true });

async function uploadStatement(admin: Page, file: Buffer, name: string): Promise<string> {
  await nav(admin, "Bank transactions").click();
  await admin.getByLabel("Statement file").setInputFiles({
    name,
    mimeType: "application/xml",
    buffer: file,
  });
  await admin.getByRole("button", { name: "Import" }).click();
  const report = admin.getByTestId("import-report");
  await expect(report).toBeVisible();
  return (await report.textContent()) ?? "";
}

test("Stripe simulator: declined, retried, paid; a redelivered event changes nothing", async ({
  browser,
}) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, `stripe-${run}@example.test`);
  const token = await place(page, /Platba kartou/);

  const simulator = page.getByRole("region", { name: "Stripe – testovací simulátor" });
  await expect(simulator).toBeVisible();
  await expectAccessible(page, "stripe simulator");
  await simulator.getByRole("button", { name: "Simulovat zamítnutou platbu" }).click();
  // The event goes through the webhook receiver and the worker; the page polls.
  await expect(page.getByTestId("payment-status")).toHaveText("Platba se nezdařila", {
    timeout: 20_000,
  });
  await page.getByRole("button", { name: "Zaplatit znovu" }).click();
  await expect(simulator).toBeVisible();
  await simulator.getByRole("button", { name: "Simulovat úspěšnou platbu" }).click();
  await expect(page.getByTestId("payment-status")).toHaveText("Zaplaceno", { timeout: 20_000 });
  await page.reload();
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");

  // Stripe redelivers: the same event id, correctly signed, is stored once and ignored.
  const attempt = (await payment(page, token)).attempt?.id ?? "";
  const body = JSON.stringify({
    id: `evt_sim_${attempt.replaceAll("-", "")}_succeeded`,
    object: "event",
    type: "payment_intent.succeeded",
    livemode: false,
    data: { object: {} },
  });
  const t = Math.floor(Date.now() / 1000);
  const sig = createHmac("sha256", WEBHOOK_SECRET).update(`${t}.${body}`).digest("hex");
  const res = await page.request.post(`${API}/webhooks/stripe`, {
    headers: { "content-type": "application/json", "stripe-signature": `t=${t},v1=${sig}` },
    data: body,
  });
  expect(res.status()).toBe(200);
  expect(await res.json()).toEqual({ received: true, new: false });
  const forged = await page.request.post(`${API}/webhooks/stripe`, {
    headers: {
      "content-type": "application/json",
      "stripe-signature": `t=${t},v1=${"0".repeat(64)}`,
    },
    data: body,
  });
  expect(forged.status()).toBe(401);
  const o: OrderModel = await order(page, CZ, token);
  expect(o.payment.status).toBe("paid");
  await page.context().close();
});

test("bank transfer: SPAYD QR, a camt.053 upload pays it, a short payment waits in the queue", async ({
  browser,
}) => {
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, `prevod-${run}@example.test`);
  const token = await place(page, /Bankovní převod/);
  const bank = page.getByTestId("bank-transfer");
  await expect(bank).toBeVisible();
  const o = await order(page, CZ, token);
  await expect(bank.getByTestId("order.bank_vs")).toHaveText(o.number);
  await expect(bank.getByTestId("order.bank_account")).toContainText("CZ6508000000192000145399");
  await expect(bank.getByTestId("bank-qr")).toHaveAttribute("data-qr-kind", "spayd");
  await expect(bank.locator("svg[role=img]")).toBeVisible();
  await expect(page.getByTestId("payment-status")).toHaveText("Nezaplaceno");
  await expectAccessible(page, "bank transfer instructions");

  // A second order is paid 100 Kč short.
  const short = await newPage(browser);
  await toCheckout(short, CZ, "tricko-henley");
  await fillContactAndAddress(short, `kratce-${run}@example.test`);
  const shortToken = await place(short, /Bankovní převod/);
  const s = await order(short, CZ, shortToken);

  const paid = await uploadStatement(
    admin,
    camt053(`E2E-${run}-1`, o.total.amount_minor, o.number),
    "statement.xml",
  );
  expect(paid).toContain("1 matched");
  // The same statement again: nothing new (A25).
  const again = await uploadStatement(
    admin,
    camt053(`E2E-${run}-1`, o.total.amount_minor, o.number),
    "statement.xml",
  );
  expect(again).toContain("0 new, 1 already imported");
  await page.reload();
  await expect(page.getByTestId("payment-status")).toHaveText("Zaplaceno");
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");
  await expect(page.getByTestId("bank-transfer")).toHaveCount(0);

  const partial = await uploadStatement(
    admin,
    camt053(`E2E-${run}-2`, s.total.amount_minor - 10_000, s.number),
    "short.xml",
  );
  expect(partial).toContain("0 matched, 1 to resolve");
  await nav(admin, "Payment exceptions").click();
  const row = admin.getByTestId("bank-tx").filter({ hasText: s.number });
  await expect(row).toContainText("Partial payment");
  await expectAccessible(admin, "payment exceptions");
  await short.context().close();
  await page.context().close();
});

test("SK bank transfer shows a PAY by square QR code", async ({ browser }) => {
  const page = await newPage(browser, "sk-SK");
  await toCheckout(page, SK, "tricko-oversize");
  await fillContactAndAddress(page, `sk-prevod-${run}@example.test`, "Bratislava");
  await place(page, /Bankový prevod/, "Objednať s povinnosťou platby");
  const bank = page.getByTestId("bank-transfer");
  await expect(bank.getByTestId("bank-qr")).toHaveAttribute("data-qr-kind", "pay_by_square");
  await expect(bank.getByTestId("order.bank_account")).toContainText("SK9611000000002918599669");
  await expect(bank.getByTestId("order.bank_amount")).toContainText("€");
  await page.context().close();
});

test("COD: delivered, collected in cash with rounding, remitted", async ({ browser }) => {
  const page = await newPage(browser);
  // The seeded sale (20 % on hoodies) and VITEJTE10 give a total in haléře, so cash rounds.
  await toCheckout(page, CZ, "mikina-crew", 1, "VITEJTE10");
  await fillContactAndAddress(page, `dobirka-${run}@example.test`);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Dobírka/ }).check();
  await acceptAndPlace(page);
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
  const token = new URL(page.url()).pathname.slice(3);
  const o = await order(page, CZ, token);
  expect(o.status).toBe("confirmed");
  expect(o.total.amount_minor % 100).not.toBe(0);

  await nav(admin, "Orders").click();
  await admin.getByRole("link", { name: o.number, exact: true }).click();
  const cod = admin.locator("section", {
    has: admin.getByRole("heading", { name: "Cash on delivery", exact: true }),
  });
  await expect(cod.getByTestId("cod-state")).toHaveText("On its way");
  await cod.getByRole("button", { name: "Mark delivered" }).click();
  await expect(cod.getByTestId("cod-state")).toHaveText("Delivered");
  await cod.getByLabel("Tender").selectOption("cash");
  await cod.getByLabel("Collected by").selectOption("carrier");
  await cod.getByRole("button", { name: "Record collection" }).click();
  await expect(cod.getByTestId("cod-state")).toHaveText("Collected");
  const rounded = Math.floor((o.total.amount_minor + 50) / 100) * 100;
  const after = await order(page, CZ, token);
  expect(after.total.amount_minor).toBe(rounded);
  expect(after.payment.status).toBe("paid");
  await expect(admin.getByRole("cell", { name: "Rounding", exact: true })).toBeVisible();
  await cod.getByLabel("Payout reference (optional)").fill(`PAYOUT-${run}`);
  await cod.getByRole("button", { name: "Confirm remittance" }).click();
  await expect(cod.getByTestId("cod-state")).toHaveText("Remitted");
  await expectAccessible(admin, "admin COD");
  await page.context().close();
});
