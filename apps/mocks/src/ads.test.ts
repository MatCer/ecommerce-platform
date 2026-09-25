import { createHash } from "node:crypto";
import { beforeEach, expect, test } from "vitest";
import { app } from "./app.ts";

const json = { "content-type": "application/json" };
const sha = (s: string) => createHash("sha256").update(s).digest("hex");
const post = (path: string, body: unknown, headers: Record<string, string> = json) =>
  app.request(path, { method: "POST", headers, body: JSON.stringify(body) });
const requests = async (p: string) =>
  (await (await app.request(`/ads/${p}/requests`)).json()) as {
    requests: { status: number; errors: string[]; query: Record<string, string>; body: unknown }[];
  };

beforeEach(async () => {
  for (const p of ["meta", "ga4", "google", "sklik"])
    await app.request(`/ads/${p}/requests`, { method: "DELETE" });
});

const metaEvent = {
  event_name: "Purchase",
  event_time: 1_790_000_000,
  event_id: "e-1",
  action_source: "website",
  event_source_url: "https://shop.example/",
  user_data: { em: [sha("a@b.cz")], client_user_agent: "Mozilla/5.0" },
};

test("meta validates the CAPI shape and hashed identifiers", async () => {
  const ok = await post("/ads/meta/v26.0/123/events", { data: [metaEvent], access_token: "t" });
  expect(ok.status).toBe(200);
  expect(await ok.json()).toMatchObject({ events_received: 1 });
  const raw = await post("/ads/meta/v26.0/123/events", {
    data: [{ ...metaEvent, user_data: { em: ["a@b.cz"] } }],
    access_token: "t",
  });
  expect(raw.status).toBe(400);
  const ms = await post("/ads/meta/v26.0/123/events", {
    data: [{ ...metaEvent, event_time: 1_790_000_000_000 }],
    access_token: "t",
  });
  expect(ms.status).toBe(400);
  const got = await requests("meta");
  expect(got.requests.map((r) => r.status)).toEqual([200, 400, 400]);
  expect(got.requests[1]?.errors.join()).toContain("em");
  expect(JSON.stringify(got.requests[0]?.body)).not.toContain('"t"'); // the token is masked
  expect((await app.request("/ads/meta/v26.0/123?fields=id&access_token=x")).status).toBe(200);
});

test("ga4 refuses PII and answers the validation server's shape", async () => {
  const q = "?measurement_id=G-TEST123&api_secret=s3cret";
  const event = { client_id: "abc", events: [{ name: "purchase", params: { value: 1 } }] };
  expect((await post(`/ads/ga4/mp/collect${q}`, event)).status).toBe(204);
  const pii = { ...event, events: [{ name: "purchase", params: { email: "a@b.cz" } }] };
  expect((await post(`/ads/ga4/mp/collect${q}`, pii)).status).toBe(400);
  const debug = await post(`/ads/ga4/debug/mp/collect${q}`, { events: [] });
  expect(debug.status).toBe(200);
  const v = (await debug.json()) as { validationMessages: unknown[] };
  expect(v.validationMessages.length).toBeGreaterThan(0);
  expect((await requests("ga4")).requests[0]?.query.api_secret).toBe("***");
});

test("google needs an OAuth token and hashed identifiers", async () => {
  const body = {
    destinations: [
      {
        operatingAccount: { accountType: "GOOGLE_ADS", accountId: "1234567890" },
        productDestinationId: "555",
      },
    ],
    encoding: "HEX",
    events: [
      {
        eventTimestamp: "2026-09-21T14:13:20Z",
        userData: { userIdentifiers: [{ emailAddress: sha("a@b.cz") }] },
      },
    ],
  };
  expect((await post("/ads/google/v1/events:ingest", body)).status).toBe(401);
  const form = { "content-type": "application/x-www-form-urlencoded" };
  const tokenRes = await app.request("/ads/google/token", {
    method: "POST",
    headers: form,
    body: "grant_type=refresh_token&client_id=c&client_secret=s&refresh_token=r",
  });
  const { access_token } = (await tokenRes.json()) as { access_token: string };
  const auth = { ...json, authorization: `Bearer ${access_token}` };
  expect((await post("/ads/google/v1/events:ingest", body, auth)).status).toBe(200);
  const bad = { ...body, encoding: "BASE64" };
  expect((await post("/ads/google/v1/events:ingest", bad, auth)).status).toBe(400);
  const refused = await app.request("/ads/google/token", {
    method: "POST",
    headers: form,
    body: "grant_type=refresh_token&client_id=c&client_secret=s&refresh_token=invalid",
  });
  expect(refused.status).toBe(400);
});

test("sklik checks the SEM S2S shape; failures can be injected", async () => {
  const body = {
    schema_version: "v2",
    event_name: "Purchase",
    event_type: "rtgconv",
    event_time: 1_790_000_000_000,
    event_url: "https://shop.example/",
    event_source: "web",
    user_ids: { user_data: { em: sha("a@b.cz") } },
    event_data: { sem_id: "sem", currency: "CZK", value: 1 },
  };
  expect((await post("/ads/sklik/rtgconv", body)).status).toBe(200);
  expect((await post("/ads/sklik/rtgconv", { ...body, event_data: { sem_id: "sem", currency: "EUR" } })).status).toBe(400);

  await app.request("/ads/sklik/config", {
    method: "PUT",
    headers: json,
    body: JSON.stringify({ status: 503, fail_times: 1 }),
  });
  expect((await post("/ads/sklik/rtgconv", body)).status).toBe(503);
  expect((await post("/ads/sklik/rtgconv", body)).status).toBe(200);
  expect((await requests("sklik")).requests.map((r) => r.status)).toEqual([200, 400, 503, 200]);
  expect((await app.request("/ads/tiktok/requests")).status).toBe(404);
});
