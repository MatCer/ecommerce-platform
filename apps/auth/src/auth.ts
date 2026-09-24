import { type BetterAuthOptions, betterAuth } from "better-auth";
import { APIError } from "better-auth/api";
import { jwt, magicLink, twoFactor } from "better-auth/plugins";
import type { Config } from "./config.ts";
import { type Mailer, templates } from "./mail.ts";

/** The `aud` of staff JWTs, verified by the Rust Admin API (spec A9). */
export const AUDIENCE = "admin-api";

interface SessionLike {
  user: { email: string; emailVerified: boolean };
  session: { createdAt: Date | string };
}

/**
 * Claims of a staff JWT (spec A9). Better Auth adds `iss`, `aud`, `exp`, `iat` and `sub`
 * (the user id). Tokens are refused to users whose email is not verified; users with 2FA
 * enabled only get a session (and so a token) after the second factor.
 */
export function staffClaims({ user, session }: SessionLike) {
  if (!user.emailVerified) {
    throw new APIError("FORBIDDEN", { message: "Email address is not verified" });
  }
  return {
    email: user.email,
    email_verified: true,
    // The login time: sessions are created at sign-in and not re-created on refresh.
    auth_time: Math.floor(new Date(session.createdAt).getTime() / 1000),
  };
}

export function createAuth(cfg: Config, database: BetterAuthOptions["database"], sendMail: Mailer) {
  return betterAuth({
    appName: "Commerce Platform",
    baseURL: cfg.baseUrl,
    basePath: "/api/auth",
    secret: cfg.secret,
    database,
    trustedOrigins: [cfg.adminOrigin],
    telemetry: { enabled: false },
    // Staff are invited by the platform (`api admin create-tenant`); there is no self sign-up.
    emailAndPassword: {
      enabled: true,
      disableSignUp: true,
      requireEmailVerification: true,
      minPasswordLength: 12,
      revokeSessionsOnPasswordReset: true,
      sendResetPassword: async ({ user, url }) =>
        sendMail({ to: user.email, ...templates.resetPassword(url) }),
    },
    emailVerification: {
      sendOnSignIn: true,
      autoSignInAfterVerification: true,
      sendVerificationEmail: async ({ user, url }) =>
        sendMail({ to: user.email, ...templates.verifyEmail(url) }),
    },
    plugins: [
      magicLink({
        disableSignUp: true,
        expiresIn: 15 * 60,
        storeToken: "hashed",
        sendMagicLink: async ({ email, url }) =>
          sendMail({ to: email, ...templates.magicLink(url) }),
      }),
      twoFactor({ issuer: "Commerce Platform" }),
      jwt({
        // Tokens only from `GET /api/auth/token`, which enforces `staffClaims`.
        disableSettingJwtHeader: true,
        jwks: { keyPairConfig: { alg: "EdDSA", crv: "Ed25519" } },
        jwt: {
          issuer: cfg.issuer,
          audience: AUDIENCE,
          expirationTime: "5m",
          definePayload: staffClaims,
        },
      }),
    ],
  });
}

export type Auth = ReturnType<typeof createAuth>;
