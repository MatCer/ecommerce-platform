import { expect, test } from "vitest";
import { app } from "./app.ts";

test("healthz reports ok", async () => {
  const res = await app.request("/healthz");
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ status: "ok" });
});

test("dns txt stub stores and serves records", async () => {
  const name = "_commerce-verification.shop.example.cz";
  const empty = await app.request(`/dns/txt?name=${name}`);
  expect(await empty.json()).toEqual({ name, records: [] });

  const put = await app.request("/dns/txt", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      name: `${name.toUpperCase()}.`,
      records: ["commerce-verification=abc"],
    }),
  });
  expect(put.status).toBe(200);

  const got = await app.request(`/dns/txt?name=${name}`);
  expect(await got.json()).toEqual({ name, records: ["commerce-verification=abc"] });
});

test("dns txt stub rejects malformed input", async () => {
  expect((await app.request("/dns/txt?name=bad%20name")).status).toBe(400);
  const res = await app.request("/dns/txt", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: "x.cz", records: [1] }),
  });
  expect(res.status).toBe(400);
});
