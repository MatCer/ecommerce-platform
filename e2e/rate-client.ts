/** A signed, per-browser-context rate identity for the local Compose e2e stack. */
import { createHmac, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Browser, BrowserContext, Page } from "@playwright/test";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const byContext = new WeakMap<BrowserContext, Record<string, string>>();

function secret(): string | undefined {
  if (process.env.E2E_RATE_SECRET) return process.env.E2E_RATE_SECRET;
  try {
    return readFileSync(join(root, ".env"), "utf8")
      .split("\n")
      .find((line) => line.startsWith("E2E_RATE_SECRET="))
      ?.slice("E2E_RATE_SECRET=".length)
      .trim();
  } catch {
    return undefined;
  }
}

export function testRateHeaders(): Record<string, string> {
  const key = secret();
  if (!key) return {};
  const id = randomBytes(12).toString("hex");
  const mac = createHmac("sha256", key).update(id).digest("hex");
  return { "x-e2e-rate-key": `${id}.${mac}` };
}

export async function testContext(
  browser: Browser,
  options: Parameters<Browser["newContext"]>[0] = {},
) {
  const context = await browser.newContext(options);
  const headers = testRateHeaders();
  byContext.set(context, headers);
  if (headers["x-e2e-rate-key"]) {
    // Admin API and S3 are separate origins with their own CORS policies. Only the auth and
    // storefront entrypoints need this identity; never add it to cross-origin API/upload calls.
    await context.route("**/*", (route) => {
      const host = new URL(route.request().url()).hostname;
      if (
        host === "admin.localhost" ||
        host === "auth.localhost" ||
        (host.endsWith(".localhost") &&
          !["api.localhost", "s3.localhost", "mail.localhost", "mocks.localhost"].includes(host))
      ) {
        return route.continue({ headers: { ...route.request().headers(), ...headers } });
      }
      return route.continue();
    });
  }
  return context;
}

/** APIRequestContext calls bypass browser routes, so pass the same signed identity explicitly. */
export function rateHeaders(page: Page): Record<string, string> {
  return byContext.get(page.context()) ?? {};
}
