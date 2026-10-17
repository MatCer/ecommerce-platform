/**
 * Page tokens (C4, `docs/decisions/same-origin-checkout.md`): the edge mints one for the checkout
 * capability a platform page is rendered with and requires it back on that page's API calls.
 */
import { createHmac, timingSafeEqual } from "node:crypto";

/** Edge → checkout worker only; clients cannot send `x-platform-*` (`stripUntrusted`). */
export const PAGE_TOKEN_HEADER = "x-platform-page-token";

/**
 * `base64url(HMAC-SHA256(secret, "pt1|" + host + "|" + capability))`, `host` being the canonical
 * authority of `shopOrigin` (non-default port kept). No secret or no capability, no token.
 */
export function pageToken(
  secret: string | undefined,
  shopOrigin: string,
  capability: string | undefined,
): string | undefined {
  if (!secret || !capability) return undefined;
  return createHmac("sha256", secret)
    .update(`pt1|${new URL(shopOrigin).host}|${capability}`)
    .digest("base64url");
}

/**
 * Constant-time check of a presented token against the request's own capability. It proves
 * possession only: the API still authorizes the capability in its transaction.
 */
export function verifyPageToken(
  secret: string | undefined,
  shopOrigin: string,
  capability: string | undefined,
  presented: string | null | undefined,
): boolean {
  const expected = pageToken(secret, shopOrigin, capability);
  if (!expected || !presented) return false;
  const a = Buffer.from(presented);
  const b = Buffer.from(expected);
  return a.length === b.length && timingSafeEqual(a, b);
}
