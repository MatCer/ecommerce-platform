/**
 * Staff session (spec §5.3, A9). Better Auth keeps an HttpOnly session cookie on the admin
 * origin; the Admin API gets a 5-minute EdDSA JWT from `GET /api/auth/token`. The JWT lives in
 * this module's memory only: never in storage, never in the URL. It is refreshed lazily one
 * minute before expiry (single flight) and on `401 invalid_token`.
 */
import { createAuthClient } from "better-auth/client";
import { magicLinkClient, twoFactorClient } from "better-auth/client/plugins";
import { createSignal } from "solid-js";
import { AUTH_BASE } from "./config.ts";
import { isFresh, readClaims, type StaffClaims } from "./jwt.ts";

export const authClient = createAuthClient({
  baseURL: globalThis.location?.origin,
  basePath: AUTH_BASE,
  plugins: [magicLinkClient(), twoFactorClient()],
});

export type SessionStatus = "loading" | "signed-in" | "signed-out";

const [status, setStatus] = createSignal<SessionStatus>("loading");
const [claims, setClaims] = createSignal<StaffClaims | null>(null);

export { claims, status };

let token: string | null = null;
let inflight: Promise<string | null> | null = null;

export class SignedOutError extends Error {
  constructor() {
    super("signed out");
  }
}

async function fetchToken(): Promise<string | null> {
  const res = await fetch(`${AUTH_BASE}/token`, { credentials: "same-origin" });
  // 401: no session. 403: email not verified (tokens are refused, A9).
  if (res.status === 401 || res.status === 403) return null;
  if (!res.ok) throw new Error(`token endpoint answered ${res.status}`);
  const body: unknown = await res.json();
  const value = typeof body === "object" && body !== null ? Reflect.get(body, "token") : null;
  return typeof value === "string" ? value : null;
}

/** Fetches a new JWT (one request at a time). `null` means there is no session. */
export function refreshToken(): Promise<string | null> {
  inflight ??= fetchToken()
    .then((t) => {
      const c = t ? readClaims(t) : null;
      token = c ? t : null;
      setClaims(c);
      setStatus(c ? "signed-in" : "signed-out");
      return token;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/** A JWT valid for at least another minute; throws `SignedOutError` without a session. */
export async function accessToken(): Promise<string> {
  const c = claims();
  if (token && c && isFresh(c, Date.now() / 1000)) return token;
  const t = await refreshToken();
  if (!t) throw new SignedOutError();
  return t;
}

/** Called once at start-up: signed in if the session cookie yields a token. */
export async function bootstrapSession(): Promise<void> {
  try {
    await refreshToken();
  } catch {
    setStatus("signed-out");
  }
}

export async function signOut(): Promise<void> {
  try {
    await authClient.signOut();
  } finally {
    token = null;
    setClaims(null);
    setStatus("signed-out");
  }
}

export type SignInResult =
  | { kind: "ok" }
  | { kind: "two-factor" }
  | { kind: "error"; code: string; status: number };

function failure(error: { code?: string; status: number }): SignInResult {
  return { kind: "error", code: error.code ?? "UNKNOWN", status: error.status };
}

export async function signInWithPassword(email: string, password: string): Promise<SignInResult> {
  const { data, error } = await authClient.signIn.email({ email, password });
  if (error) return failure(error);
  if (data && "twoFactorRedirect" in data && data.twoFactorRedirect) return { kind: "two-factor" };
  return (await refreshToken()) ? { kind: "ok" } : { kind: "error", code: "NO_TOKEN", status: 403 };
}

export async function verifyTotp(code: string): Promise<SignInResult> {
  const { error } = await authClient.twoFactor.verifyTotp({ code, trustDevice: false });
  if (error) return failure(error);
  return (await refreshToken()) ? { kind: "ok" } : { kind: "error", code: "NO_TOKEN", status: 403 };
}

export async function sendMagicLink(email: string): Promise<SignInResult> {
  const { error } = await authClient.signIn.magicLink({
    email,
    callbackURL: "/",
    errorCallbackURL: "/login?error=link",
  });
  return error ? failure(error) : { kind: "ok" };
}

export async function requestPasswordReset(email: string): Promise<SignInResult> {
  const redirectTo = `${location.origin}/reset-password`;
  const { error } = await authClient.requestPasswordReset({ email, redirectTo });
  return error ? failure(error) : { kind: "ok" };
}

export async function resetPassword(tokenValue: string, newPassword: string) {
  const { error } = await authClient.resetPassword({ token: tokenValue, newPassword });
  return error ? failure(error) : ({ kind: "ok" } as const);
}
