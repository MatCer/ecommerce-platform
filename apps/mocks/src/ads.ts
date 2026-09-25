import type { Context, Hono } from "hono";

/**
 * Ad-platform stand-ins (WP20). The worker and API reach them through
 * `AD_PLATFORMS_BASE_URL=http://mocks:4010/ads` (dev only): every vendor path lives under
 * `/ads/<platform>` exactly as on the vendor's host. Each endpoint checks the request shape
 * the vendor documents (including the hashed identifiers: lowercase 64-hex SHA-256) and
 * answers like the vendor; a malformed request gets the vendor's 400.
 *
 * - Meta CAPI: `POST /ads/meta/:version/:pixel/events`, `GET /ads/meta/:version/:pixel`
 * - GA4 MP: `POST /ads/ga4/mp/collect` (204), `POST /ads/ga4/debug/mp/collect`
 *   (`validationMessages`)
 * - Google: `POST /ads/google/token` (OAuth refresh), `POST /ads/google/v1/events:ingest`
 *   (Data Manager API)
 * - Seznam SEM: `POST /ads/sklik/rtgconv`
 *
 * Test controls, per platform (`meta`, `ga4`, `google`, `sklik`):
 * - `GET /ads/:platform/requests` → `{ config, requests }` (secrets masked)
 * - `DELETE /ads/:platform/requests` resets requests and config
 * - `PUT /ads/:platform/config` `{ status?, fail_times? }`: answer `status` (default 200) to the
 *   next `fail_times` requests (every request when `fail_times` is null), then succeed again.
 * In-memory and bounded.
 */

const PLATFORMS = ["meta", "ga4", "google", "sklik"] as const;
type Platform = (typeof PLATFORMS)[number];

interface Recorded {
  received_at: string;
  path: string;
  /** The query string with secret values masked. */
  query: Record<string, string>;
  authorized: boolean;
  status: number;
  errors: string[];
  body: unknown;
}
interface State {
  status: number;
  fail_times: number | null;
  requests: Recorded[];
}

const MAX_REQUESTS = 500;
const SECRET_PARAMS = new Set(["api_secret", "access_token", "client_secret", "refresh_token"]);
const HASH = /^[0-9a-f]{64}$/;
const states = new Map<Platform, State>();

function state(p: Platform): State {
  let s = states.get(p);
  if (!s) {
    s = { status: 200, fail_times: null, requests: [] };
    states.set(p, s);
  }
  return s;
}

const isPlatform = (p: string): p is Platform => (PLATFORMS as readonly string[]).includes(p);
const obj = (v: unknown): Record<string, unknown> | null =>
  typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

function masked(c: Context): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of new URL(c.req.url).searchParams) out[k] = SECRET_PARAMS.has(k) ? "***" : v;
  return out;
}

/** The configured failure for this request, if any (consumes one of `fail_times`). */
function injected(s: State): number | null {
  if (s.status >= 200 && s.status < 300) return null;
  if (s.fail_times === null) return s.status;
  if (s.fail_times > 0) {
    s.fail_times -= 1;
    return s.status;
  }
  return null;
}

async function jsonBody(c: Context): Promise<unknown> {
  const raw = await c.req.text();
  try {
    return JSON.parse(raw);
  } catch {
    return raw.slice(0, 1000);
  }
}

function record(p: Platform, c: Context, body: unknown, status: number, errors: string[]) {
  const s = state(p);
  s.requests.push({
    received_at: new Date().toISOString(),
    path: new URL(c.req.url).pathname,
    query: masked(c),
    authorized: (c.req.header("authorization") ?? "").startsWith("Bearer "),
    status,
    errors,
    body: redact(body),
  });
  if (s.requests.length > MAX_REQUESTS) s.requests.shift();
}

/** Recorded bodies keep everything but secrets. */
function redact(body: unknown): unknown {
  const o = obj(body);
  if (!o) return body;
  return Object.fromEntries(
    Object.entries(o).map(([k, v]) => [k, SECRET_PARAMS.has(k) ? "***" : v]),
  );
}

// --- validation --------------------------------------------------------------------------------

function hashes(v: unknown, field: string, errors: string[], asArray: boolean) {
  if (v === undefined) return;
  const list = asArray ? v : [v];
  if (!Array.isArray(list) || list.some((h) => typeof h !== "string" || !HASH.test(h)))
    errors.push(`${field} must be ${asArray ? "an array of " : ""}lowercase SHA-256 hex`);
}

export function validateMeta(body: unknown): string[] {
  const errors: string[] = [];
  const b = obj(body);
  const data = b?.data;
  if (!Array.isArray(data) || data.length < 1 || data.length > 1000)
    return ["data must be an array of 1-1000 events"];
  if (typeof b?.access_token !== "string" || !b.access_token) errors.push("access_token missing");
  for (const [i, raw] of data.entries()) {
    const e = obj(raw);
    const at = `data[${i}]`;
    if (!e) {
      errors.push(`${at} must be an object`);
      continue;
    }
    if (typeof e.event_name !== "string" || !e.event_name) errors.push(`${at}.event_name missing`);
    if (!Number.isInteger(e.event_time) || (e.event_time as number) > 1e11)
      errors.push(`${at}.event_time must be Unix seconds`);
    if (typeof e.event_id !== "string" || !e.event_id) errors.push(`${at}.event_id missing`);
    if (e.action_source !== "website") errors.push(`${at}.action_source must be website`);
    const u = obj(e.user_data);
    if (!u) {
      errors.push(`${at}.user_data missing`);
      continue;
    }
    if (typeof e.event_source_url !== "string") errors.push(`${at}.event_source_url missing`);
    if (typeof u.client_user_agent !== "string" || !u.client_user_agent)
      errors.push(`${at}.user_data.client_user_agent is required for website events`);
    for (const f of ["em", "ph", "external_id", "country", "fn", "ln", "zp", "ct"])
      hashes(u[f], `${at}.user_data.${f}`, errors, true);
    if ("client_ip_address" in u && typeof u.client_ip_address !== "string")
      errors.push(`${at}.user_data.client_ip_address must be a string`);
  }
  return errors;
}

const GA4_NAME = /^[a-zA-Z][a-zA-Z0-9_]{0,39}$/;

export function validateGa4(query: URLSearchParams, body: unknown): string[] {
  const errors: string[] = [];
  if (!/^G-[A-Z0-9]+$/.test(query.get("measurement_id") ?? ""))
    errors.push("measurement_id query parameter missing");
  if (!query.get("api_secret")) errors.push("api_secret query parameter missing");
  const b = obj(body);
  if (typeof b?.client_id !== "string" || !b.client_id) errors.push("client_id missing");
  const events = b?.events;
  if (!Array.isArray(events) || events.length < 1 || events.length > 25)
    return [...errors, "events must be an array of 1-25 events"];
  for (const [i, raw] of events.entries()) {
    const e = obj(raw);
    if (!e || typeof e.name !== "string" || !GA4_NAME.test(e.name))
      errors.push(`events[${i}].name is invalid`);
    const params = obj(e?.params ?? {});
    if (!params) errors.push(`events[${i}].params must be an object`);
  }
  // Measurement Protocol forbids PII: an email address anywhere is refused here.
  if (JSON.stringify(body).includes("@")) errors.push("the payload contains an email address");
  return errors;
}

export function validateGoogle(body: unknown): string[] {
  const errors: string[] = [];
  const b = obj(body);
  const dest = b?.destinations;
  if (!Array.isArray(dest) || dest.length < 1) errors.push("destinations missing");
  else
    for (const [i, raw] of dest.entries()) {
      const d = obj(raw);
      const acct = obj(d?.operatingAccount);
      if (acct?.accountType !== "GOOGLE_ADS" || !/^\d{10}$/.test(String(acct?.accountId)))
        errors.push(`destinations[${i}].operatingAccount is invalid`);
      if (!/^\d+$/.test(String(d?.productDestinationId)))
        errors.push(`destinations[${i}].productDestinationId is invalid`);
    }
  if (b?.encoding !== "HEX") errors.push("encoding must be HEX");
  const events = b?.events;
  if (!Array.isArray(events) || events.length < 1 || events.length > 2000)
    return [...errors, "events must be an array of 1-2000 events"];
  for (const [i, raw] of events.entries()) {
    const e = obj(raw);
    const at = `events[${i}]`;
    if (typeof e?.eventTimestamp !== "string" || Number.isNaN(Date.parse(e.eventTimestamp)))
      errors.push(`${at}.eventTimestamp must be RFC 3339`);
    const ids = obj(e?.userData)?.userIdentifiers;
    if (!Array.isArray(ids) || ids.length === 0) errors.push(`${at}.userData.userIdentifiers missing`);
    else
      for (const id of ids) {
        const o = obj(id);
        hashes(o?.emailAddress, `${at}.emailAddress`, errors, false);
        hashes(o?.phoneNumber, `${at}.phoneNumber`, errors, false);
      }
  }
  return errors;
}

export function validateSklik(body: unknown): string[] {
  const errors: string[] = [];
  const b = obj(body);
  if (b?.schema_version !== "v2") errors.push("schema_version must be v2");
  if (b?.event_type !== "rtgconv") errors.push("event_type must be rtgconv");
  if (typeof b?.event_name !== "string") errors.push("event_name missing");
  if (!Number.isInteger(b?.event_time) || (b?.event_time as number) < 1e12)
    errors.push("event_time must be Unix milliseconds");
  if (typeof b?.event_url !== "string") errors.push("event_url missing");
  if (b?.event_source !== "web" && b?.event_source !== "app") errors.push("event_source invalid");
  const user = obj(obj(b?.user_ids)?.user_data);
  if (!user) errors.push("user_ids.user_data missing");
  for (const f of ["em", "ph", "fn", "ln"]) hashes(user?.[f], `user_data.${f}`, errors, false);
  const data = obj(b?.event_data);
  if (typeof data?.sem_id !== "string" || !data.sem_id) errors.push("event_data.sem_id missing");
  if (data?.currency !== undefined && data.currency !== "CZK") errors.push("currency must be CZK");
  return errors;
}

// --- routes ------------------------------------------------------------------------------------

const tokens = new Set<string>();

export function adRoutes(app: Hono) {
  const answer = (p: Platform, c: Context, body: unknown, errors: string[], ok: () => Response) => {
    const failure = errors.length ? 400 : injected(state(p));
    const status = failure ?? 200;
    record(p, c, body, status, errors);
    if (errors.length) return c.json({ error: { message: errors.join("; "), code: 100 } }, 400);
    if (failure) return c.json({ error: { message: "injected failure" } }, failure as 500);
    return ok();
  };

  app.post("/ads/meta/:version/:pixel/events", async (c) => {
    const body = await jsonBody(c);
    const errors = /^v\d+\.\d+$/.test(c.req.param("version")) ? validateMeta(body) : ["bad version"];
    return answer("meta", c, body, errors, () =>
      c.json({ events_received: (obj(body)?.data as unknown[]).length, messages: [], fbtrace_id: "mock" }),
    );
  });
  app.get("/ads/meta/:version/:pixel", (c) => {
    const errors = c.req.query("access_token") ? [] : ["access_token missing"];
    return answer("meta", c, null, errors, () => c.json({ id: c.req.param("pixel") }));
  });

  for (const debug of [false, true]) {
    app.post(debug ? "/ads/ga4/debug/mp/collect" : "/ads/ga4/mp/collect", async (c) => {
      const body = await jsonBody(c);
      const errors = validateGa4(new URL(c.req.url).searchParams, body);
      if (debug) {
        // The validation server always answers 200 with its findings.
        record("ga4", c, body, 200, errors);
        return c.json({
          validationMessages: errors.map((description) => ({
            fieldPath: "",
            description,
            validationCode: "VALUE_INVALID",
          })),
        });
      }
      // The real endpoint accepts anything with 204; the mock refuses malformed payloads so
      // tests notice them.
      return answer("ga4", c, body, errors, () => c.body(null, 204));
    });
  }

  app.post("/ads/google/token", async (c) => {
    const form = new URLSearchParams(await c.req.text());
    const errors =
      form.get("grant_type") === "refresh_token" &&
      form.get("client_id") &&
      form.get("client_secret") &&
      form.get("refresh_token") &&
      form.get("refresh_token") !== "invalid"
        ? []
        : ["invalid_grant"];
    record("google", c, Object.fromEntries([...form.keys()].map((k) => [k, "***"])), errors.length ? 400 : 200, errors);
    if (errors.length) return c.json({ error: "invalid_grant" }, 400);
    const token = `ya29.mock-${crypto.randomUUID()}`;
    tokens.add(token);
    return c.json({ access_token: token, expires_in: 3599, token_type: "Bearer" });
  });
  // `events:ingest` is not a Hono parameter pattern: match the literal path.
  app.post("/ads/google/v1/*", async (c) => {
    if (!new URL(c.req.url).pathname.endsWith("/v1/events:ingest")) return c.text("Not found", 404);
    const body = await jsonBody(c);
    const bearer = (c.req.header("authorization") ?? "").replace(/^Bearer /, "");
    if (!tokens.has(bearer)) {
      record("google", c, body, 401, ["unauthenticated"]);
      return c.json({ error: { code: 401, status: "UNAUTHENTICATED" } }, 401);
    }
    return answer("google", c, body, validateGoogle(body), () =>
      c.json({ requestId: crypto.randomUUID() }),
    );
  });

  app.post("/ads/sklik/rtgconv", async (c) => {
    const body = await jsonBody(c);
    return answer("sklik", c, body, validateSklik(body), () => c.json({ status: "ok" }));
  });

  app.get("/ads/:platform/requests", (c) => {
    const p = c.req.param("platform");
    if (!isPlatform(p)) return c.json({ error: "unknown platform" }, 404);
    const s = state(p);
    return c.json({ config: { status: s.status, fail_times: s.fail_times }, requests: s.requests });
  });
  app.delete("/ads/:platform/requests", (c) => {
    const p = c.req.param("platform");
    if (!isPlatform(p)) return c.json({ error: "unknown platform" }, 404);
    states.delete(p);
    return c.body(null, 204);
  });
  app.put("/ads/:platform/config", async (c) => {
    const p = c.req.param("platform");
    if (!isPlatform(p)) return c.json({ error: "unknown platform" }, 404);
    const input = obj(await c.req.json().catch(() => null)) ?? {};
    const s = state(p);
    if (input.status !== undefined) {
      if (typeof input.status !== "number" || input.status < 200 || input.status > 599)
        return c.json({ error: "status must be 200-599" }, 400);
      s.status = input.status;
    }
    if (input.fail_times !== undefined) {
      if (input.fail_times !== null && (!Number.isInteger(input.fail_times) || (input.fail_times as number) < 0))
        return c.json({ error: "fail_times must be a non-negative integer or null" }, 400);
      s.fail_times = input.fail_times as number | null;
    }
    return c.json({ status: s.status, fail_times: s.fail_times });
  });
}
