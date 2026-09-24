import { Hono } from "hono";

/**
 * Local stand-ins for third-party APIs (Packeta, PPL, ČNB, bank, ad platforms).
 * The real mock endpoints arrive with the work packages that call them.
 */
export const app = new Hono();

app.get("/healthz", (c) => c.json({ status: "ok" }));
