/**
 * Browser options shared by the gates (measure, smoke). The theme builder runs them inside a
 * sandbox container against a preview host (WP23), which needs three things the local runs do
 * not:
 * - `THEME_KIT_CHROMIUM_ARGS` (JSON array): extra Chromium flags, e.g.
 *   `["--host-resolver-rules=MAP *.localhost caddy"]` (Chromium resolves `*.localhost` to
 *   loopback by itself, so the preview host must be mapped to the proxy explicitly);
 * - a cookie (`--cookie name=value`, the preview token) sent with every request;
 * - page-context fetches instead of Node-side requests, so every request goes through the same
 *   (mapped) Chromium network stack.
 */
import type { BrowserContext, Page } from "playwright";

export function chromiumArgs(): string[] {
  const raw = process.env.THEME_KIT_CHROMIUM_ARGS;
  if (!raw) return [];
  const args: unknown = JSON.parse(raw);
  if (!Array.isArray(args) || !args.every((a) => typeof a === "string"))
    throw new Error("THEME_KIT_CHROMIUM_ARGS must be a JSON array of strings");
  return args;
}

export interface Cookie {
  name: string;
  value: string;
}

/** `name=value` → cookie (the value is used as is: preview tokens are URL-safe). */
export function parseCookie(raw: string | undefined): Cookie | undefined {
  if (!raw) return undefined;
  const i = raw.indexOf("=");
  if (i < 1) throw new Error("--cookie expects name=value");
  return { name: raw.slice(0, i), value: raw.slice(i + 1) };
}

export async function addCookie(ctx: BrowserContext, base: URL, cookie: Cookie | undefined) {
  if (!cookie) return;
  await ctx.addCookies([
    {
      ...cookie,
      url: base.origin,
      secure: base.protocol === "https:",
      httpOnly: true,
      sameSite: "Lax",
    },
  ]);
}

/**
 * Page-model calls of a fresh render of `url` (`x-edge-subrequests`). `Authorization` makes the
 * edge bypass its HTML cache (A2); previews are never cached anyway. Read from a separate
 * navigation's response (through the browser, so cookies and host mapping apply) before any
 * theme script runs: page JavaScript cannot fake the number.
 */
export async function freshSubrequests(page: Page, url: string): Promise<number> {
  const probe = await page.context().newPage();
  try {
    await probe.setExtraHTTPHeaders({ authorization: "Bearer gate" });
    // Only the document: no theme script, style or image is loaded or run.
    await probe.route("**/*", (r) =>
      r.request().isNavigationRequest() ? r.continue() : r.abort(),
    );
    await probe.addInitScript(() => {
      window.stop();
    });
    const res = await probe.goto(url, { waitUntil: "commit" });
    const n = res?.headers()["x-edge-subrequests"];
    return n === undefined ? Number.NaN : Number(n);
  } finally {
    await probe.close();
  }
}
