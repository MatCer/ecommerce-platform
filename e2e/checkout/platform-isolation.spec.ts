import { randomBytes } from "node:crypto";
import {
  type Browser,
  type BrowserContextOptions,
  expect,
  type Page,
  test,
} from "@playwright/test";
import { sql } from "../admin/support";
import {
  checkoutReady,
  choosePickupPoint,
  cookieValue,
  FRESH,
  FRESH_HTTPS,
  pageTokenMeta,
  RESTARTED,
  restartButton,
  restartNotice,
  securityContext,
  startSameOriginCheckout,
} from "./support";

declare global {
  interface Window {
    probe?: Window | null;
  }
}

const TOKEN = /^[A-Za-z0-9_-]{43}$/;
const CAPABILITY = /^[0-9a-f]{64}$/;

async function openCheckout(
  browser: Browser,
  shop = FRESH_HTTPS,
  options: BrowserContextOptions = {},
): Promise<{ page: Page; cap: string; token: string; marker: string }> {
  const context = await securityContext(browser, options);
  const page = await context.newPage();
  await startSameOriginCheckout(page, shop);
  const cap = (await cookieValue(context, "__Host-Http-cart")) ?? "";
  expect(cap, "the start set the same-origin capability").toMatch(CAPABILITY);
  const marker = `pii${randomBytes(6).toString("hex")}`;
  const address = JSON.stringify({
    name: `${marker} Jana`,
    street: `${marker} 12`,
    city: "Praha",
    postal_code: "110 00",
    country: "CZ",
  });
  expect(
    sql(`WITH planted AS (
           UPDATE carts SET email = '${marker}@example.test', billing_address = '${address}'::jsonb
           WHERE checkout_token_hash = sha256(convert_to('${cap}', 'UTF8')) RETURNING 1)
         SELECT count(*) FROM planted`),
    "exactly one snapshot holds the capability",
  ).toBe("1");
  const html = (await (await page.reload())?.text()) ?? "";
  expect(html, "the platform HTML shows the planted contact").toContain(`${marker}@example.test`);
  expect(html, "the platform HTML shows the planted address").toContain(`${marker} 12`);
  await expect(pageTokenMeta(page)).toHaveCount(1);
  const token = (await pageTokenMeta(page).getAttribute("content")) ?? "";
  expect(token).toMatch(TOKEN);
  return { page, cap, token, marker };
}

async function themePage(page: Page): Promise<Page> {
  const theme = await page.context().newPage();
  await theme.goto(`${FRESH_HTTPS}/`);
  return theme;
}

/** A click on an injected button, so the opener has a user activation. */
async function openByClick(page: Page, url: string): Promise<Page> {
  await page.evaluate((url) => {
    const button = document.createElement("button");
    button.textContent = "probe-open";
    button.style.cssText = "position:fixed;top:0;left:0;z-index:2147483647";
    button.addEventListener("click", () => {
      window.probe = window.open(url);
      button.remove();
    });
    document.body.append(button);
  }, url);
  const [popup] = await Promise.all([
    page.context().waitForEvent("page"),
    page.getByRole("button", { name: "probe-open" }).click(),
  ]);
  return popup;
}

const probeClosed = (page: Page) => page.evaluate(() => window.probe?.closed);

test("theme fetch and XHR of platform page paths get an empty 403", async ({ browser }) => {
  const { page } = await openCheckout(browser);
  const theme = await themePage(page);
  const paths = ["/checkout", "/CHECKOUT", "/cs/CHECKOUT"];
  const fetched = await theme.evaluate(
    (paths) =>
      Promise.all(
        paths.map(async (path) => {
          const res = await fetch(path);
          return { path, status: res.status, body: await res.text() };
        }),
      ),
    paths,
  );
  expect(fetched).toEqual(paths.map((path) => ({ path, status: 403, body: "" })));
  const xhr = await theme.evaluate(
    () =>
      new Promise<{ status: number; body: string }>((resolve) => {
        const req = new XMLHttpRequest();
        req.onloadend = () => resolve({ status: req.status, body: req.responseText });
        req.open("GET", "/checkout");
        req.send();
      }),
  );
  expect(xhr).toEqual({ status: 403, body: "" });
  await page.context().close();
});

test("with the theme CSP bypassed, a frame of the platform page gets a 403 and the theme reads nothing", async ({
  browser,
}) => {
  const { page, marker } = await openCheckout(browser, FRESH_HTTPS, { bypassCSP: true });
  const theme = await themePage(page);
  const framed = theme.waitForResponse(
    (r) => r.url() === `${FRESH_HTTPS}/checkout` && r.frame() !== theme.mainFrame(),
  );
  await theme.evaluate(() => {
    const frame = document.createElement("iframe");
    frame.id = "probe-frame";
    frame.src = "/checkout";
    document.body.append(frame);
  });
  const response = await framed;
  expect(await response.request().headerValue("sec-fetch-dest")).toBe("iframe");
  expect(response.status()).toBe(403);
  const read = await theme.evaluate(() => {
    const frame = document.getElementById("probe-frame") as HTMLIFrameElement | null;
    try {
      return frame?.contentDocument?.documentElement?.outerHTML ?? "";
    } catch {
      return "";
    }
  });
  expect(read).not.toContain(marker);
  await page.context().close();
});

test("the checkout API answers only the platform page's token for its own capability", async ({
  browser,
}) => {
  const { page, cap, token, marker } = await openCheckout(browser);
  const other = await openCheckout(browser);
  const theme = await themePage(page);
  const call = (from: Page, presented?: string) =>
    from.evaluate(async (presented) => {
      const res = await fetch("/_p/checkout", {
        headers: presented ? { "x-page-token": presented } : {},
      });
      const headers: string[] = [];
      res.headers.forEach((value, name) => {
        headers.push(`${name}: ${value}`);
      });
      return { status: res.status, headers: headers.join("\n"), body: await res.text() };
    }, presented);
  expect((await call(theme)).status, "theme JS sends the cookie without a token").toBe(403);
  const own = await call(page, token);
  expect(own.status).toBe(200);
  expect(own.body, "the call reads this checkout").toContain(`${marker}@example.test`);
  expect(`${own.headers}${own.body}`).not.toContain(token);
  expect(`${own.headers}${own.body}`).not.toContain(cap);
  expect((await call(page, other.token)).status, "a token of another capability").toBe(403);
  await page.context().close();
  await other.page.context().close();
});

test("the island's shipping save carries the page token", async ({ browser }) => {
  const { page, token } = await openCheckout(browser);
  await checkoutReady(page);
  const [saved] = await Promise.all([
    page.waitForRequest(
      (r) => r.method() === "PUT" && r.url() === `${FRESH_HTTPS}/_p/checkout/shipping`,
    ),
    page.getByRole("radio", { name: /Zásilkovna – na adresu/ }).check(),
  ]);
  expect(await saved.headerValue("x-page-token")).toBe(token);
  expect((await saved.response())?.status()).toBe(200);
  await page.context().close();
});

test("the Packeta widget works under the platform CSP", async ({ browser, browserName }) => {
  test.skip(
    browserName !== "chromium",
    "the mock widget is plain HTTP, so it runs only on the http shop origin; one engine proves the CSP allowance",
  );
  const { page, token } = await openCheckout(browser, FRESH);
  await checkoutReady(page);
  const saved = page.waitForRequest(
    (r) => r.method() === "PUT" && r.url() === `${FRESH}/_p/checkout/shipping`,
  );
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  expect(await (await saved).headerValue("x-page-token")).toBe(token);
  expect((await (await saved).response())?.status()).toBe(200);
  await page.context().close();
});

test("checkout scripts and styles load from one build prefix", async ({ browser }) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const loaded: { path: string; status: number; type: string }[] = [];
  page.on("response", (r) => {
    const url = new URL(r.url());
    const type = r.request().resourceType();
    if (
      new URL(r.frame().url()).pathname === "/checkout" &&
      url.origin === FRESH_HTTPS &&
      (type === "script" || type === "stylesheet") &&
      url.pathname !== "/_p/tokens.css"
    )
      loaded.push({ path: url.pathname, status: r.status(), type });
  });
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await checkoutReady(page);
  expect(loaded.map((a) => a.type)).toEqual(expect.arrayContaining(["script", "stylesheet"]));
  expect(
    loaded.filter((a) => a.status !== 200 || !/^\/_checkout\/[0-9a-f]{16}\//.test(a.path)),
  ).toEqual([]);
  expect(new Set(loaded.map((a) => a.path.split("/")[2])).size, "one build id").toBe(1);
  await context.close();
});

test("COOP severs every opener between theme and platform pages", async ({ browser }) => {
  const { page } = await openCheckout(browser);
  const theme = await themePage(page);

  const platform = await openByClick(theme, "/checkout");
  await platform.waitForURL(`${FRESH_HTTPS}/checkout`);
  await expect.poll(() => probeClosed(theme), "theme -> platform popup").toBe(true);

  const held = await openByClick(theme, "/");
  await held.waitForURL(`${FRESH_HTTPS}/`);
  expect(
    await theme.evaluate(() => [window.probe?.closed, window.probe?.location.pathname]),
    "theme -> theme keeps the opener",
  ).toEqual([false, "/"]);
  await theme.evaluate(() => window.probe?.location.assign("/checkout"));
  await held.waitForURL(`${FRESH_HTTPS}/checkout`);
  await expect.poll(() => probeClosed(theme), "held popup navigated to the platform").toBe(true);

  const shop = await openByClick(page, "/");
  await shop.waitForURL(`${FRESH_HTTPS}/`);
  await expect.poll(() => probeClosed(page), "platform -> theme popup").toBe(true);
  expect(await shop.evaluate(() => window.opener === null)).toBe(true);
  await page.context().close();
});

test("the restart form posts with the shop's Origin and opens a new checkout", async ({
  browser,
}) => {
  const { page, cap } = await openCheckout(browser);
  await page.context().clearCookies({ name: "__Host-Http-cart" });
  await page.reload();
  await expect(restartNotice(page)).toHaveText(RESTARTED);
  await expect(pageTokenMeta(page)).toHaveCount(0);
  const landed = page.waitForResponse(
    (r) =>
      new URL(r.url()).pathname === "/checkout" &&
      r.request().redirectedFrom()?.method() === "POST",
  );
  const [start] = await Promise.all([
    page.waitForRequest(
      (r) => r.method() === "POST" && r.url() === `${FRESH_HTTPS}/_p/checkout/start`,
    ),
    restartButton(page).click(),
  ]);
  expect(await start.headerValue("origin")).toBe(FRESH_HTTPS);
  expect((await landed).status()).toBe(200);
  await expect(pageTokenMeta(page)).toHaveAttribute("content", TOKEN);
  const restarted = await cookieValue(page.context(), "__Host-Http-cart");
  expect(restarted).toMatch(CAPABILITY);
  expect(restarted).not.toBe(cap);
  await page.context().close();
});

test("service worker registration fails on theme and platform pages", async ({ browser }) => {
  const { page } = await openCheckout(browser);
  const theme = await themePage(page);
  for (const [on, prefix] of [
    [theme, "/_astro/"],
    [page, "/_checkout/"],
  ] as const) {
    const outcome = await on.evaluate(async (prefix) => {
      const script = Array.from(document.querySelectorAll("astro-island[component-url]"))
        .map((island) => new URL(island.getAttribute("component-url") ?? "", location.href))
        .find((u) => u.origin === location.origin && u.pathname.startsWith(prefix))?.href;
      if (!script) return { script };
      const blocked = new Promise<string>((resolve) =>
        document.addEventListener("securitypolicyviolation", (e) => {
          if (e.blockedURI === script) resolve(e.effectiveDirective);
        }),
      );
      const rejected = await navigator.serviceWorker.register(script).then(
        () => "registered",
        (e: Error) => e.name,
      );
      return {
        script: "found",
        rejected,
        blocked: rejected === "registered" ? null : await blocked,
        registrations: (await navigator.serviceWorker.getRegistrations()).length,
      };
    }, prefix);
    expect(outcome, `a same-origin ${prefix} script as a service worker`).toEqual({
      script: "found",
      rejected: "SecurityError",
      blocked: "worker-src",
      registrations: 0,
    });
  }
  await page.context().close();
});

test("Chromium speculation and prefetch never fetch platform HTML", async ({
  browser,
  browserName,
}) => {
  test.skip(
    browserName !== "chromium",
    "speculation rules and the CDP Preload domain are Chromium-only",
  );
  const { page } = await openCheckout(browser);
  const theme = await themePage(page);
  const cdp = await page.context().newCDPSession(theme);
  const requests = new Map<string, { url?: string; purpose?: string; status?: number }>();
  const attempts = new Map<string, string>();
  const record = (id: string, seen: { url?: string; purpose?: string; status?: number }) =>
    requests.set(id, { ...requests.get(id), ...seen });
  cdp.on("Network.requestWillBeSent", (e) => record(e.requestId, { url: e.request.url }));
  cdp.on("Network.requestWillBeSentExtraInfo", (e) => {
    const purpose = Object.entries(e.headers).find(([k]) => k.toLowerCase() === "sec-purpose");
    if (purpose) record(e.requestId, { purpose: String(purpose[1]) });
  });
  cdp.on("Network.responseReceived", (e) => record(e.requestId, { status: e.response.status }));
  // Under DevTools Chromium cancels the prerender itself (PrerenderingDisabledByDevTools), so the
  // edge sees only its prefetch-ahead-of-prerender, reported on the initiating page's session.
  cdp.on("Preload.prefetchStatusUpdated", (e) => attempts.set(e.prefetchUrl, e.requestId));
  await cdp.send("Preload.enable");
  await cdp.send("Network.enable");

  const paths = ["/CHECKOUT", "/cs/CHECKOUT"];
  await theme.evaluate((paths) => {
    paths.forEach((path, i) => {
      const a = document.createElement("a");
      a.href = path;
      a.textContent = `probe ${path}`;
      a.style.cssText = `position:fixed;left:0;top:${40 * i}px;z-index:2147483647;background:#fff`;
      document.body.append(a);
    });
  }, paths);
  for (const path of paths) {
    const url = `${FRESH_HTTPS}${path}`;
    await theme.getByRole("link", { name: `probe ${path}`, exact: true }).hover();
    await expect
      .poll(() => requests.get(attempts.get(url) ?? ""), `speculation of ${path}`)
      .toEqual({ url, purpose: "prefetch;prerender", status: 403 });
  }

  const prefetched = theme.waitForResponse(
    (r) =>
      r.url() === `${FRESH_HTTPS}/CHECKOUT` && r.request().headers()["sec-purpose"] === "prefetch",
  );
  await theme.evaluate(() => {
    const link = document.createElement("link");
    link.rel = "prefetch";
    link.href = "/CHECKOUT";
    document.head.append(link);
  });
  expect((await prefetched).status()).toBe(403);
  await page.context().close();
});
