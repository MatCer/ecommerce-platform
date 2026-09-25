/**
 * Default theme on the seeded demo shop (WP8): browsing CZ + SK (+ the /cs locale prefix),
 * filters, search, variants, cart, checkout handoff, keyboard-only use, consent and axe.
 * Needs `make up && make seed`.
 */

import { expect, type Page, test } from "@playwright/test";
import { testContext } from "../rate-client";
import { CZ, decideConsent, expectAccessible, hydrated, SK, screenshot } from "./support";

const main = (page: Page) => page.getByRole("main");

test.describe("browsing", () => {
  test("CZ: home → category → subcategory → product, with SEO data", async ({ page, context }) => {
    await decideConsent(context);
    await page.goto(`${CZ}/`);
    await expect(page.locator("html")).toHaveAttribute("lang", "cs");
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expectAccessible(page, "home");
    // Footer marks name the methods checkout offers in this market.
    const footer = page.getByRole("contentinfo");
    await expect(
      footer
        .getByRole("list", { name: "Platba" })
        .getByRole("listitem")
        .filter({ hasText: /^Dobírka$/ }),
    ).toBeVisible();
    await expect(
      footer.getByRole("list", { name: "Doprava" }).getByText("Osobní odběr – Praha"),
    ).toBeVisible();

    await page
      .getByRole("navigation", { name: "Kategorie" })
      .getByRole("link", { name: /Oblečení/ })
      .click();
    await expect(page).toHaveURL(`${CZ}/c/obleceni`);
    await expect(page.getByRole("heading", { level: 1, name: "Oblečení" })).toBeVisible();
    await expect(page.locator('link[rel="canonical"]')).toHaveAttribute("href", `${CZ}/c/obleceni`);
    await expect(page.locator('link[hreflang="sk-SK"]')).toHaveAttribute(
      "href",
      `${SK}/c/oblecenie`,
    );
    await expectAccessible(page, "category");

    await main(page).getByRole("link", { name: "Mikiny", exact: true }).click();
    await expect(page.getByRole("heading", { level: 1, name: "Mikiny" })).toBeVisible();
    await main(page).getByRole("link", { name: "Mikina Fleece" }).click();
    await expect(page).toHaveURL(`${CZ}/p/mikina-fleece`);
    await expect(page.getByRole("heading", { level: 1, name: "Mikina Fleece" })).toBeVisible();
    const types = await page
      .locator('script[type="application/ld+json"]')
      .evaluateAll((els) => els.map((e) => JSON.parse(e.textContent ?? "{}")["@type"]));
    expect(types).toEqual(["Product", "BreadcrumbList"]);
    // Omnibus (A18): the reduction is shown with the 30-day lowest price.
    await expect(main(page).getByText(/Nejnižší cena za 30 dní před slevou/)).toBeVisible();
    await expect(main(page).getByText("Informace o bezpečnosti produktu").first()).toBeVisible();
    await expectAccessible(page, "product");
  });

  test("SK: Slovak strings and euro prices; /cs renders the same market in Czech", async ({
    page,
    context,
  }) => {
    await decideConsent(context, SK);
    await page.goto(`${SK}/c/mikiny`);
    await expect(page.locator("html")).toHaveAttribute("lang", "sk");
    await expect(main(page).getByText(/€/).first()).toBeVisible();
    await main(page).getByRole("link", { name: "Mikina Fleece" }).click();
    await expect(page.getByRole("button", { name: "Pridať do košíka" })).toBeVisible();

    await page.goto(`${SK}/cs`);
    await expect(page.locator("html")).toHaveAttribute("lang", "cs");
    await expect(page.locator('link[rel="canonical"]')).toHaveAttribute("href", `${SK}/cs`);
    // Links the page model and the theme build carry the prefix; prices stay in euro.
    await page
      .getByRole("navigation", { name: "Kategorie" })
      .getByRole("link", { name: /Oblečení/ })
      .click();
    await expect(page).toHaveURL(`${SK}/cs/c/obleceni`);
    await main(page).getByRole("link", { name: "Mikina Fleece" }).first().click();
    await expect(page).toHaveURL(`${SK}/cs/p/mikina-fleece`);
    await expect(page.getByRole("button", { name: "Přidat do košíku" })).toBeVisible();
    await expect(main(page).getByText(/€/).first()).toBeVisible();
    // The footer switches to the same page in Slovak.
    await page.getByRole("contentinfo").getByRole("link", { name: "Slovenčina" }).click();
    await expect(page).toHaveURL(`${SK}/p/mikina-fleece`);
  });
});

test("filters are a GET form: facet, active chip, noindex, removal", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/c/obleceni`);
  await hydrated(page);
  const size = page.getByRole("group", { name: "Velikost" });
  await page.locator("summary", { hasText: "Velikost" }).click();
  await size.getByText("M", { exact: true }).click();
  await expect(page).toHaveURL(/f\.opt\.velikost=m/);
  await expect(page.locator('meta[name="robots"]')).toHaveAttribute("content", "noindex,follow");
  await expect(page.getByRole("link", { name: "Odebrat filtr: Velikost M" })).toBeVisible();
  // Zero-match values are disabled, never hidden (A23).
  await page.locator("summary", { hasText: "Barva" }).click();
  const colours = page.getByRole("group", { name: "Barva" }).getByRole("checkbox");
  expect(await colours.count()).toBeGreaterThan(1);
  await page.keyboard.press("Escape");
  await page.getByRole("link", { name: "Odebrat filtr: Velikost M" }).click();
  await expect(page).toHaveURL(`${CZ}/c/obleceni`);
  await expect(page.locator('meta[name="robots"]')).toHaveCount(0);
});

test("filters work without JavaScript", async ({ browser }) => {
  const ctx = await testContext(browser, { javaScriptEnabled: false });
  const page = await ctx.newPage();
  await page.goto(`${CZ}/c/obleceni`);
  await page.locator("summary", { hasText: "Materiál" }).click();
  await page.getByRole("group", { name: "Materiál" }).getByText("Len", { exact: true }).click();
  await page
    .getByRole("group", { name: "Materiál" })
    .getByRole("button", { name: "Použít" })
    .click();
  await expect(page).toHaveURL(/f\.param\.material=/);
  await expect(page.getByRole("link", { name: /Odebrat filtr: Materiál Len/ })).toBeVisible();
  await ctx.close();
});

test.describe("search", () => {
  for (const q of ["čepice", "cepice"]) {
    test(`results for "${q}" (diacritics-insensitive)`, async ({ page, context }) => {
      await decideConsent(context);
      await page.goto(`${CZ}/search?${new URLSearchParams({ q })}`);
      await expect(page.locator('meta[name="robots"]')).toHaveAttribute(
        "content",
        "noindex,follow",
      );
      await expect(main(page).getByRole("link", { name: "Čepice Merino" })).toBeVisible();
    });
  }

  test("typeahead combobox: suggestions, arrow keys, Enter opens the product", async ({
    page,
    context,
  }) => {
    await decideConsent(context);
    await page.goto(`${CZ}/`);
    await hydrated(page);
    const box = page.getByRole("combobox", { name: "Hledat" });
    await box.fill("cepice mer");
    const list = page.getByRole("listbox");
    await expect(list.getByRole("option", { name: /Čepice Merino/ })).toBeVisible();
    await expect(box).toHaveAttribute("aria-expanded", "true");
    let option = "";
    for (let i = 0; i < 6 && !option.includes("Čepice Merino"); i++) {
      await box.press("ArrowDown");
      const id = await box.getAttribute("aria-activedescendant");
      option = (await page.locator(`#${id}`).textContent()) ?? "";
    }
    await box.press("Enter");
    await expect(page).toHaveURL(`${CZ}/p/cepice-merino`);
  });

  test("no results: message and ways back", async ({ page, context }) => {
    await decideConsent(context);
    await page.goto(`${CZ}/search?q=xyzzyqq`);
    await expect(main(page).getByText("Nic jsme nenašli.")).toBeVisible();
    await expect(main(page).getByRole("link", { name: "Oblečení" })).toBeVisible();
  });
});

test("variant selection, add to cart, cart drawer, checkout origin", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/p/mikina-fleece`);
  await hydrated(page);
  // The radios are visually hidden inside their chip labels: click the chips like a user.
  await page.locator("label", { hasText: "Cihlová" }).click();
  await page.locator("label", { hasText: /^L$/ }).click();
  await expect(page.getByRole("radio", { name: "L", exact: true })).toBeChecked();
  await expect(main(page).getByText("Kód: LN-025-CIHLOVA-L")).toBeVisible();
  await main(page).getByRole("button", { name: "Přidat do košíku" }).first().click();
  const drawer = page.getByRole("dialog", { name: /Košík/ });
  await expect(drawer).toBeVisible();
  await expect(drawer.getByRole("link", { name: "Mikina Fleece" })).toBeVisible();
  await expect(drawer.getByText(/Cihlová/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Košík, položek: 1" })).toBeAttached();
  await drawer.getByRole("button", { name: "Přidat" }).click();
  await expect(drawer.getByText("2 položky")).toBeVisible();
  await screenshot(page, "cart-drawer-desktop");
  await drawer.getByRole("button", { name: "K pokladně" }).click();
  await expect(page).toHaveURL(/^http:\/\/checkout\.demo\.localhost(:\d+)?\//);
});

test("keyboard only: skip link, header, product page, cart", async ({ page, context }) => {
  await decideConsent(context);
  await page.goto(`${CZ}/p/mikina-fleece`);
  await hydrated(page);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Přeskočit na obsah" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("#main")).toBeFocused();

  // Variant radios: Tab into the group, arrows move the choice.
  const popelava = page.getByRole("radio", { name: "Popelavá" });
  await popelava.focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: "Cihlová" })).toBeChecked();
  const add = main(page).getByRole("button", { name: "Přidat do košíku" }).first();
  await add.focus();
  await page.keyboard.press("Enter");
  const drawer = page.getByRole("dialog", { name: /Košík/ });
  await expect(drawer).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  // Header: the cart button opens the drawer and focus returns to it on Escape.
  const cartButton = page.getByRole("button", { name: /Košík, položek/ });
  await cartButton.focus();
  await page.keyboard.press("Enter");
  await expect(drawer).toBeVisible();
  await expect(drawer.getByRole("button", { name: "Zavřít košík" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(cartButton).toBeFocused();

  // Search by keyboard.
  await page.getByRole("combobox", { name: "Hledat" }).focus();
  await page.keyboard.type("mikina");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(`${CZ}/search?q=mikina`);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("mikina");
});

test("phone sheets (menu, filters): Escape closes, focus never stays behind an open sheet", async ({
  browser,
}) => {
  const ctx = await testContext(browser, { viewport: { width: 390, height: 844 } });
  await decideConsent(ctx);
  const page = await ctx.newPage();
  await page.goto(`${CZ}/c/obleceni`);
  await hydrated(page);
  for (const [trigger, id] of [
    ["Menu", "#nav-drawer"],
    ["Filtry", "#filter-sheet"],
  ] as const) {
    const button = page.getByRole("button", { name: trigger, exact: true });
    const sheet = page.locator(id);
    await button.click();
    await expect(sheet).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(sheet).toBeHidden();
    await expect(button).toBeFocused();
    await button.click();
    await expect(sheet).toBeVisible();
    for (let i = 0; i < 60 && (await sheet.isVisible()); i++) await page.keyboard.press("Tab");
    await expect(sheet).toBeHidden();
  }
  await ctx.close();
});

test("RUM: a consented, sampled visit beacons its Web Vitals on leave; no consent, no beacon", async ({
  browser,
}) => {
  for (const consent of ["analytics", ""]) {
    const ctx = await testContext(browser);
    await decideConsent(ctx, CZ, consent);
    await ctx.addInitScript(() => {
      Math.random = () => 0; // inside the 10 % sample
    });
    const beacons: string[] = [];
    ctx.on("request", (r) => {
      if (r.url().endsWith("/_p/e")) beacons.push(r.postData() ?? "");
    });
    const page = await ctx.newPage();
    await page.goto(`${CZ}/p/mikina-fleece`);
    await hydrated(page);
    await page.waitForTimeout(500); // the reporter loads lazily after hydration
    // Leaving the page (pagehide) sends one beacon; Playwright does not observe beacons sent
    // while a document unloads, so the event is dispatched in place.
    await page.evaluate(() => dispatchEvent(new Event("pagehide")));
    await page.waitForTimeout(500);
    const vitals = beacons
      .flatMap(
        (b) =>
          (JSON.parse(b) as { events: { type: string; name: string; template: string }[] }).events,
      )
      .filter((e) => e.type === "web_vital");
    if (consent) {
      expect(vitals.map((v) => v.name)).toContain("LCP");
      expect(vitals.map((v) => v.name)).toContain("CLS");
      expect(vitals[0]?.template).toBe("product");
    } else expect(beacons).toEqual([]);
    await ctx.close();
  }
});

test.describe("consent (A20)", () => {
  test("nothing is stored before a choice; reject and reopen from the footer", async ({
    page,
    context,
  }) => {
    await page.goto(`${CZ}/p/mikina-fleece`);
    await hydrated(page);
    const banner = page.getByRole("region", { name: "Souhlas s cookies" });
    await expect(banner).toBeVisible();
    await expectAccessible(page, "product with consent banner");
    expect((await context.cookies()).map((c) => c.name)).not.toContain("consent");
    expect(await page.evaluate(() => localStorage.length)).toBe(0);

    await banner.getByRole("button", { name: "Odmítnout" }).click();
    await expect(banner).toBeHidden();
    const cookie = (await context.cookies()).find((c) => c.name === "consent");
    expect(cookie?.value).toBe("");
    expect(await page.evaluate(() => localStorage.length)).toBe(0);

    await page.getByRole("button", { name: "Nastavení cookies" }).click();
    await expect(banner).toBeVisible();
    await banner.getByRole("checkbox", { name: "Personalizace" }).check();
    await banner.getByRole("button", { name: "Uložit výběr" }).click();
    await expect(banner).toBeHidden();
    await page.reload();
    // Personalization granted: this visit is remembered for "recently viewed" (its id).
    await expect
      .poll(() => page.evaluate(() => localStorage.getItem("sf:personalization:recent")))
      .toMatch(/[0-9a-f-]{36}/);
  });

  test("recently viewed appears only with personalization consent", async ({ page, context }) => {
    await decideConsent(context, CZ, "personalization");
    await page.goto(`${CZ}/p/mikina-fleece`);
    await hydrated(page);
    await expect
      .poll(() => page.evaluate(() => localStorage.getItem("sf:personalization:recent")))
      .toBeTruthy();
    await page.goto(`${CZ}/p/cepice-merino`);
    const recent = page.getByRole("region", { name: "Naposledy prohlížené" });
    await expect(recent.getByRole("link", { name: "Mikina Fleece" })).toBeVisible();
  });
});
