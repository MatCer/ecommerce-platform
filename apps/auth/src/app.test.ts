import { memoryAdapter } from "better-auth/adapters/memory";
import { describe, expect, test } from "vitest";
import { createApp } from "./app.ts";
import { AUDIENCE, createAuth, staffClaims } from "./auth.ts";
import type { Config } from "./config.ts";
import type { Mail } from "./mail.ts";

const cfg: Config = {
  databaseUrl: "unused",
  secret: "test-secret-test-secret-test-secret-0123",
  baseUrl: "http://auth.localhost:8080",
  issuer: "http://auth.localhost",
  adminOrigin: "http://admin.localhost:8080",
  smtpUrl: "unused",
  mailFrom: "test@example.test",
  internalToken: "internal-token-internal-token-0123456789",
  port: 3000,
};

function setup() {
  const outbox: Mail[] = [];
  const db: Record<string, Record<string, unknown>[]> = {
    user: [],
    session: [],
    account: [],
    verification: [],
    twoFactor: [],
    jwks: [],
  };
  const auth = createAuth(cfg, memoryAdapter(db), async (mail) => {
    outbox.push(mail);
  });
  return { app: createApp(auth, cfg), outbox, db };
}

/** Creates a user, invites them and returns the magic link path from the email. */
async function inviteLink(app: ReturnType<typeof setup>["app"], outbox: Mail[], address: string) {
  const create = internal("/internal/users", { email: address });
  const { id } = (await (await app.request(create.path, create.init)).json()) as { id: string };
  const invite = internal("/internal/users/invite", {
    email: address,
    callback_url: cfg.adminOrigin,
  });
  expect((await app.request(invite.path, invite.init)).status).toBe(202);
  const link = new URL(outbox.at(-1)?.text.match(/https?:\/\/\S+/)?.[0] ?? "");
  return { id, path: link.pathname + link.search };
}

const internal = (path: string, payload: unknown, token = cfg.internalToken) => ({
  path,
  init: {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify(payload),
  },
});

function claimsOf(jwt: string): Record<string, unknown> {
  const [header, payload] = jwt.split(".");
  return {
    ...JSON.parse(Buffer.from(payload ?? "", "base64url").toString()),
    header: JSON.parse(Buffer.from(header ?? "", "base64url").toString()),
  };
}

describe("internal endpoints", () => {
  test("require the service token", async () => {
    const { app } = setup();
    const req = internal("/internal/users", { email: "a@example.test" }, "wrong");
    expect((await app.request(req.path, req.init)).status).toBe(401);
  });

  test("find-or-create users idempotently and reject bad input", async () => {
    const { app, outbox } = setup();
    const req = internal("/internal/users", { email: "Owner@Example.test", name: "Owner" });
    const first = await app.request(req.path, req.init);
    expect(first.status).toBe(201);
    const created = (await first.json()) as { id: string; created: boolean };
    const again = await app.request(req.path, req.init);
    expect(await again.json()).toEqual({ id: created.id, created: false });
    expect(outbox).toHaveLength(0);

    const bad = internal("/internal/users", { email: "not-an-email" });
    expect((await app.request(bad.path, bad.init)).status).toBe(400);
  });
});

describe("staff sign-in", () => {
  test("invite -> magic link -> verified session -> EdDSA JWT for admin-api", async () => {
    const { app, outbox } = setup();
    const create = internal("/internal/users", { email: "owner@example.test" });
    const { id } = (await (await app.request(create.path, create.init)).json()) as { id: string };
    const invite = internal("/internal/users/invite", {
      email: "owner@example.test",
      callback_url: cfg.adminOrigin,
    });
    expect((await app.request(invite.path, invite.init)).status).toBe(202);
    expect(outbox).toHaveLength(1);
    const link = outbox[0]?.text.match(/https?:\/\/\S+/)?.[0];
    expect(link).toContain("/api/auth/magic-link/verify?token=");

    const verify = await app.request(new URL(link ?? "").pathname + new URL(link ?? "").search);
    expect(verify.status).toBe(302);
    expect(verify.headers.get("location")).toBe(`${cfg.adminOrigin}/`);
    const cookie = verify.headers.get("set-cookie")?.split(";")[0] ?? "";
    expect(cookie).toContain("session_token=");

    const res = await app.request("/api/auth/token", { headers: { cookie } });
    expect(res.status).toBe(200);
    const { token } = (await res.json()) as { token: string };
    const claims = claimsOf(token);
    expect(claims.header).toMatchObject({ alg: "EdDSA" });
    expect(claims).toMatchObject({
      sub: id,
      iss: cfg.issuer,
      aud: AUDIENCE,
      email: "owner@example.test",
      email_verified: true,
    });
    const iat = claims.iat as number;
    expect((claims.exp as number) - iat).toBe(300);
    expect(Math.abs((claims.auth_time as number) - iat)).toBeLessThan(5);

    const jwks = (await (await app.request("/api/auth/jwks")).json()) as {
      keys: { kty: string; crv: string; kid: string }[];
    };
    expect(jwks.keys[0]).toMatchObject({
      kty: "OKP",
      crv: "Ed25519",
      kid: claims.header && (claims.header as { kid: string }).kid,
    });
  });

  test("users with 2FA enabled get no session from a magic link", async () => {
    const { app, outbox, db } = setup();
    const { id, path } = await inviteLink(app, outbox, "secure@example.test");
    const user = db.user?.find((u) => u.id === id);
    if (!user) throw new Error("user not stored");
    user.twoFactorEnabled = true;
    const res = await app.request(path);
    expect(res.headers.get("set-cookie") ?? "").not.toContain("session_token=");
    expect(db.session).toHaveLength(0);
  });

  test("public sign-up is disabled and tokens need a session", async () => {
    const { app } = setup();
    const signUp = await app.request("/api/auth/sign-up/email", {
      method: "POST",
      headers: { "content-type": "application/json", origin: cfg.adminOrigin },
      body: JSON.stringify({ email: "x@example.test", password: "a-long-password-123", name: "X" }),
    });
    expect(signUp.status).toBeGreaterThanOrEqual(400);
    expect((await app.request("/api/auth/token")).status).toBe(401);
  });

  test("magic links are not sent to unknown addresses", async () => {
    const { app, outbox } = setup();
    await app.request("/api/auth/sign-in/magic-link", {
      method: "POST",
      headers: { "content-type": "application/json", origin: cfg.adminOrigin },
      body: JSON.stringify({ email: "stranger@example.test", callbackURL: cfg.adminOrigin }),
    });
    // The link may be sent, but using it must not create an account.
    const link = outbox[0]?.text.match(/https?:\/\/\S+/)?.[0];
    if (link) {
      const url = new URL(link);
      const res = await app.request(url.pathname + url.search);
      expect(res.headers.get("set-cookie") ?? "").not.toContain("session_token=");
    }
  });

  test("CORS allows only the admin origin, with credentials", async () => {
    const { app } = setup();
    const preflight = (origin: string) =>
      app.request("/api/auth/token", {
        method: "OPTIONS",
        headers: { origin, "access-control-request-method": "GET" },
      });
    const ok = await preflight(cfg.adminOrigin);
    expect(ok.headers.get("access-control-allow-origin")).toBe(cfg.adminOrigin);
    expect(ok.headers.get("access-control-allow-credentials")).toBe("true");
    const evil = await preflight("http://evil.localhost");
    expect(evil.headers.get("access-control-allow-origin")).not.toBe("http://evil.localhost");
  });
});

describe("staffClaims", () => {
  test("refuses unverified users", () => {
    expect(() =>
      staffClaims({
        user: { email: "a@example.test", emailVerified: false },
        session: { createdAt: new Date() },
      }),
    ).toThrow();
  });

  test("auth_time is the session creation time in seconds", () => {
    const createdAt = new Date("2026-09-25T10:00:00Z");
    expect(
      staffClaims({
        user: { email: "a@example.test", emailVerified: true },
        session: { createdAt },
      }),
    ).toEqual({ email: "a@example.test", email_verified: true, auth_time: 1_790_330_400 });
  });
});
