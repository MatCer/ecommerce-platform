import { randomBytes } from "node:crypto";
import { type BrowserContext, expect, type Page, type Request, test } from "@playwright/test";
import {
  acceptAndPlace,
  checkoutReady,
  chooseCashOnDelivery,
  detailsForm,
  FRESH_HTTPS,
  isPlacement,
  postDetails,
  receiptNumber,
  sameOriginReview,
  securityContext,
  startSameOriginCheckout,
} from "./support";

interface DocumentState {
  inert: boolean;
  old: true | null;
}

declare global {
  interface Window {
    oldDocument?: true;
    reportOldDocument?: (state: DocumentState) => void;
  }
}

const email = () => `obnova-${randomBytes(6).toString("hex")}@example.test`;
const placeButton = (page: Page) =>
  page.getByRole("button", { name: "Objednat s povinností platby" });

async function review(context: BrowserContext): Promise<Page> {
  const page = await context.newPage();
  await sameOriginReview(page, FRESH_HTTPS, email());
  await page.waitForLoadState("load");
  await checkoutReady(page);
  return page;
}

async function restore(page: Page): Promise<Request> {
  const whileHeld: DocumentState[] = [];
  let holding = false;
  await page.exposeFunction("reportOldDocument", (state: DocumentState) => {
    if (holding) whileHeld.push(state);
  });
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let routed!: (request: Request) => void;
  const reload = new Promise<Request>((resolve) => {
    routed = resolve;
  });
  await page.route(
    (url) => url.pathname === "/checkout",
    async (route) => {
      holding = true;
      routed(route.request());
      await held;
      holding = false;
      await route.continue();
    },
  );
  await expect(page.locator("body")).toHaveJSProperty("inert", false);
  await page.evaluate(() => {
    window.oldDocument = true;
    setInterval(
      () =>
        window.reportOldDocument?.({
          inert: document.body.inert,
          old: window.oldDocument ?? null,
        }),
      50,
    );
    dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true }));
  });
  const request = await reload;
  await expect
    .poll(
      () => whileHeld.at(-1),
      "the old document reports itself inert while its replacement is held (Chromium evaluates nothing in a document whose navigation is held)",
    )
    .toEqual({ inert: true, old: true });
  const navigated = page.waitForEvent("framenavigated", (frame) => frame === page.mainFrame());
  release();
  await navigated;
  await page.waitForLoadState("load");
  await checkoutReady(page);
  expect(
    await page.evaluate(() => ({ inert: document.body.inert, old: window.oldDocument ?? null })),
    "a new document",
  ).toEqual({ inert: false, old: null });
  expect(request.method(), "the restore loads the page by GET").toBe("GET");
  expect(request.isNavigationRequest()).toBe(true);
  return request;
}

test.describe("no engine here restores these no-store pages from the back-forward cache, so synthetic persisted page transitions drive the restore handler", () => {
  test("a persisted pagehide on the review makes the page inert", async ({ browser }) => {
    const context = await securityContext(browser);
    const page = await review(context);
    await expect(page.locator("body")).toHaveJSProperty("inert", false);
    await page.evaluate(() =>
      dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true })),
    );
    await expect(page.locator("body"), "the page enters the cache inert").toHaveJSProperty(
      "inert",
      true,
    );
    await context.close();
  });

  test("a persisted pageshow on the review replaces it with a fresh review", async ({
    browser,
  }) => {
    const context = await securityContext(browser);
    const page = await review(context);
    const request = await restore(page);
    expect(request.url(), "the review's own path").toBe(`${FRESH_HTTPS}/checkout`);
    await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout`);
    await chooseCashOnDelivery(page);
    await expect(placeButton(page), "the fresh review works").toBeEnabled();
    await context.close();
  });

  test("a persisted pageshow on a refused form's page loads /checkout and resends no form", async ({
    browser,
  }) => {
    const context = await securityContext(browser);
    const page = await context.newPage();
    await startSameOriginCheckout(page, FRESH_HTTPS);
    const field = detailsForm(page, "contact").locator('[name="email"]');
    await field.evaluate((el) => el.removeAttribute("pattern"));
    await field.fill(`obnova-${randomBytes(6).toString("hex")}@example`);
    expect(await postDetails(page, "contact"), "an email only the server refuses").toBe(422);
    await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout/contact`);
    await page.waitForLoadState("load");
    await checkoutReady(page);
    const posts: string[] = [];
    page.on("request", (r) => {
      if (r.method() === "POST") posts.push(new URL(r.url()).pathname);
    });
    const request = await restore(page);
    expect(request.url(), "the path alone, never the refused form's query").toBe(
      `${FRESH_HTTPS}/checkout`,
    );
    await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout`);
    expect(posts, "the restore resends no form").toEqual([]);
    await expect(detailsForm(page, "contact").locator('[name="email"]')).toHaveValue("");
    await context.close();
  });
});

test("the back button from the receipt never brings the review back", async ({
  browser,
  browserName,
}) => {
  const context = await securityContext(browser);
  const page = await review(context);
  const placements: string[] = [];
  page.on("request", (r) => {
    if (isPlacement(r)) placements.push(r.url());
  });
  await chooseCashOnDelivery(page);
  const entries = await page.evaluate(() => {
    window.oldDocument = true;
    return history.length;
  });
  await acceptAndPlace(page);
  const number = await receiptNumber(page);
  const firefox = browserName === "firefox";
  expect(
    (await page.evaluate(() => history.length)) - entries,
    "the receipt gets its own history entry, except in Firefox, whose receipt replaces the review's entry of the same URL",
  ).toBe(firefox ? 0 : 1);
  await page.goBack();
  if (firefox) {
    await expect(page, "Firefox has no review entry to go back to").toHaveURL(`${FRESH_HTTPS}/`);
  } else {
    await expect(page).toHaveURL(`${FRESH_HTTPS}/checkout`);
    expect(await receiptNumber(page), "the same receipt").toBe(number);
    expect(
      await page.evaluate(() => ({
        old: window.oldDocument ?? null,
        type: (performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming).type,
      })),
      "no engine here restores a no-store checkout page from the back-forward cache: the back button loads it fresh",
    ).toEqual({ old: null, type: "back_forward" });
  }
  await expect(placeButton(page), "never the review").toHaveCount(0);
  expect(placements, "the back button resends no placement").toHaveLength(1);
  await context.close();
});
