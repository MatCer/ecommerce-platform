/**
 * WP24 AI theme editing against the seeded demo shop after `make theme-build`, with the fake
 * provider (no API key; spec §16 M3): a prompt → the scripted agent edits the home page and
 * writes a functional check → the real sandboxed build + gates (incl. that check) → the
 * revision cannot be published before the run is accepted → accept → publish → the storefront
 * shows the change → rolling back restores it.
 *
 * One full build runs for real: the spec takes a few minutes.
 */
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import { magicLink, root, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });
const owner = "owner@lnen.example";
const port = process.env.HTTP_PORT ?? "8080";
const shop = `http://demo.localhost:${port}`;
const DEMO = "(SELECT id FROM platform.tenants WHERE slug = 'demo')";
let context: BrowserContext;
let page: Page;

function sql(query: string): string[] {
  return execFileSync(
    "docker",
    ["compose", "exec", "-T", "postgres", "psql", "-U", "postgres", "-d", "app", "-tAc", query],
    { cwd: root, env: { ...process.env, COMPOSE_PROFILES: "full" }, encoding: "utf8" },
  )
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
}

test.beforeAll(async ({ browser }) => {
  context = await browser.newContext();
  page = await context.newPage();
  await useEnglish(page);
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await expect(page.getByRole("status")).toContainText(owner);
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
});

test.afterAll(async () => {
  await context?.close();
});

test("a prompt becomes a checked AI revision that is accepted, published and rolled back", async () => {
  test.setTimeout(15 * 60_000);
  const live = sql(
    `SELECT r.number FROM theme_active a JOIN theme_revisions r ON r.id = a.revision_id WHERE a.tenant_id = ${DEMO}`,
  )[0];
  const prompt = `Doprava zdarma od 1 500 Kč <b>{${randomBytes(3).toString("hex")}}</b>`;
  const note = `AI edit: ${prompt}`;

  await page.goto("/themes");
  const ai = page.getByRole("region", { name: /Edit with AI/ });
  await expect(ai.getByText("Demo AI")).toBeVisible();
  await ai.getByRole("textbox", { name: "What should change?" }).fill(prompt);
  await ai.getByRole("button", { name: "Start AI edit" }).click();

  // The run: tool steps, a real build + gates (the agent's functional check included).
  const run = ai.getByRole("article");
  await expect(run.getByText("Ready for review")).toBeVisible({ timeout: 12 * 60_000 });
  await expect(run.getByText("Ran the checks")).toBeVisible();
  await expect(run.getByText(/All checks passed on revision #\d+/)).toBeVisible();
  const diff = run.getByLabel("Changes");
  await expect(diff).toContainText("+++ b/src/pages/index.astro");
  await expect(diff).toContainText("data-ai-edit");
  await expect(diff).toContainText("+++ b/checks/ai-edit.spec.ts");
  const [number, functional] = sql(
    `SELECT r.number || '|' || (SELECT s->>'status' FROM jsonb_array_elements(r.checks->'steps') s
                                WHERE s->>'name' = 'functional')
     FROM ai_theme_runs a JOIN theme_revisions r ON r.id = a.revision_id
     WHERE a.tenant_id = ${DEMO} ORDER BY a.id DESC LIMIT 1`,
  )[0]?.split("|") ?? [];
  expect(functional).toBe("passed");

  // Not publishable before the run is accepted.
  const row = (n: number | string) =>
    page.getByRole("row").filter({ has: page.getByRole("button", { name: `#${n}`, exact: true }) });
  await row(number ?? "")
    .getByRole("button", { name: "Publish" })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Publish" }).click();
  await expect(page.getByText(/AI edit that was not accepted/)).toBeVisible();

  await run.getByRole("button", { name: "Accept" }).click();
  await expect(run.getByText(`Accepted. Revision #${number}`)).toBeVisible();
  const shopPage = await context.newPage();
  await shopPage.goto(shop);
  await expect(shopPage.locator("[data-ai-edit]")).toHaveCount(0);
  await row(number ?? "")
    .getByRole("button", { name: "Publish" })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Publish" }).click();
  await expect(page.getByText(`Revision #${number} is live.`)).toBeVisible();
  await shopPage.reload();
  await expect(shopPage.locator("[data-ai-edit]")).toHaveText(note);

  // Roll back to what was live before.
  await row(live ?? "")
    .getByRole("button", { name: "Roll back" })
    .click();
  await page.getByRole("dialog").getByRole("button", { name: "Roll back" }).click();
  await expect(page.getByText(`Revision #${live} is live.`)).toBeVisible();
  await shopPage.reload();
  await expect(shopPage.locator("[data-ai-edit]")).toHaveCount(0);
  const audit = sql(
    `SELECT string_agg(DISTINCT action, ',' ORDER BY action) FROM audit_log
     WHERE tenant_id = ${DEMO} AND action LIKE 'theme.ai_run_%'`,
  );
  expect(audit[0]).toBe("theme.ai_run_accepted,theme.ai_run_started");
});
