import { readFile } from "node:fs/promises";
import { Hono } from "hono";
import { packetaRoutes } from "./packeta.ts";

/**
 * Local stand-ins for third-party APIs (Packeta, PPL, ČNB, bank, ad platforms, DNS).
 * The real mock endpoints arrive with the work packages that call them.
 */
export const app = new Hono();

app.get("/healthz", (c) => c.json({ status: "ok" }));

packetaRoutes(app);

/**
 * DNS TXT stub for custom-domain verification (spec A29). Tests and operators publish records
 * with `PUT /dns/txt`; `api admin verify-domain` reads them with `GET /dns/txt?name=`.
 * In-memory: records vanish on restart, like a fresh zone.
 */
const txt = new Map<string, string[]>();
const MAX_NAMES = 10_000;

function dnsName(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const name = raw.trim().toLowerCase().replace(/\.$/, "");
  return name.length > 0 && name.length <= 253 && /^[a-z0-9_.-]+$/.test(name) ? name : null;
}

app.get("/dns/txt", (c) => {
  const name = dnsName(c.req.query("name"));
  if (!name) return c.json({ error: "invalid name" }, 400);
  return c.json({ name, records: txt.get(name) ?? [] });
});

app.put("/dns/txt", async (c) => {
  const body: unknown = await c.req.json().catch(() => null);
  const record = typeof body === "object" && body !== null ? (body as Record<string, unknown>) : {};
  const name = dnsName(record.name);
  const records = record.records;
  const valid =
    Array.isArray(records) &&
    records.length <= 20 &&
    records.every((r) => typeof r === "string" && r.length <= 255);
  if (!name || !valid) return c.json({ error: "expected { name, records: string[] }" }, 400);
  if (!txt.has(name) && txt.size >= MAX_NAMES) return c.json({ error: "zone full" }, 507);
  txt.set(name, records as string[]);
  return c.json({ name, records });
});

/**
 * Demo merchant feed host (WP13a): the old shop's Heureka/Google feeds and product photos, so a
 * URL import runs end to end locally. The worker's SSRF-safe client reaches this host only via
 * the dev allowlist (`SAFE_FETCH_ALLOW_HOSTS=mocks`, A21). Fixed file names only.
 */
const FIXTURES = new URL("../../../fixtures/", import.meta.url);
const FEEDS = new Set(["heureka-demo.xml", "google-demo.xml"]);

app.get("/feeds/:name", async (c) => {
  const name = c.req.param("name");
  if (!FEEDS.has(name)) return c.text("Not found", 404);
  const body = await readFile(new URL(`feeds/${name}`, FIXTURES));
  return c.body(body, 200, { "content-type": "application/xml; charset=utf-8" });
});

app.get("/images/demo/:name", async (c) => {
  const name = c.req.param("name");
  if (!/^[a-z0-9-]{1,64}\.jpg$/.test(name)) return c.text("Not found", 404);
  try {
    const body = await readFile(new URL(`images/demo/${name}`, FIXTURES));
    return c.body(body, 200, { "content-type": "image/jpeg" });
  } catch {
    return c.text("Not found", 404);
  }
});
