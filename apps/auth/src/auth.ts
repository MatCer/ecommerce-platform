import { AsyncLocalStorage } from "node:async_hooks";
import { type BetterAuthOptions, betterAuth } from "better-auth";
import { APIError } from "better-auth/api";
import { jwt, magicLink, twoFactor } from "better-auth/plugins";
import type { Config } from "./config.ts";
import { type Mailer, templates } from "./mail.ts";

/**
 * Set around a magic-link request whose link the caller emails itself (the platform's mail
 * pipeline, WP9): the link is stored here instead of being sent.
 */
export const linkCapture = new AsyncLocalStorage<{ url?: string }>();

/** The `aud` of staff JWTs, verified by the Rust Admin API (spec A9). */
export const AUDIENCE = "admin-api";

interface SessionLike {
  user: { email: string; emailVerified: boolean };
  session: { createdAt: Date | string };
}

/**
 * One-factor sign-in paths. Better Auth's two-factor plugin only challenges password sign-in,
 * so a user with 2FA enabled must not get a session from these (spec A9: tokens only after
 * 2FA). They sign in with password + TOTP instead.
 */
export const ONE_FACTOR_SESSION_PATHS = new Set(["/magic-link/verify", "/verify-email"]);

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
    // Rate limits are per client IP (sign-in, sign-up, password/email change: 3 per 10 s; the
    // rest 100 per minute). The IP comes only from the header the reverse proxy sets: without
    // it every request would share one bucket.
    advanced: { ipAddress: { ipAddressHeaders: [cfg.clientIpHeader] } },
    rateLimit: { enabled: true, window: 60, max: 100 },
    databaseHooks: {
      user: {
        update: {
          // Turning 2FA on ends every existing session (they were created with one factor);
          // the enrolling device gets a fresh session from the two-factor plugin right after.
          before: async (data, ctx) => {
            const userId = ctx?.context.session?.user.id;
            if (data.twoFactorEnabled === true && ctx && userId) {
              await ctx.context.internalAdapter.deleteUserSessions(userId);
            }
          },
        },
      },
      session: {
        create: {
          before: async (session, ctx) => {
            if (!ctx || !ONE_FACTOR_SESSION_PATHS.has(ctx.path)) return;
            const user = await ctx.context.internalAdapter.findUserById(session.userId);
            // Added to the user model by the two-factor plugin.
            if ((user as { twoFactorEnabled?: boolean } | null)?.twoFactorEnabled) {
              throw new APIError("FORBIDDEN", {
                message: "Two-factor authentication is enabled: sign in with password and code",
              });
            }
          },
        },
      },
    },
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
        sendMagicLink: async ({ email, url }) => {
          const capture = linkCapture.getStore();
          if (capture) capture.url = url;
          else await sendMail({ to: email, ...templates.magicLink(url) });
        },
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
