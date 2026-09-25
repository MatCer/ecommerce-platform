import { Hono } from "hono";

/**
 * Local stand-ins for third-party APIs (Packeta, PPL, ČNB, bank, ad platforms, DNS).
 * The real mock endpoints arrive with the work packages that call them.
 */
export const app = new Hono();

app.get("/healthz", (c) => c.json({ status: "ok" }));

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
