import { Hono } from "hono";
import { media, storefront } from "./storefront/app.ts";

/**
 * Local stand-ins for third-party APIs (Packeta, PPL, ČNB, bank, ad platforms).
 * The real mock endpoints arrive with the work packages that call them.
 *
 * Also hosts the stub Storefront API + fixture media used by the edge until WP6 (spec WP2).
 */
export const app = new Hono();

app.get("/healthz", (c) => c.json({ status: "ok" }));
app.route("/storefront/v1", storefront);
app.route("/", media);
