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
    port,
  };
}
