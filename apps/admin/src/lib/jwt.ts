/** Claims the SPA reads from its staff JWT. Never used for authorization (the API verifies). */
export interface StaffClaims {
  sub: string;
  email: string;
  exp: number;
  auth_time: number;
}

function base64UrlDecode(part: string): string {
  const b64 = part.replace(/-/g, "+").replace(/_/g, "/");
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  const bytes = Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/** Reads (does not verify) the payload of a JWT; `null` if it is malformed. */
export function readClaims(token: string): StaffClaims | null {
  const payload = token.split(".")[1];
  if (!payload) return null;
  try {
    const raw: unknown = JSON.parse(base64UrlDecode(payload));
    if (typeof raw !== "object" || raw === null) return null;
    const c = raw as Record<string, unknown>;
    if (typeof c.sub !== "string" || typeof c.exp !== "number") return null;
    return {
      sub: c.sub,
      email: typeof c.email === "string" ? c.email : "",
      exp: c.exp,
      auth_time: typeof c.auth_time === "number" ? c.auth_time : 0,
    };
  } catch {
    return null;
  }
}

/** Refresh this many seconds before expiry (tokens live 5 minutes). */
export const REFRESH_SKEW_S = 60;

export function isFresh(claims: StaffClaims, nowS: number): boolean {
  return claims.exp - REFRESH_SKEW_S > nowS;
}
