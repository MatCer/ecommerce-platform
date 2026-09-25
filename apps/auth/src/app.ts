import { Hono } from "hono";
import { bearerAuth } from "hono/bearer-auth";
import { cors } from "hono/cors";
import { type Auth, linkCapture } from "./auth.ts";
import type { Config } from "./config.ts";

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function email(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const value = raw.trim().toLowerCase();
  return value.length <= 254 && EMAIL.test(value) ? value : null;
}

async function body(req: Request): Promise<Record<string, unknown>> {
  const parsed: unknown = await req.json().catch(() => null);
  return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, unknown>) : {};
}

export function createApp(auth: Auth, cfg: Pick<Config, "adminOrigin" | "internalToken">) {
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
  app.on(["GET", "POST"], "/api/auth/*", (c) => auth.handler(c.req.raw));

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
