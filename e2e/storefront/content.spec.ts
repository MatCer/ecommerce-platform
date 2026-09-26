/**
 * Content on the seeded demo shop (WP13a): legal pages from the footer, CMS pages with blocks,
 * the blog, the footer menu, sitemaps listing pages, export feeds and llms.txt.
 * Needs `make up && make seed`.
 */
import { expect, type Page, test } from "@playwright/test";
import { CZ, decideConsent, expectAccessible, SK } from "./support";

const main = (page: Page) => page.getByRole("main");
const footer = (page: Page) => page.getByRole("contentinfo");

test("legal pages are linked from the footer and rendered from blocks", async ({
  page,
  context,
}) => {
  await decideConsent(context);
  await page.goto(`${CZ}/`);
  const legal = footer(page).getByRole("navigation", { name: "Právní informace" });
  await legal.getByRole("link", { name: "Obchodní podmínky" }).click();
  await expect(page).toHaveURL(`${CZ}/pages/obchodni-podminky`);
  await expect(page.getByRole("heading", { level: 1, name: "Obchodní podmínky" })).toBeVisible();
  await expect(main(page).getByText("Lnen & Co. s.r.o.").first()).toBeVisible();
  await expect(main(page).getByRole("heading", { level: 2 }).first()).toBeVisible();
  await expect(page.locator('link[rel="canonical"]')).toHaveAttribute(
    "href",
    `${CZ}/pages/obchodni-podminky`,
  );
  await expect(page.locator('link[hreflang="sk-SK"]')).toHaveAttribute(
    "href",
    `${SK}/pages/obchodne-podmienky`,
  );
  await expectAccessible(page, "legal page");
  // The cookie policy link of the consent banner points at the published cookies page.
  await expect(footer(page).getByRole("link", { name: "Cookies" })).toHaveAttribute(
    "href",
    "/pages/cookies",
  );
});

test("CMS page from the footer menu with a keyboard-operable FAQ", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/`);
  const service = footer(page).getByRole("navigation", { name: "Zákaznický servis" });
  await service.getByRole("link", { name: "Kontakt" }).click();
  await expect(page).toHaveURL(`${CZ}/pages/kontakt`);
  await expect(page.getByRole("heading", { level: 1, name: "Kontakt" })).toBeVisible();
  const question = main(page).getByText("Jak vrátit zboží?");
  const answer = main(page).getByText(/Do 14 dnů bez udání důvodu/);
  await expect(answer).toBeHidden();
  await question.focus();
  await page.keyboard.press("Enter");
  await expect(answer).toBeVisible();
  await expectAccessible(page, "cms page");

  await page.goto(`${CZ}/pages/doprava-a-platba`);
  await expect(
    main(page).getByRole("heading", { level: 2, name: "Způsoby dopravy" }),
  ).toBeVisible();
  const missing = await page.goto(`${CZ}/pages/neexistuje`);
  expect(missing?.status()).toBe(404);
});

test("blog index and post", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/blog`);
  await expect(page.getByRole("heading", { level: 1, name: "Blog" })).toBeVisible();
  await expectAccessible(page, "blog index");
  await main(page).getByRole("link", { name: "Jak vybrat správnou velikost trička" }).click();
  await expect(page).toHaveURL(`${CZ}/blog/jak-vybrat-velikost-tricka`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Jak vybrat správnou velikost trička" }),
  ).toBeVisible();
  await expect(main(page).getByRole("link", { name: /Všechna trička/ })).toHaveAttribute(
    "href",
    "/c/tricka",
  );
  const types = await page
    .locator('script[type="application/ld+json"]')
    .evaluateAll((els) => els.map((e) => JSON.parse(e.textContent ?? "{}")["@type"]));
  expect(types).toEqual(["BlogPosting", "BreadcrumbList"]);
  await expectAccessible(page, "blog post");
});

test("sitemaps, llms.txt and export feeds", async ({ request }) => {
  const sitemap = await (await request.get(`${CZ}/sitemap-1.xml`)).text();
  for (const path of [
    "/pages/obchodni-podminky",
    "/pages/kontakt",
    "/blog/jak-vybrat-velikost-tricka",
  ]) {
    expect(sitemap).toContain(`${CZ}${path}`);
  }
  const llms = await (await request.get(`${CZ}/llms.txt`)).text();
  expect(llms).toContain(`${CZ}/feeds/cz/google.xml`);

  // The seed queues a feed export; the worker writes it within seconds.
  await expect
    .poll(async () => (await request.get(`${CZ}/feeds/cz/google.xml`)).status(), {
      timeout: 60_000,
    })
    .toBe(200);
  const google = await (await request.get(`${CZ}/feeds/cz/google.xml`)).text();
  expect(google).toContain('xmlns:g="http://base.google.com/ns/1.0"');
  expect(google).toMatch(/<g:price>\d+\.\d{2} CZK<\/g:price>/);
  const heureka = await request.get(`${SK}/feeds/sk/heureka.xml`);
  expect(heureka.status()).toBe(200);
  expect(await heureka.text()).toMatch(/<PRICE_VAT>[\d.]+<\/PRICE_VAT>/);
  const zbozi = await (await request.get(`${CZ}/feeds/cz/zbozi.xml`)).text();
  expect(zbozi).toContain('xmlns="http://www.zbozi.cz/ns/offer/1.0"');
});
