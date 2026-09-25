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
 * edge bypass its HTML cache (A2); previews are never cached anyway. Fetched from the page, so
 * cookies and host mapping apply.
 */
export async function freshSubrequests(page: Page, url: string): Promise<number> {
  const n = await page.evaluate(async (u) => {
    const res = await fetch(u, { headers: { authorization: "Bearer gate" } });
    return res.headers.get("x-edge-subrequests");
  }, url);
  return n === null ? Number.NaN : Number(n);
}
