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

test("packeta widget mock: library entry point and a list that answers only the opener", async () => {
  const lib = await app.request("/packeta/library.js");
  expect(lib.headers.get("content-type")).toContain("javascript");
  expect(await lib.text()).toContain("window.Packeta = { Widget: { pick");

  const page = await app.request(
    "/packeta/widget?country=sk&origin=http%3A%2F%2Fcheckout.demo.localhost%3A8080",
  );
  const html = await page.text();
  expect(html).toContain("Z-BOX Bratislava");
  expect(html).not.toContain("Praha");
  expect(html).toContain('const target = "http://checkout.demo.localhost:8080"');
  // A non-origin (path, script) is never used as the postMessage target.
  const bad = await app.request("/packeta/widget?origin=javascript%3Aalert(1)");
  expect(await bad.text()).toContain("const target = null");
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

test("demo feeds and images are served by fixed name only", async () => {
  const feed = await app.request("/feeds/heureka-demo.xml");
  expect(feed.status).toBe(200);
  expect(await feed.text()).toContain("<SHOPITEM>");
  const img = await app.request("/images/demo/tee-ink.jpg");
  expect(img.headers.get("content-type")).toBe("image/jpeg");
  for (const bad of ["/feeds/..%2F..%2Fpackage.json", "/feeds/other.xml", "/images/demo/x.png"]) {
    expect((await app.request(bad)).status).toBe(404);
  }
  expect((await app.request("/images/demo/missing.jpg")).status).toBe(404);
});

test("fio API mock: scripted incoming payments in the statement format", async () => {
  const add = await app.request("/fio/_transactions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      token: "mockToken123",
      transactions: [{ date: "2026-09-25", amount: 1290.5, vs: "100001", name: "Jan" }],
    }),
  });
  expect(add.status).toBe(200);
  const res = await app.request(
    "/fio/v1/rest/periods/mockToken123/2026-09-20/2026-09-30/transactions.json",
  );
  const body = (await res.json()) as {
    accountStatement: { transactionList: { transaction: Record<string, { value: unknown }>[] } };
  };
  const [tx] = body.accountStatement.transactionList.transaction;
  expect(tx?.column1?.value).toBe(1290.5);
  expect(tx?.column5?.value).toBe("100001");
  expect(typeof tx?.column22?.value).toBe("number");
  const outside = await app.request(
    "/fio/v1/rest/periods/mockToken123/2026-10-01/2026-10-02/transactions.json",
  );
  expect(
    (
      (await outside.json()) as {
        accountStatement: { transactionList: { transaction: unknown[] } };
      }
    ).accountStatement.transactionList.transaction,
  ).toEqual([]);
  const bad = await app.request("/fio/v1/rest/periods/x/2026-10-01/2026-10-02/transactions.json");
  expect(bad.status).toBe(400);
});
