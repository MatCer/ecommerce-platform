import { createHmac } from "node:crypto";
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
  clientIpHeader: "x-real-ip",
  signInRateMax: 100,
  port: 3000,
};

function setup(config: Config = cfg) {
  const outbox: Mail[] = [];
  const db: Record<string, Record<string, unknown>[]> = {
    user: [],
    session: [],
    account: [],
    verification: [],
    twoFactor: [],
    jwks: [],
  };
  const auth = createAuth(config, memoryAdapter(db), async (mail) => {
    outbox.push(mail);
  });
  return { app: createApp(auth, config), outbox, db, auth };
}

/** RFC 6238 TOTP (SHA-1, 6 digits, 30 s) for a base32 secret, as an authenticator app. */
function totp(base32: string): string {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of base32.replace(/=+$/, "").toUpperCase()) {
    bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  }
  const key = Buffer.from((bits.match(/.{8}/g) ?? []).map((b) => Number.parseInt(b, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30_000)));
  const mac = createHmac("sha1", key).update(counter).digest();
  const offset = (mac.at(-1) ?? 0) & 0xf;
  return String((mac.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).padStart(6, "0");
}

async function signInWithLink(app: ReturnType<typeof setup>["app"], path: string) {
  const res = await app.request(path);
  return res.headers.get("set-cookie")?.split(";")[0] ?? "";
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

  test("invite with deliver=false returns the link instead of mailing it", async () => {
    const { app, outbox } = setup();
    const create = internal("/internal/users", { email: "staff@example.test" });
    await app.request(create.path, create.init);
    const invite = internal("/internal/users/invite", {
      email: "staff@example.test",
      callback_url: cfg.adminOrigin,
      deliver: false,
    });
    const res = await app.request(invite.path, invite.init);
    expect(res.status).toBe(200);
    const { url } = (await res.json()) as { url: string };
    expect(url).toContain("/api/auth/magic-link/verify?token=");
    expect(outbox).toHaveLength(0);
    // The returned link signs in like an emailed one.
    const verify = await app.request(new URL(url).pathname + new URL(url).search);
    expect(verify.status).toBe(302);
    expect(verify.headers.get("set-cookie")).toContain("session_token=");
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

  test("enabling 2FA ends the sessions created with one factor", async () => {
    const { app, outbox, auth } = setup();
    const address = "twofa@example.test";
    const first = await inviteLink(app, outbox, address);
    const enrolling = await signInWithLink(app, first.path);
    const second = await inviteLink(app, outbox, address);
    const other = await signInWithLink(app, second.path);
    expect((await app.request("/api/auth/token", { headers: { cookie: other } })).status).toBe(200);

    const password = "a-long-password-for-2fa";
    const ctx = await auth.$context;
    await ctx.internalAdapter.linkAccount({
      providerId: "credential",
      accountId: first.id,
      userId: first.id,
      password: await ctx.password.hash(password),
    });
    const post = (path: string, cookie: string, payload: unknown) =>
      app.request(path, {
        method: "POST",
        headers: { cookie, origin: cfg.adminOrigin, "content-type": "application/json" },
        body: JSON.stringify(payload),
      });
    const enable = await post("/api/auth/two-factor/enable", enrolling, { password });
    expect(enable.status).toBe(200);
    const { totpURI } = (await enable.json()) as { totpURI: string };
    const secret = new URL(totpURI).searchParams.get("secret") ?? "";
    const verified = await post("/api/auth/two-factor/verify-totp", enrolling, {
      code: totp(secret),
    });
    expect(verified.status).toBe(200);
    const fresh = verified.headers.get("set-cookie")?.split(";")[0] ?? "";

    // The other one-factor session is gone; the enrolling device holds a new one.
    expect((await app.request("/api/auth/token", { headers: { cookie: other } })).status).toBe(401);
    expect((await app.request("/api/auth/token", { headers: { cookie: fresh } })).status).toBe(200);
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

describe("rate limits", () => {
  const signIn = (app: ReturnType<typeof setup>["app"], headers: Record<string, string>) =>
    app.request("/api/auth/sign-in/email", {
      method: "POST",
      headers: { "content-type": "application/json", origin: cfg.adminOrigin, ...headers },
      body: JSON.stringify({ email: "nobody@example.test", password: "wrong-password-123" }),
    });

  test("are per client IP from the proxy's header; client X-Forwarded-For is ignored", async () => {
    const { app } = setup({ ...cfg, signInRateMax: 3 });
    const a = { "x-real-ip": "203.0.113.7" };
    for (let i = 0; i < 3; i++) expect((await signIn(app, a)).status).toBe(401);
    expect((await signIn(app, a)).status).toBe(429);
    // A spoofed forwarded chain does not open a new bucket...
    const spoof = { ...a, "x-forwarded-for": `198.51.100.${Date.now() % 250}` };
    expect((await signIn(app, spoof)).status).toBe(429);
    // ...but another client (another proxy-set IP) has its own.
    expect((await signIn(app, { "x-real-ip": "203.0.113.8" })).status).toBe(401);
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
