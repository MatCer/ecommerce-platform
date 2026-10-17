import { createHmac, timingSafeEqual } from "node:crypto";

export const WORKER_PAGE_TOKEN_HEADER = "x-platform-page-token";
export const CLIENT_PAGE_TOKEN_HEADER = "x-page-token";

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
