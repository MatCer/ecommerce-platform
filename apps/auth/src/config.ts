/** Environment for the auth service. Every value is validated at startup (fail fast). */
export interface Config {
  /** Postgres URL of the `auth_service` role; its search_path is the `auth` schema. */
  databaseUrl: string;
  /** Signs cookies and encrypts the JWT private keys at rest. */
  secret: string;
  /** Public base URL of this service, e.g. `http://auth.localhost:8080`. */
  baseUrl: string;
  /** `iss` of staff JWTs (spec A9), independent of the port the stack runs on. */
  issuer: string;
  /** The admin SPA origin: the only trusted origin (CORS with credentials, redirects). */
  adminOrigin: string;
  /** SMTP server for magic links and verification mail, e.g. `smtp://mailpit:1025`. */
  smtpUrl: string;
  mailFrom: string;
  /** Bearer token the API's superadmin CLI uses for `/internal/*`. */
  internalToken: string;
  /**
   * The request header carrying the client IP, set (overwritten) by the trusted reverse proxy
   * in front of this service: Caddy's `X-Real-IP` locally, e.g. `cf-connecting-ip` behind
   * Cloudflare. Nothing else is trusted, so a client-supplied `X-Forwarded-For` chain cannot
   * pick its own rate-limit bucket.
   */
  clientIpHeader: string;
  /**
   * Sign-in requests (password, magic link and its verification) per client IP and minute
   * (`AUTH_SIGNIN_RATE_MAX`, default 10). The local stack raises it: every e2e browser shares
   * one IP there.
   */
  signInRateMax: number;
  port: number;
}

type Env = Record<string, string | undefined>;

function required(env: Env, name: string): string {
  const value = env[name]?.trim();
  if (!value) throw new Error(`missing required environment variable ${name}`);
  return value;
}

function secret(env: Env, name: string): string {
  const value = required(env, name);
  if (value.length < 32) throw new Error(`${name} must be at least 32 characters`);
  return value;
}

function origin(env: Env, name: string): string {
  const raw = required(env, name);
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`${name} is not a URL`);
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(`${name} must be http(s)`);
  }
  return url.origin;
}

function clientIpHeader(raw: string | undefined): string {
  const value = raw?.trim().toLowerCase() || "x-real-ip";
  if (!/^[a-z0-9-]{1,64}$/.test(value)) throw new Error("AUTH_CLIENT_IP_HEADER is invalid");
  return value;
}

function signInRateMax(raw: string | undefined): number {
  const value = Number(raw?.trim() || 10);
  if (!Number.isInteger(value) || value < 1 || value > 1000) {
    throw new Error("AUTH_SIGNIN_RATE_MAX must be 1-1000");
  }
  return value;
}

export function loadConfig(env: Env = process.env): Config {
  const port = Number(env.PORT ?? 3000);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) throw new Error("PORT is invalid");
  return {
    databaseUrl: required(env, "DATABASE_URL"),
    secret: secret(env, "BETTER_AUTH_SECRET"),
    baseUrl: origin(env, "BETTER_AUTH_URL"),
    issuer: env.AUTH_ISSUER?.trim() || "http://auth.localhost",
    adminOrigin: origin(env, "ADMIN_ORIGIN"),
    smtpUrl: required(env, "SMTP_URL"),
    mailFrom: env.MAIL_FROM?.trim() || "Commerce Platform <no-reply@platform.localhost>",
    internalToken: secret(env, "AUTH_INTERNAL_TOKEN"),
    clientIpHeader: clientIpHeader(env.AUTH_CLIENT_IP_HEADER),
    signInRateMax: signInRateMax(env.AUTH_SIGNIN_RATE_MAX),
    port,
  };
}
