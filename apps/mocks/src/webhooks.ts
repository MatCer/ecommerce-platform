import { createHmac, timingSafeEqual } from "node:crypto";
import type { Hono } from "hono";

/**
 * A webhook receiver for the platform's outbound webhooks (spec §8.5, WP14). Each bucket
 * records what it receives; e2e tests configure it and read it back:
 * - `POST /webhooks/:bucket`: a delivery. Recorded with its headers and raw body; when the
 *   bucket has a secret, `signature_valid` says whether `X-Signature: t=<ts>,v1=<hex>` is the
 *   HMAC-SHA256 of `<ts>.<body>` under it. Answers the bucket's configured status (default 200).
 * - `PUT /webhooks/:bucket/config` `{ status?, secret? }`
 * - `GET /webhooks/:bucket` → `{ config, deliveries }`
 * - `DELETE /webhooks/:bucket` resets it.
 * In-memory and bounded; buckets vanish on restart.
 */
interface Received {
  received_at: string;
  event: string | null;
  webhook_id: string | null;
  signature_valid: boolean | null;
  body: unknown;
}
interface Bucket {
  status: number;
  secret: string | null;
  deliveries: Received[];
}

const buckets = new Map<string, Bucket>();
const MAX_BUCKETS = 1000;
const MAX_DELIVERIES = 200;
const NAME = /^[A-Za-z0-9_-]{1,64}$/;

function bucket(name: string): Bucket | null {
  if (!NAME.test(name)) return null;
  let b = buckets.get(name);
  if (!b) {
    if (buckets.size >= MAX_BUCKETS) return null;
    b = { status: 200, secret: null, deliveries: [] };
    buckets.set(name, b);
  }
  return b;
}

/** `t=<ts>,v1=<hex>` over `<ts>.<raw body>` with the whole secret as the HMAC key. */
export function verifySignature(header: string | undefined, raw: string, secret: string): boolean {
  const parts = Object.fromEntries(
    (header ?? "").split(",").map((p) => p.split("=", 2) as [string, string]),
  );
  const t = parts.t;
  const v1 = parts.v1;
  if (!t || !v1 || !/^\d+$/.test(t) || !/^[0-9a-f]{64}$/.test(v1)) return false;
  const expected = createHmac("sha256", secret).update(`${t}.${raw}`).digest();
  return timingSafeEqual(expected, Buffer.from(v1, "hex"));
}

export function webhookRoutes(app: Hono) {
  app.post("/webhooks/:bucket", async (c) => {
    const b = bucket(c.req.param("bucket"));
    if (!b) return c.json({ error: "invalid bucket" }, 400);
    const raw = await c.req.text();
    let body: unknown = null;
    try {
      body = JSON.parse(raw);
    } catch {
      body = raw.slice(0, 1000);
    }
    b.deliveries.push({
      received_at: new Date().toISOString(),
      event: c.req.header("x-webhook-event") ?? null,
      webhook_id: c.req.header("x-webhook-id") ?? null,
      signature_valid: b.secret ? verifySignature(c.req.header("x-signature"), raw, b.secret) : null,
      body,
    });
    if (b.deliveries.length > MAX_DELIVERIES) b.deliveries.shift();
    return c.body(null, b.status as 200);
  });

  app.put("/webhooks/:bucket/config", async (c) => {
    const b = bucket(c.req.param("bucket"));
    if (!b) return c.json({ error: "invalid bucket" }, 400);
    const cfg: unknown = await c.req.json().catch(() => null);
    const input = typeof cfg === "object" && cfg !== null ? (cfg as Record<string, unknown>) : {};
    if (input.status !== undefined) {
      if (typeof input.status !== "number" || input.status < 200 || input.status > 599)
        return c.json({ error: "status must be 200-599" }, 400);
      b.status = input.status;
    }
    if (input.secret !== undefined) {
      if (input.secret !== null && typeof input.secret !== "string")
        return c.json({ error: "secret must be a string" }, 400);
      b.secret = input.secret;
    }
    return c.json({ status: b.status, has_secret: b.secret !== null });
  });

  app.get("/webhooks/:bucket", (c) => {
    const b = bucket(c.req.param("bucket"));
    if (!b) return c.json({ error: "invalid bucket" }, 400);
    return c.json({
      config: { status: b.status, has_secret: b.secret !== null },
      deliveries: b.deliveries,
    });
  });

  app.delete("/webhooks/:bucket", (c) => {
    buckets.delete(c.req.param("bucket"));
    return c.body(null, 204);
  });
}
