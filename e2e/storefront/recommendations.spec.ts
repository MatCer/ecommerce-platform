/**
 * WP17 acceptance: recommendations on the seeded demo shop (`make seed` adds 90 days of order
 * history and runs the backfill rollup). Bought together on the product page, bestsellers on
 * the home page for everyone, personal picks only with the server-side `personalization`
 * consent (A20), the cart drawer cross-sell, recently viewed with live prices, and private
 * answers never cached (A2). Needs `make up && make seed && make theme-build`.
 */
import { expect, type Page, test } from "@playwright/test";
import { CZ, decideConsent, expectAccessible, grantConsent, hydrated } from "./support";

const main = (page: Page) => page.getByRole("main");

type Recs = { strategy: string | null; products: { slug: string; name: string }[] };

test("product page: frequently bought together from the order history", async ({
  page,
  context,
}) => {
  await decideConsent(context);
  await page.goto(`${CZ}/p/tricko-basic`);
  const slot = main(page).getByRole("region", { name: "Často kupováno společně" });
  await expect(slot.getByRole("link", { name: "Kšiltovka Classic" })).toBeVisible();
  await expect(slot.getByRole("link", { name: "Tričko Basic", exact: true })).toHaveCount(0);
  await expectAccessible(page, "product with bought together");
});

test("home: bestsellers for everyone, no personal picks without consent", async ({
  page,
  context,
}) => {
  await decideConsent(context);
  await page.goto(`${CZ}/`);
  await hydrated(page);
  const bestsellers = page.getByRole("region", { name: "Nejprodávanější" });
  await expect(bestsellers.getByRole("article").first()).toBeVisible();
  await expect(page.getByRole("region", { name: "Vybrali jsme pro vás" })).toHaveCount(0);

  // Even a claimed consent cookie is not enough: the server has no record (A20).
  await context.addCookies([{ name: "consent", value: "analytics,personalization", url: CZ }]);
  const res = await page.request.get(`${CZ}/_p/recommendations?context=home&limit=8`);
  expect(res.headers()["cache-control"]).toBe("private, no-store");
  expect(((await res.json()) as Recs).strategy).not.toBe("personalized");
});

test("home: personal picks from the visitor's own browsing with personalization consent", async ({
  page,
}) => {
  await grantConsent(page, CZ, ["analytics", "personalization"]);
  // Browse caps: the consented beacon records the product views.
  for (const slug of ["cepice-merino", "kulich-rib", "cepice-fisherman"]) {
    await page.goto(`${CZ}/p/${slug}`);
    await hydrated(page);
  }
  await page.goto(`${CZ}/`);
  const forYou = page.getByRole("region", { name: "Vybrali jsme pro vás" });
  await expect(forYou.getByRole("link").first()).toBeVisible({ timeout: 15_000 });
  const res = await page.request.get(`${CZ}/_p/recommendations?context=home&limit=8`);
  const recs = (await res.json()) as Recs;
  expect(recs.strategy).toBe("personalized");
  // Caps first: the affinity comes from the category the visitor browsed.
  const caps = recs.products.filter((p) => /Čepice|Kulich|Kšiltovka|Klobouk|Čelenka/.test(p.name));
  expect(caps.length).toBeGreaterThanOrEqual(3);
  expect(recs.products[0]?.name).toMatch(/Čepice|Kulich|Kšiltovka|Klobouk|Čelenka/);
  await expectAccessible(page, "home with personal picks");

  // The public route never personalizes, whatever cookies come along.
  const pub = await page.request.get(`${CZ}/_p/public/recommendations?context=home`);
  expect(((await pub.json()) as Recs).strategy).not.toBe("personalized");
});

test("cart drawer: cross-sell for the cart's products", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/p/tricko-basic`);
  await hydrated(page);
  await main(page).getByRole("button", { name: "Přidat do košíku" }).first().click();
  const drawer = page.getByRole("dialog", { name: /Košík/ });
  await expect(drawer).toBeVisible();
  const crossSell = drawer.getByRole("region", { name: "Mohlo by se vám hodit" });
  await expect(crossSell.getByRole("link", { name: /Kšiltovka Classic/ })).toBeVisible();
  // Never the product already in the cart.
  await expect(crossSell.getByRole("link", { name: /^Tričko Basic/ })).toHaveCount(0);
  await expectAccessible(page, "cart drawer with cross-sell");
});

test("recently viewed: live prices, only with the server-side consent", async ({ page }) => {
  await grantConsent(page, CZ, ["personalization"]);
  await page.goto(`${CZ}/p/mikina-fleece`);
  await hydrated(page);
  await expect
    .poll(() => page.evaluate(() => localStorage.getItem("sf:personalization:recent")))
    .toMatch(/[0-9a-f-]{36}/);
  await page.goto(`${CZ}/p/cepice-merino`);
  const recent = page.getByRole("region", { name: "Naposledy prohlížené" });
  await expect(recent.getByRole("link", { name: /Mikina Fleece/ })).toBeVisible();
  await expect(recent.getByText(/Kč/).first()).toBeVisible();

  // Withdrawn on the server: the ids alone rehydrate nothing.
  await grantConsent(page, CZ, []);
  const ids = await page.evaluate(() => localStorage.getItem("sf:personalization:recent"));
  const res = await page.request.get(
    `${CZ}/_p/recommendations?context=recent&ids=${JSON.parse(ids ?? "[]").join(",")}`,
  );
  expect(((await res.json()) as Recs).products).toEqual([]);
});
