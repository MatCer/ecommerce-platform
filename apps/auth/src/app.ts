import { createHmac, timingSafeEqual } from "node:crypto";
import { Hono } from "hono";
import { bearerAuth } from "hono/bearer-auth";
import { cors } from "hono/cors";
import { type Auth, linkCapture } from "./auth.ts";
import type { Config } from "./config.ts";

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const RATE_ID = /^([0-9a-f]{24})\.([0-9a-f]{64})$/;

function signedRateIp(header: string | null, secret: string | undefined): string | null {
  if (!secret || !header) return null;
  const match = RATE_ID.exec(header);
  if (!match) return null;
  const [, id, mac] = match;
  if (!id || !mac) return null;
  const expected = createHmac("sha256", secret).update(id).digest();
  const supplied = Buffer.from(mac, "hex");
  if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return null;
  // A valid test context gets a stable synthetic IP. The signature, not the header text,
  // authorizes the separate bucket; ordinary and forged requests use Caddy's peer IP.
  return `10.215.${expected[0]}.${expected[1]}`;
}

function email(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const value = raw.trim().toLowerCase();
  return value.length <= 254 && EMAIL.test(value) ? value : null;
}

async function body(req: Request): Promise<Record<string, unknown>> {
  const parsed: unknown = await req.json().catch(() => null);
  return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, unknown>) : {};
}

export function createApp(
  auth: Auth,
  cfg: Pick<Config, "adminOrigin" | "internalToken" | "clientIpHeader" | "e2eRateSecret">,
) {
  const app = new Hono();

  app.get("/healthz", (c) => c.json({ status: "ok" }));

  // The admin SPA calls Better Auth cross-origin with its session cookie (spec A9).
  app.use(
    "/api/auth/*",
    cors({
      origin: cfg.adminOrigin,
      credentials: true,
      allowMethods: ["GET", "POST", "OPTIONS"],
      allowHeaders: ["Content-Type", "Authorization"],
      maxAge: 600,
    }),
  );
  app.on(["GET", "POST"], "/api/auth/*", (c) => {
    const ip = signedRateIp(c.req.header("x-e2e-rate-key") ?? null, cfg.e2eRateSecret);
    if (!ip) return auth.handler(c.req.raw);
    const headers = new Headers(c.req.raw.headers);
    headers.set(cfg.clientIpHeader, ip);
    headers.delete("x-e2e-rate-key");
    return auth.handler(new Request(c.req.raw, { headers }));
  });

  // Superadmin endpoints for the API's CLI. Not routed by Caddy; service token required.
  const internal = new Hono();
  internal.use(bearerAuth({ token: cfg.internalToken }));

  /** Find or create a staff user by email (idempotent). No email is sent. */
  internal.post("/users", async (c) => {
    const input = await body(c.req.raw);
    const address = email(input.email);
    const name = typeof input.name === "string" ? input.name.trim().slice(0, 200) : "";
    if (!address) return c.json({ error: "invalid email" }, 400);
    const ctx = await auth.$context;
    const existing = await ctx.internalAdapter.findUserByEmail(address);
    if (existing) return c.json({ id: existing.user.id, created: false });
    const user = await ctx.internalAdapter.createUser(
      { email: address, name: name || address, emailVerified: false },
      { method: "admin" },
    );
    return c.json({ id: user.id, created: true }, 201);
  });

  /**
   * Magic link for an existing user; signing in with it also verifies the address. Emailed
   * here, or with `deliver: false` returned as `{url}` for the platform's mail pipeline (the
   * staff invitation goes through the outbox, WP9).
   */
  internal.post("/users/invite", async (c) => {
    const input = await body(c.req.raw);
    const address = email(input.email);
    const callbackURL = typeof input.callback_url === "string" ? input.callback_url : "/";
    if (!address) return c.json({ error: "invalid email" }, 400);
    const request = () =>
      auth.api.signInMagicLink({ body: { email: address, callbackURL }, headers: new Headers() });
    if (input.deliver === false) {
      const capture: { url?: string } = {};
      await linkCapture.run(capture, request);
      if (!capture.url) return c.json({ error: "no link issued" }, 500);
      return c.json({ url: capture.url });
    }
    await request();
    return c.json({ invited: address }, 202);
  });

  app.route("/internal", internal);
  return app;
}
