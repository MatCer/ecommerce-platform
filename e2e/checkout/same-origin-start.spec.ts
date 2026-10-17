import { expect, type Page, type Route, test } from "@playwright/test";
import {
  addLine,
  cookieValue,
  FRESH_HTTPS,
  pageTokenMeta,
  RESTARTED,
  restartButton,
  restartNotice,
  securityContext,
  shopTab,
  submitStart,
} from "./support";

const CAPABILITY = /^[0-9a-f]{64}$/;

async function hold(page: Page): Promise<Route> {
  let held!: (route: Route) => void;
  const route = new Promise<Route>((resolve) => {
    held = resolve;
  });
  await page.route("**/_p/checkout/start", (r) => held(r));
  await submitStart(page);
  return route;
}

/** Chromium intercepts before it adds Fetch Metadata, so the replay states the admission headers. */
async function replay(route: Route, frozenShopCart: string) {
  const response = await route.fetch({
    maxRedirects: 0,
    headers: {
      cookie: `cart=${frozenShopCart}`,
      origin: FRESH_HTTPS,
      "sec-fetch-site": "same-origin",
      "sec-fetch-mode": "navigate",
      "sec-fetch-dest": "document",
      "content-type": "application/x-www-form-urlencoded",
    },
  });
  expect(response.status()).toBe(303);
  expect(response.headers().location).toBe("/checkout");
  const cap = /__Host-Http-cart=([0-9a-f]{64});/.exec(response.headers()["set-cookie"] ?? "")?.[1];
  expect(cap, "the start minted a capability").toMatch(CAPABILITY);
  return { response, cap: cap ?? "" };
}

test("a start answered after a newer one shows the restart notice; only a new start opens", async ({
  browser,
  browserName,
}) => {
  test.skip(
    browserName === "webkit",
    "Playwright 1.63 cannot fulfill a held 3xx response in WebKit (WKRouteImpl.fulfill)",
  );
  const context = await securityContext(browser);
  const first = await context.newPage();
  await first.goto(`${FRESH_HTTPS}/`, { waitUntil: "domcontentloaded" });
  await addLine(first, FRESH_HTTPS, "tricko-basic");
  const second = await shopTab(context, `${FRESH_HTTPS}/`);
  const frozenShopCart = (await cookieValue(context, "cart")) ?? "";
  expect(frozenShopCart).toMatch(CAPABILITY);

  const startA = await hold(first);
  const startB = await hold(second);
  const a = await replay(startA, frozenShopCart);
  const b = await replay(startB, frozenShopCart);
  expect(b.cap).not.toBe(a.cap);

  await startB.fulfill({ response: b.response });
  await second.waitForURL(`${FRESH_HTTPS}/checkout`, { waitUntil: "domcontentloaded" });
  await expect(pageTokenMeta(second), "the newer start opens").toHaveCount(1);
  await startA.fulfill({ response: a.response });
  await first.waitForURL(`${FRESH_HTTPS}/checkout`, { waitUntil: "domcontentloaded" });
  expect(await cookieValue(context, "__Host-Http-cart"), "tab 1 presents A").toBe(a.cap);
  await expect(restartNotice(first)).toHaveText(RESTARTED);
  await expect(pageTokenMeta(first)).toHaveCount(0);

  await first.unroute("**/_p/checkout/start");
  await Promise.all([
    first.waitForResponse(
      (r) =>
        new URL(r.url()).pathname === "/checkout" &&
        r.request().redirectedFrom()?.method() === "POST",
    ),
    restartButton(first).click(),
  ]);
  await expect(pageTokenMeta(first), "the restart opens").toHaveCount(1);
  const c = (await cookieValue(context, "__Host-Http-cart")) ?? "";
  expect(c).toMatch(CAPABILITY);
  expect([a.cap, b.cap]).not.toContain(c);
  await context.close();

  for (const [cap, opens] of [
    [a.cap, false],
    [b.cap, false],
    [c, true],
  ] as const) {
    const alone = await securityContext(browser);
    await alone.addCookies([
      { name: "__Host-Http-cart", value: cap, url: FRESH_HTTPS, httpOnly: true, secure: true },
    ]);
    const page = await alone.newPage();
    await page.goto(`${FRESH_HTTPS}/checkout`);
    if (!opens) await expect(restartNotice(page)).toHaveText(RESTARTED);
    await expect(pageTokenMeta(page)).toHaveCount(opens ? 1 : 0);
    await alone.close();
  }
});
