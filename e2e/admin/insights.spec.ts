import { randomUUID } from "node:crypto";
import { expect, type Page, test } from "@playwright/test";
import {
  enqueueJob,
  eventually,
  expectAccessible,
  screenshot,
  signInOwner,
  sql,
} from "./support.ts";

test.describe.configure({ mode: "serial" });

const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
const MARKET = `(SELECT id FROM markets WHERE tenant_id = ${DEMO} AND is_default)`;
const SESSION = "0197a8a0-0000-7000-8000-0000000e2e38";
const ANON = "e2e38e2e38e2e38e2e38e2e38e2e38e2";
const RECENT = "current_date - 3";
const BASELINE = "current_date - 12";
const BEFORE = "current_date - 27";
const AFTER = "current_date - 10";
const MARK = "e2e:insights";
const FUNNEL = [
  "sessions",
  "funnel.view_item",
  "funnel.add_to_cart",
  "funnel.begin_checkout",
  "funnel.purchase",
];

let weak = "";
let strong = "";
let weakName = "";
let runsBefore = "";

const day = (offset: number) =>
  new Intl.DateTimeFormat("en", { dateStyle: "medium", timeZone: "UTC" }).format(
    new Date(Date.now() - offset * 86_400_000),
  );

function cleanUp(): void {
  sql(`UPDATE ai_settings SET insights = false WHERE tenant_id = ${DEMO}`);
  sql(`UPDATE ai_theme_runs SET revision_id = NULL WHERE tenant_id = ${DEMO} AND created_by = '${MARK}';
       DELETE FROM theme_revisions WHERE tenant_id = ${DEMO} AND created_by = '${MARK}';
       DELETE FROM ai_theme_runs WHERE tenant_id = ${DEMO} AND created_by = '${MARK}'`);
  sql(`DELETE FROM insight_signals WHERE tenant_id = ${DEMO}
       AND (kind, subject) IN (('slow_page', 'blog:INP'), ('funnel_drop', 'view_item_to_add_to_cart'),
                               ('low_add_to_cart', '${weak}'), ('low_add_to_cart', '${strong}'))`);
  sql(`DELETE FROM daily_metrics WHERE tenant_id = ${DEMO} AND market_id = ${MARKET}
       AND ((date IN (${RECENT}, ${BASELINE}) AND dims = '{}'
             AND metric IN (${FUNNEL.map((m) => `'${m}'`).join(", ")}))
         OR (date IN (${RECENT}, ${BEFORE}, ${AFTER})
             AND dims->>'product_id' IN ('${weak}', '${strong}')))`);
  sql(`DELETE FROM events WHERE tenant_id = ${DEMO} AND session_id = '${SESSION}'`);
  sql(`DELETE FROM analytics_counters WHERE tenant_id = ${DEMO} AND market_id = ${MARKET}
       AND day = ${RECENT} AND template = 'blog'`);
}

function funnel(date: string, values: number[]): void {
  const rows = FUNNEL.map(
    (m, i) => `(${DEMO}, ${date}, ${MARKET}, '${m}', '{}'::jsonb, ${values[i]})`,
  );
  sql(`INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
       VALUES ${rows.join(", ")}`);
}

function productViews(product: string, views: number, adds: number, date = RECENT): void {
  sql(`INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
       SELECT ${DEMO}, ${date}, ${MARKET}, m, jsonb_build_object('product_id', '${product}'), v
       FROM (VALUES ('product.views', ${views}), ('product.add_to_carts', ${adds})) x(m, v)`);
}

async function setInsightsSwitch(admin: Page): Promise<void> {
  await admin.goto("/settings/ai");
  const card = admin.getByRole("region", { name: "AI on the shop" });
  const box = card.getByRole("checkbox", { name: "Insight hypotheses" });
  await expect(box).not.toBeChecked();
  await expect(
    card.getByText(/They appear after the next nightly run, not right away\./),
  ).toBeVisible();
  const put = admin.waitForRequest(
    (r) => r.method() === "PUT" && r.url().endsWith("/admin/v1/ai/settings"),
  );
  // Kobalte checkboxes: the visually hidden input is toggled through its label.
  await card.locator("label").filter({ hasText: "Insight hypotheses" }).click();
  expect((await put).postDataJSON(), "only the toggled switch is sent").toEqual({ insights: true });
  await expect(admin.getByText("Changes saved").last()).toBeVisible();
  await expect(box).toBeChecked();
  expect(sql(`SELECT insights FROM ai_settings WHERE tenant_id = ${DEMO}`)).toBe("t");
}

test.beforeAll(() => {
  [weak = "", strong = ""] = sql(
    `SELECT id FROM products WHERE tenant_id = ${DEMO} ORDER BY created_at, id LIMIT 2`,
  ).split("\n");
  weakName = sql(`SELECT name FROM product_translations
    WHERE tenant_id = ${DEMO} AND product_id = '${weak}'
    ORDER BY (locale = (SELECT default_locale FROM markets WHERE tenant_id = ${DEMO} AND is_default))
             DESC NULLS LAST, locale LIMIT 1`);
  cleanUp();
  funnel(RECENT, [20_000, 10_000, 600, 300, 150]);
  funnel(BASELINE, [60_000, 30_000, 3_000, 1_500, 750]);
  productViews(weak, 4_000, 40);
  productViews(strong, 6_000, 600);
  sql(`INSERT INTO events (id, tenant_id, at, type, anon_id, session_id, market_id, props)
       SELECT gen_random_uuid(), ${DEMO}, (${RECENT}) + time '12:00', 'web_vitals', '${ANON}',
              '${SESSION}', ${MARKET},
              jsonb_build_object('template', 'blog', 'name', 'INP', 'value', 400.0)
       FROM generate_series(1, 1000)`);
  sql(`INSERT INTO analytics_counters (tenant_id, market_id, day, template, requests)
       VALUES (${DEMO}, ${MARKET}, ${RECENT}, 'blog', 100000)`);
  runsBefore = sql(`SELECT count(*) FROM ai_theme_runs WHERE tenant_id = ${DEMO}`);
});

test.afterAll(() => {
  cleanUp();
});

test("detected insights are ranked, labelled, handed to the theme editor and dismissed", async ({
  browser,
}) => {
  test.setTimeout(180_000);
  const admin = await signInOwner(browser);
  await setInsightsSwitch(admin);

  const yesterday = new Date(Date.now() - 86_400_000).toISOString().slice(0, 10);
  const job = enqueueJob("insights.run", DEMO, { day: yesterday });
  await eventually(() => sql(`SELECT status FROM queue.jobs WHERE id = ${job}`) === "done", 90_000);

  await admin.setViewportSize({ width: 390, height: 844 });
  await admin.goto("/insights");
  await expect(admin.getByRole("heading", { level: 1, name: "Insights" })).toBeVisible();
  const list = admin.getByRole("list", { name: "Insights" });
  const slow = list.getByRole("region", { name: "Blog: INP" });
  const drop = list.getByRole("region", { name: "Product viewers who add to cart" });
  const product = list.getByRole("region", { name: weakName, exact: true });

  const headings = await list.getByRole("heading", { level: 2 }).allTextContents();
  expect(
    headings.filter((h) => ["Blog: INP", "Product viewers who add to cart", weakName].includes(h)),
  ).toEqual(["Blog: INP", "Product viewers who add to cart", weakName]);

  await expect(
    slow.getByText(
      "INP p75 is 400 ms, above the good limit of 200 ms (1,000 measurements in the last 14 days).",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(slow.getByText("Affected visits: ~2,500 a week", { exact: true })).toBeVisible();
  await expect(slow.getByText(`Detected since ${day(1)}`, { exact: true })).toBeVisible();
  await expect(
    drop.getByText(
      "Fell to 6% in the last 7 days, from 10% in the 21 days before (600 of 10,000 against 3,000 of 30,000).",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(drop.getByText("Affected visits: ~400 a week", { exact: true })).toBeVisible();
  await expect(
    product.getByText(
      "Added to cart on 1% of views in the last 14 days, against 6.4% across the shop (40 of 4,000 views).",
      { exact: true },
    ),
  ).toBeVisible();
  await expect(product.getByText("Affected visits: ~108 a week", { exact: true })).toBeVisible();
  await expect(product.getByRole("link", { name: weakName })).toHaveAttribute(
    "href",
    `/products/${weak}`,
  );

  const draft =
    "Make the blog template lighter with smaller images and fewer blocks above the fold.";
  const ai = slow.getByRole("region", { name: "Possible cause" });
  await expect(ai.getByText("AI-generated", { exact: true })).toBeVisible();
  await expect(
    ai.getByText("Demo AI: the blog pages are slow, INP p75 400.0 against a good bound of 200.0.", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(ai.locator("blockquote")).toHaveText(draft);
  await expect(ai.getByText("Model: fake", { exact: true })).toBeVisible();
  await expectAccessible(admin, "insights (phone)");
  await screenshot(admin, "insights-phone");

  await admin.setViewportSize({ width: 1280, height: 900 });
  await expect(slow).toBeVisible();
  await expectAccessible(admin, "insights (desktop)");
  await screenshot(admin, "insights-desktop");

  const starts: unknown[] = [];
  await admin.route("**/admin/v1/themes/ai-runs", (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    const body = route.request().postDataJSON() as { prompt: string };
    starts.push(body);
    const now = new Date().toISOString();
    return route.fulfill({
      status: 202,
      json: {
        id: randomUUID(),
        base_revision_id: randomUUID(),
        prompt: body.prompt,
        status: "queued",
        cancel_requested: false,
        turns: 0,
        checks_run: 0,
        tokens: 0,
        cost_micros: 0,
        created_by: "owner@lnen.example",
        created_at: now,
        updated_at: now,
      },
    });
  });
  const slowId = sql(`SELECT id FROM insight_signals WHERE tenant_id = ${DEMO}
                      AND kind = 'slow_page' AND subject = 'blog:INP'`);

  await slow.getByRole("button", { name: "Open in AI theme editor" }).click();
  await expect(admin).toHaveURL(/\/themes$/);
  const editor = admin.getByRole("region", { name: /Edit with AI/ });
  const prompt = editor.getByRole("textbox", { name: "What should change?" });
  await expect(prompt).toHaveValue(draft);
  await expect(prompt).toBeFocused();
  await editor.getByRole("button", { name: "Start AI edit" }).click();
  await expect(prompt).toHaveValue("");
  expect(starts, "the run carries the insight it came from").toEqual([
    { prompt: draft, insight_id: slowId },
  ]);

  await prompt.fill("Make the header sticky.");
  await editor.getByRole("button", { name: "Start AI edit" }).click();
  await expect(prompt).toHaveValue("");
  expect(starts[1], "a later manual run is not linked").toEqual({
    prompt: "Make the header sticky.",
  });
  await admin.reload();
  await expect(prompt, "a reload does not restore the insight's draft").toHaveValue("");
  expect(sql(`SELECT count(*) FROM ai_theme_runs WHERE tenant_id = ${DEMO}`)).toBe(runsBefore);

  await admin.goBack();
  await drop.getByRole("button", { name: "Dismiss" }).click();
  await expect(admin.getByText("Insight dismissed").last()).toBeVisible();
  await expect(drop).toHaveCount(0);
  await admin.reload();
  await expect(slow).toBeVisible();
  await expect(drop).toHaveCount(0);
  expect(
    sql(`SELECT status FROM insight_signals WHERE tenant_id = ${DEMO}
         AND kind = 'funnel_drop' AND subject = 'view_item_to_add_to_cart'`),
  ).toBe("dismissed");

  await admin.context().close();
});

test("a published theme change shows the weeks before and after", async ({ browser }) => {
  const signal = `(SELECT id FROM insight_signals WHERE tenant_id = ${DEMO}
                   AND kind = 'low_add_to_cart' AND subject = '${weak}')`;
  const seeded = `tenant_id = ${DEMO} AND created_by = '${MARK}'`;
  sql(`INSERT INTO ai_theme_runs (tenant_id, base_revision_id, prompt, status, created_by, insight_id)
       SELECT ${DEMO}, revision_id, 'Show the add to cart button above the fold.', 'accepted',
              '${MARK}', ${signal}
       FROM theme_active WHERE tenant_id = ${DEMO}`);
  sql(`INSERT INTO theme_revisions (tenant_id, number, artifact_id, origin, source_key, change,
                                    status, created_by, ai_run_id, published_at)
       SELECT ${DEMO}, (SELECT max(number) + 1 FROM theme_revisions WHERE tenant_id = ${DEMO}),
              r.artifact_id, 'custom', 'e2e/insights.tar.gz', 'ai', 'superseded', '${MARK}', run.id,
              (current_date - 20) + time '12:00'
       FROM theme_active a JOIN theme_revisions r ON r.id = a.revision_id
       JOIN ai_theme_runs run ON run.tenant_id = a.tenant_id AND run.created_by = '${MARK}'
       WHERE a.tenant_id = ${DEMO}`);
  sql(`UPDATE ai_theme_runs SET revision_id = (SELECT id FROM theme_revisions WHERE ${seeded})
       WHERE ${seeded}`);
  const number = sql(`SELECT number FROM theme_revisions WHERE ${seeded}`);
  productViews(weak, 2_000, 20, BEFORE);
  productViews(weak, 2_500, 75, AFTER);

  const admin = await signInOwner(browser);
  await admin.setViewportSize({ width: 390, height: 844 });
  await admin.goto("/insights");
  const product = admin
    .getByRole("list", { name: "Insights" })
    .getByRole("region", { name: weakName, exact: true });
  const change = product.getByRole("region", { name: "Before and after the theme change" });
  await expect(
    change.getByText(`Theme revision #${number} published on ${day(20)}`, { exact: true }),
  ).toBeVisible();
  await expect(change.getByRole("term")).toHaveText([
    `Before (${day(34)} – ${day(21)})`,
    `After (${day(19)} – ${day(6)})`,
  ]);
  await expect(change.getByRole("definition")).toHaveText([
    "1% of 2,000 views",
    "3% of 2,500 views",
  ]);
  await expect(product.getByText(`Detected since ${day(1)}`, { exact: true })).toBeVisible();
  await change.scrollIntoViewIfNeeded();
  await expectAccessible(admin, "insight readout (phone)");
  await screenshot(admin, "insights-readout-phone");
  await admin.setViewportSize({ width: 1280, height: 900 });
  await change.scrollIntoViewIfNeeded();
  await expectAccessible(admin, "insight readout (desktop)");
  await screenshot(admin, "insights-readout-desktop");

  sql(
    `UPDATE theme_revisions SET published_at = (current_date - 3) + time '12:00' WHERE ${seeded}`,
  );
  await admin.reload();
  await expect(change.getByRole("term").last()).toHaveText(`After (${day(2)} – ${day(-11)})`);
  await expect(change.getByRole("definition").last()).toHaveText(`Collecting until ${day(-11)}`);

  sql(`UPDATE insight_signals SET status = 'resolved' WHERE id = ${signal}`);
  await admin.reload();
  await expect(product.getByText("No longer detected", { exact: true })).toBeVisible();
  await expect(product.getByText(`Last detected on ${day(1)}`, { exact: true })).toBeVisible();
  await expect(product.getByText(/^Detected since/)).toHaveCount(0);
  await expect(change).toBeVisible();

  await admin.context().close();
});
