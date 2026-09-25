/**
 * WP18 acceptance (storefront + mail): sign up in the theme's footer → double opt-in mail in
 * Mailpit → confirm on the checkout origin → subscribed with a consent record → a segment and
 * a campaign with a personalized products block (Admin API) → test send + real send → Mailpit
 * shows per-recipient products and RFC 8058 List-Unsubscribe headers → one-click unsubscribe
 * and the preference page work → the next campaign skips both → a simulated SES bounce
 * suppresses the address.
 *
 * Runs on the seeded demo shop (`make seed`). The segment only matches addresses that
 * subscribed during this test, so parallel suites and earlier runs are never mailed.
 * Recipient B's category affinity is inserted directly (the hourly WP17 rollup derives it from
 * orders; that path has its own tests).
 */
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, type Page, test } from "@playwright/test";
import { magicLink, mailpit, root, run, useEnglish } from "../admin/support.ts";
import { CZ, decideConsent, expectAccessible } from "./support.ts";

const port = new URL(CZ).port;
const API = `http://api.localhost:${port}`;

function env(name: string, fallback: string): string {
  if (process.env[name]) return process.env[name] ?? fallback;
  try {
    const line = readFileSync(join(root, ".env"), "utf8")
      .split("\n")
      .find((l) => l.startsWith(`${name}=`));
    if (line) return line.slice(name.length + 1).trim();
  } catch {
    // defaults
  }
  return fallback;
}

/** SQL as the database superuser (fixtures and assertions the public API does not expose). */
function sql(query: string): string {
  return execFileSync(
    "docker",
    ["compose", "exec", "-T", "postgres", "psql", "-U", "postgres", "-d", "app", "-At", "-c", query],
    { cwd: root, env: { ...process.env, COMPOSE_PROFILES: "full" }, encoding: "utf8" },
  ).trim();
}
const lit = (s: string) => `'${s.replace(/'/g, "''")}'`;

interface Summary {
  ID: string;
  Subject: string;
  Created: string;
}
interface Message {
  ID: string;
  MessageID: string;
  Subject: string;
  Text: string;
  HTML: string;
}

/** Messages to `to` whose subject matches, newest first (polls up to `ms`). */
async function mails(to: string, subject: RegExp, count = 1, ms = 60_000): Promise<Message[]> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const found = ((await res.json()) as { messages: Summary[] }).messages
      .filter((m) => subject.test(m.Subject))
      .sort((a, b) => b.Created.localeCompare(a.Created));
    if (found.length >= count) {
      return Promise.all(
        found.map(
          async (m) => (await (await fetch(`${mailpit}/api/v1/message/${m.ID}`)).json()) as Message,
        ),
      );
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`no mail to ${to} matching ${subject}`);
}

async function headers(id: string): Promise<Record<string, string[]>> {
  return (await (await fetch(`${mailpit}/api/v1/message/${id}/headers`)).json()) as Record<
    string,
    string[]
  >;
}

/** Product slugs a campaign mail links to, in order (tracked links carry the target in `u`). */
function productSlugs(html: string): string[] {
  const out: string[] = [];
  for (const m of html.matchAll(/newsletter\/click\?[^"]*?u=([^&"]+)/g)) {
    const target = decodeURIComponent(m[1] ?? "");
    const slug = /\/p\/([a-z0-9-]+)/.exec(target)?.[1];
    if (slug && !out.includes(slug)) out.push(slug);
  }
  return out;
}

/** Signs up through the footer form and confirms through the mailed link. */
async function subscribe(page: Page, email: string) {
  await page.goto(`${CZ}/`);
  const form = page.locator("form#newsletter");
  await form.getByLabel("E-mail").fill(email);
  await form.getByRole("button", { name: "Odebírat" }).click();
  await expect(page.getByText("Hotovo! Potvrďte prosím odběr v e-mailu.")).toBeVisible();
  const [confirm] = await mails(email, /^Potvrďte odběr novinek/);
  const link = /https?:\/\/[^\s"<>]+\/newsletter\/confirm\?token=[0-9a-f]{64}/.exec(
    confirm?.Text ?? "",
  )?.[0];
  expect(link, "confirmation link").toBeTruthy();
  await page.goto(link ?? "");
  await expect(page.getByRole("heading", { name: "Potvrzení odběru" })).toBeVisible();
  await expect(page.getByText(/Potvrďte, že chcete dostávat novinky na adresu .\*\*\*@example\.com/)).toBeVisible();
  await expectAccessible(page, "newsletter confirmation");
  await page.getByRole("button", { name: "Potvrdit odběr" }).click();
  await expect(page.getByText("Děkujeme, odběr je potvrzený.")).toBeVisible();
}

test("newsletter: double opt-in, personalized campaign, unsubscribe, bounce", async ({
  browser,
  request,
}) => {
  test.setTimeout(240_000);
  const started = new Date(Date.now() - 5_000).toISOString();
  const a = `nl-a-${run}@example.com`;
  const b = `nl-b-${run}@example.com`;
  const shopper = await browser.newContext({ locale: "cs-CZ" });
  await decideConsent(shopper);
  const page = await shopper.newPage();
  await subscribe(page, a);
  await subscribe(page, b);

  // Consent evidence (A20): the confirmation is recorded for the address.
  expect(
    sql(
      `SELECT granted || ',' || source FROM consent_records WHERE subject_type = 'email'
       AND subject_id = ${lit(a)} AND purpose = 'email_marketing' ORDER BY at DESC LIMIT 1`,
    ),
  ).toBe("true,double_opt_in");

  // Staff: an Admin API token of the demo owner.
  const staff = await browser.newContext({ locale: "en-US" });
  const admin = await staff.newPage();
  await useEnglish(admin);
  const since = new Date(Date.now() - 1000);
  await admin.goto(`http://admin.localhost:${port}/login`);
  await admin.getByLabel("Email").fill("owner@lnen.example");
  await admin.getByRole("button", { name: "Email me a sign-in link" }).click();
  await admin.goto(await magicLink("owner@lnen.example", since));
  await expect(admin.getByRole("heading", { name: "Overview" })).toBeVisible();
  const token = await admin.evaluate(async () => {
    const res = await fetch("/api/auth/token", { credentials: "same-origin" });
    return ((await res.json()) as { token: string }).token;
  });
  const tenant = await admin.evaluate(() => localStorage.getItem("admin.tenant") ?? "");
  const h = { authorization: `Bearer ${token}`, "x-tenant-id": tenant };
  const subscriber = async (email: string) =>
    (
      (await (
        await request.get(`${API}/admin/v1/subscribers?q=${encodeURIComponent(email)}`, {
          headers: h,
        })
      ).json()) as { items: { id: string; status: string; customer_id: string | null }[] }
    ).items[0];
  expect((await subscriber(a))?.status).toBe("subscribed");

  const segment = await request.post(`${API}/admin/v1/segments`, {
    headers: h,
    data: {
      name: `E2E ${run}`,
      rules: {
        match: "all",
        conditions: [
          { field: "locale", locales: ["cs"] },
          { field: "subscribed", after: started },
        ],
      },
    },
  });
  expect(segment.status()).toBe(201);
  const segmentId = ((await segment.json()) as { id: string }).id;
  const preview = (await (
    await request.post(`${API}/admin/v1/segments/preview`, {
      headers: h,
      data: { match: "all", conditions: [{ field: "subscribed", after: started }] },
    })
  ).json()) as { count: number; sample: { email: string }[] };
  expect(preview.sample.map((s) => s.email).sort()).toEqual([a, b].sort());

  const content = (subject: string) => ({
    cs: {
      subject,
      preheader: "Podzimní výběr",
      blocks: [
        { type: "heading", text: "Novinky na podzim" },
        { type: "text", html: "<p>Mrkněte na <a href=\"/\">celý obchod</a>.</p>" },
        { type: "personalized_products", title: "Vybrali jsme pro vás", limit: 2 },
        { type: "button", label: "Do obchodu", href: "/" },
      ],
    },
  });
  const create = async (name: string, subject: string) => {
    const res = await request.post(`${API}/admin/v1/campaigns`, {
      headers: h,
      data: { name, segment_id: segmentId, content: content(subject) },
    });
    expect(res.status()).toBe(201);
    return ((await res.json()) as { id: string }).id;
  };
  const subject = `Podzim ${run}`;
  const campaign = await create(`Podzim ${run}`, subject);

  // Test send: marked, untracked, to staff only.
  const tester = `nl-test-${run}@example.com`;
  const sent = await request.post(`${API}/admin/v1/campaigns/${campaign}/test`, {
    headers: h,
    data: { emails: [tester] },
  });
  expect(sent.status()).toBe(200);
  const [testMail] = await mails(tester, new RegExp(`^\\[TEST\\] ${subject}$`));
  expect(testMail?.HTML).toContain("Vybrali jsme pro vás");
  const fallback = [...(testMail?.HTML ?? "").matchAll(/\/p\/([a-z0-9-]+)/g)].map((m) => m[1]);
  expect(fallback.length).toBeGreaterThan(0);

  // B is a customer granting personalization, with an interest in a category the fallback's
  // first product is not in: B's products must come from that category, A's stay the fallback.
  const category = sql(
    `SELECT pc.category_id FROM product_categories pc
     JOIN products p ON p.id = pc.product_id AND p.status = 'active'
     WHERE pc.tenant_id = ${lit(tenant)} AND pc.category_id NOT IN (
       SELECT pc2.category_id FROM product_categories pc2
       JOIN product_translations t ON t.product_id = pc2.product_id
       WHERE t.tenant_id = ${lit(tenant)} AND t.slug = ${lit(fallback[0] ?? "")})
     GROUP BY 1 HAVING count(*) >= 3 ORDER BY count(*) DESC LIMIT 1`,
  );
  expect(category).toMatch(/^[0-9a-f-]{36}$/);
  sql(
    `WITH c AS (INSERT INTO customers (tenant_id, email, locale, email_verified_at)
                VALUES (${lit(tenant)}, ${lit(b)}, 'cs', now()) RETURNING id),
          s AS (UPDATE subscribers SET customer_id = (SELECT id FROM c)
                WHERE tenant_id = ${lit(tenant)} AND email = ${lit(b)} RETURNING 1),
          k AS (INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose, granted,
                                             text_version, source)
                SELECT ${lit(tenant)}, 'customer', id::text, 'personalization', true, '2026-09-25',
                       'preferences' FROM c RETURNING 1)
     INSERT INTO customer_affinity (tenant_id, customer_id, dim, key, score)
     SELECT ${lit(tenant)}, id, 'category', ${lit(category)}, 10 FROM c`,
  );
  const inCategory = sql(
    `SELECT string_agg(t.slug, ',') FROM product_translations t
     JOIN product_categories pc ON pc.product_id = t.product_id
     WHERE t.tenant_id = ${lit(tenant)} AND pc.category_id = ${lit(category)}`,
  ).split(",");

  // The real send: batches via the worker.
  const scheduled = await request.post(`${API}/admin/v1/campaigns/${campaign}/schedule`, {
    headers: h,
    data: { at: null },
  });
  expect(scheduled.status()).toBe(200);
  const [mailA] = await mails(a, new RegExp(`^${subject}$`));
  const [mailB] = await mails(b, new RegExp(`^${subject}$`));
  const slugsA = productSlugs(mailA?.HTML ?? "");
  const slugsB = productSlugs(mailB?.HTML ?? "");
  expect(slugsA.length).toBeGreaterThan(0);
  expect(slugsB.length).toBeGreaterThan(0);
  expect(slugsB.every((s) => inCategory.includes(s)), `${slugsB} in ${category}`).toBe(true);
  expect(slugsA).not.toEqual(slugsB);
  expect(mailA?.HTML).toContain("/_p/newsletter/click?t=");
  for (const m of [mailA, mailB]) {
    const hdr = await headers(m?.ID ?? "");
    expect(hdr["List-Unsubscribe-Post"]).toEqual(["List-Unsubscribe=One-Click"]);
    expect(hdr["List-Unsubscribe"]?.[0]).toMatch(
      /^<http:\/\/checkout\.demo\.localhost:\d+\/_p\/newsletter\/unsubscribe\?t=[0-9a-f]{64}>$/,
    );
  }

  // A tracked link lands on the shop.
  const click = /href="(http[^"]+\/_p\/newsletter\/click\?[^"]+)"/.exec(mailA?.HTML ?? "")?.[1];
  await page.goto((click ?? "").replace(/&amp;/g, "&"));
  expect(new URL(page.url()).host).toBe(new URL(CZ).host);

  // A: one-click unsubscribe as a mailbox provider does it (RFC 8058).
  const oneClick = (await headers(mailA?.ID ?? ""))["List-Unsubscribe"]?.[0]?.slice(1, -1) ?? "";
  const res = await request.post(oneClick, {
    headers: { "content-type": "application/x-www-form-urlencoded" },
    data: "List-Unsubscribe=One-Click",
  });
  expect(res.status()).toBe(200);
  expect((await subscriber(a))?.status).toBe("unsubscribed");

  // B: the preference page linked in the footer.
  const prefs = /href="(http[^"]+\/newsletter\?t=[0-9a-f]{64})"/.exec(mailB?.HTML ?? "")?.[1];
  await page.goto(prefs ?? "");
  await expect(page.getByRole("heading", { name: "Odběr novinek" })).toBeVisible();
  await expect(page.getByText(/odebírá naše novinky/)).toBeVisible();
  await expectAccessible(page, "newsletter preferences");
  await page.getByRole("button", { name: "Odhlásit odběr" }).click();
  await expect(page.getByText("Odběr je zrušený.", { exact: false })).toBeVisible();
  expect((await subscriber(b))?.status).toBe("unsubscribed");

  // The next campaign reaches nobody from this test.
  const next = await create(`Zima ${run}`, `Zima ${run}`);
  await request.post(`${API}/admin/v1/campaigns/${next}/schedule`, { headers: h, data: {} });
  await expect
    .poll(
      async () =>
        (
          (await (
            await request.get(`${API}/admin/v1/campaigns/${next}`, { headers: h })
          ).json()) as { status: string }
        ).status,
      { timeout: 60_000 },
    )
    .toBe("sent");
  const stats = (
    (await (await request.get(`${API}/admin/v1/campaigns/${next}`, { headers: h })).json()) as {
      stats: { sent: number };
    }
  ).stats;
  expect(stats.sent).toBe(0);

  // A simulated SES bounce (SNS envelope) for A's campaign message suppresses the address.
  const messageId = mailA?.MessageID ?? "";
  const notification = {
    notificationType: "Bounce",
    bounce: { bounceType: "Permanent", bouncedRecipients: [{ emailAddress: a }] },
    mail: { commonHeaders: { messageId: `<${messageId}>` } },
  };
  const secret = env("MAIL_EVENTS_SECRET", "local-mail-events-secret-0123456789abcdef");
  const bounce = await request.post(`${API}/webhooks/ses`, {
    headers: {
      authorization: `Basic ${Buffer.from(`ses:${secret}`).toString("base64")}`,
      "content-type": "text/plain",
    },
    data: JSON.stringify({
      Type: "Notification",
      MessageId: `e2e-${run}`,
      TopicArn: "arn:aws:sns:eu-central-1:000000000000:ses",
      Message: JSON.stringify(notification),
    }),
  });
  expect(bounce.status()).toBe(200);
  expect(((await bounce.json()) as { applied: boolean }).applied).toBe(true);
  const suppressed = (await (
    await request.get(`${API}/admin/v1/email-suppressions?q=${encodeURIComponent(a)}`, {
      headers: h,
    })
  ).json()) as { items: { email: string; reason: string }[] };
  expect(suppressed.items).toEqual([expect.objectContaining({ email: a, reason: "bounce" })]);
  expect((await subscriber(a))?.status).toBe("bounced");

  await shopper.close();
  await staff.close();
});
