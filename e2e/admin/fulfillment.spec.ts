/**
 * Fulfillment, invoicing and refunds (WP12) against the seeded demo shop (`make up && make
 * seed && make theme-build`), the carrier/ČNB mocks and the worker:
 * - prepaid CZ order: invoice on the payment date (A17.1) emailed as a PDF, a Packeta label
 *   from the admin, the shipped email with the tracking link, delivered by a tracking poll,
 *   then a partial refund with a credit note (A15);
 * - cash on delivery with PPL: invoiced on dispatch (A17.2), delivered by tracking (A16), the
 *   cash collected;
 * - an EUR order of the CZ VAT payer: the invoice carries the CZK recap at the ČNB rate.
 */
import { expect, type Page, test } from "@playwright/test";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  choosePickupPoint,
  fakePay,
  fillContactAndAddress,
  mail,
  newPage,
  order,
  SK,
  toCheckout,
} from "../checkout/support";
import {
  enqueueJob,
  eventually,
  expectAccessible,
  mailWithAttachments,
  run,
  signInOwner,
  sql,
} from "./support";

const port = process.env.HTTP_PORT ?? "8080";
const MOCKS = `http://mocks.localhost:${port}`;

let admin: Page;

test.beforeAll(async ({ browser }) => {
  admin = await signInOwner(browser);
});

test.afterAll(async () => {
  await admin.context().close();
});

const nav = (p: Page, name: string) =>
  p.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name, exact: true });

async function openOrder(number: string) {
  // A dialog left open by the previous test (e.g. a refund result) keeps the page inert.
  if (await admin.getByRole("dialog").count()) await admin.keyboard.press("Escape");
  await nav(admin, "Orders").click();
  await admin.getByRole("link", { name: number, exact: true }).click();
  await expect(admin.getByRole("heading", { name: new RegExp(number) }).first()).toBeVisible();
}

/** Clicks an order action; actions with a form open a dialog confirmed by the same label. */
async function action(name: string, dialog = false) {
  await admin.getByRole("button", { name, exact: true }).first().click();
  if (dialog) {
    await admin.getByRole("dialog").getByRole("button", { name, exact: true }).click();
    await expect(admin.getByRole("dialog")).toHaveCount(0);
  }
}

/** The issued invoice of an order once its PDF is rendered: number, DUZP, payment date. */
function invoiceOf(number: string, kind = "invoice") {
  const row = sql(
    `SELECT i.number, i.taxable_supply_date, i.currency, coalesce(i.document->'czk_recap'->'rate'->>'rate_milli', '')
     FROM invoices i JOIN orders o ON o.id = i.order_id
     WHERE o.number = ${Number(number)} AND i.kind = '${kind}' AND i.pdf_key IS NOT NULL
     ORDER BY i.created_at DESC LIMIT 1`,
  );
  if (!row) return null;
  const [doc, duzp, currency, rate] = row.split("|");
  return { doc: doc ?? "", duzp: duzp ?? "", currency: currency ?? "", rate: rate ?? "" };
}

const pragueDate = (column: string, table: string, where: string) =>
  sql(`SELECT (${column} AT TIME ZONE 'Europe/Prague')::date FROM ${table} WHERE ${where}`);

test("prepaid CZ order: invoice on payment, Packeta label, shipped + delivered by tracking, partial refund with a credit note", async ({
  browser,
}) => {
  const email = `wp12-prepaid-${run}@example.test`;
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley", 2);
  await fillContactAndAddress(page, email);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  const o = await order(page, CZ, token);
  expect(o.payment.status).toBe("paid");

  // A17.1: the worker issues the invoice dated on the payment, renders it and emails the PDF.
  const inv = await eventually(() => invoiceOf(o.number), 45_000);
  const paidOn = pragueDate(
    "a.completed_at",
    "orders o JOIN payment_attempts a ON a.id = o.paid_attempt_id",
    `o.number = ${Number(o.number)}`,
  );
  expect(inv.duzp).toBe(paidOn);
  expect(inv.doc).toMatch(/^FV\d{9}$/);
  const invoiceMail = await mailWithAttachments(email, `Faktura k objednávce ${o.number}`);
  expect(invoiceMail.Attachments.map((a) => a.FileName)).toEqual([`${inv.doc}.pdf`]);

  // The customer's order page lists the invoice with a short-lived download link.
  await page.goto(`${checkoutOf(CZ)}/o/${token}`);
  const download = page.getByRole("link", { name: /Stáhnout PDF/ }).first();
  await expect(download).toBeVisible();
  const pdf = await page.request.get((await download.getAttribute("href")) ?? "");
  expect((await pdf.body()).subarray(0, 5).toString()).toBe("%PDF-");

  // Admin: the Packeta label for the chosen pickup point.
  await openOrder(o.number);
  await expect(admin.getByRole("cell", { name: inv.doc, exact: true })).toBeVisible();
  await expect(
    admin.getByText("Invoice templates require accountant approval before real use"),
  ).toBeVisible();
  await action("Create label", true);
  const packets = await eventually(async () => {
    const res = await fetch(`${MOCKS}/packeta/_packets?number=${o.number}`);
    const list = (await res.json()) as { id: number; barcode: string; addressId: string }[];
    return list.length > 0 ? list : null;
  });
  const packet = packets[0];
  expect(String(packet?.addressId)).toBe("4101");
  await expect(admin.getByText(packet?.barcode ?? "missing").first()).toBeVisible();
  await expectAccessible(admin, "order with a label");

  await action("Mark shipped");
  const shipped = await mail(email, `Objednávka ${o.number} je na cestě`);
  expect(shipped.Text).toContain(`https://tracking.packeta.com/cs/?id=${packet?.barcode}`);

  // Packeta reports the parcel delivered; the tracking poll moves the order.
  const set = await fetch(`${MOCKS}/packeta/_packets/${packet?.id}/status`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ statusCode: 7 }),
  });
  expect(set.status).toBe(200);
  enqueueJob("shipping.track");
  await eventually(async () => (await order(page, CZ, token)).status === "delivered");
  await mail(email, `Objednávka ${o.number} byla doručena`);

  // A15: one of the two T-shirts back, with a credit note referencing the invoice.
  await admin.reload();
  await action("Refund");
  const dialog = admin.getByRole("dialog");
  await dialog
    .getByLabel(/Henley/)
    .first()
    .fill("1");
  await expect(dialog.getByText(/Total/)).toBeVisible();
  await dialog.getByRole("button", { name: "Refund", exact: true }).click();
  await expect(admin.getByText(/Credit note/).first()).toBeVisible();
  const credit = await eventually(() => invoiceOf(o.number, "credit_note"), 45_000);
  expect(credit.doc).toMatch(/^DB\d{9}$/);
  const refunded = await order(page, CZ, token);
  expect(refunded.payment.status).toBe("partially_refunded");
  await mail(email, `Vrácení peněz k objednávce ${o.number}`);
  await mailWithAttachments(email, `Opravný daňový doklad k objednávce ${o.number}`);
  await page.context().close();
});

test("COD with PPL: invoiced on dispatch, delivered by tracking, cash collected", async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const email = `wp12-cod-${run}@example.test`;
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, email);
  await page.getByRole("radio", { name: /PPL – kurýr/ }).check();
  await page.getByRole("radio", { name: /Dobírka/ }).check();
  await acceptAndPlace(page);
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
  const token = new URL(page.url()).pathname.slice(3);
  const o = await order(page, CZ, token);
  expect(o.status).toBe("confirmed");

  await openOrder(o.number);
  await action("Create label", true);
  const shipmentNumber = await eventually(() =>
    sql(
      `SELECT s.tracking_number FROM shipments s JOIN orders o ON o.id = s.order_id
       WHERE o.number = ${Number(o.number)} AND s.status = 'label_created'`,
    ),
  );
  expect(shipmentNumber).toMatch(/^\d{11}$/);
  // No invoice before dispatch (A17.2).
  expect(invoiceOf(o.number)).toBeNull();
  await action("Mark shipped");
  const inv = await eventually(() => invoiceOf(o.number), 45_000);
  const shippedOn = pragueDate(
    "s.shipped_at",
    "shipments s JOIN orders o ON o.id = s.order_id",
    `o.number = ${Number(o.number)}`,
  );
  expect(inv.duzp).toBe(shippedOn);
  await mail(email, `Objednávka ${o.number} je na cestě`);

  const phase = await fetch(`${MOCKS}/ppl/_shipments/${shipmentNumber}/phase`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ phase: "Delivered" }),
  });
  expect(phase.status).toBe(200);
  enqueueJob("shipping.track");
  await eventually(async () => (await order(page, CZ, token)).status === "delivered");

  // A16: delivered → collected (card, so no cash rounding) → the order is paid.
  await admin.reload();
  const cod = admin.locator("section", {
    has: admin.getByRole("heading", { name: "Cash on delivery", exact: true }),
  });
  await expect(cod.getByTestId("cod-state")).toHaveText("Delivered");
  await cod.getByLabel("Tender").selectOption("card");
  await cod.getByLabel("Collected by").selectOption("carrier");
  await cod.getByRole("button", { name: "Record collection" }).click();
  await expect(cod.getByTestId("cod-state")).toHaveText("Collected");
  expect((await order(page, CZ, token)).payment.status).toBe("paid");
  await page.context().close();
});

test("an EUR order of the CZ VAT payer is invoiced with the CZK recap at the ČNB rate", async ({
  browser,
}) => {
  const email = `wp12-eur-${run}@example.test`;
  const page = await newPage(browser, "sk-SK");
  await toCheckout(page, SK, "tricko-oversize");
  await fillContactAndAddress(page, email, "Bratislava");
  await page.getByRole("radio", { name: /Packeta – na adresu/ }).check();
  await page.getByRole("radio", { name: /Testovacia platba/ }).check();
  await acceptAndPlace(page, "Objednať s povinnosťou platby");
  const token = await fakePay(page, "Pay");
  const o = await order(page, SK, token);
  const inv = await eventually(() => invoiceOf(o.number), 45_000);
  expect(inv.currency).toBe("EUR");
  // The mock's fixing for the taxable supply date, in CZK per EUR × 1000.
  expect(Number(inv.rate)).toBeGreaterThan(20_000);
  await mailWithAttachments(email, `Faktúra k objednávke ${o.number}`);
  await page.context().close();
});
