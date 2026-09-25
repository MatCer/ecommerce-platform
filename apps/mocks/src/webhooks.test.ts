import { createHmac } from "node:crypto";
import { expect, test } from "vitest";
import { app } from "./app.ts";

const json = { "content-type": "application/json" };

test("webhook receiver records deliveries, verifies signatures and fails on demand", async () => {
  const secret = "whsec_test";
  await app.request("/webhooks/t1/config", {
    method: "PUT",
    headers: json,
    body: JSON.stringify({ secret }),
  });
  const body = JSON.stringify({ id: "evt_1", type: "order.paid", data: {} });
  const t = 1_700_000_000;
  const v1 = createHmac("sha256", secret).update(`${t}.${body}`).digest("hex");
  const ok = await app.request("/webhooks/t1", {
    method: "POST",
    headers: { ...json, "x-signature": `t=${t},v1=${v1}`, "x-webhook-event": "order.paid" },
    body,
  });
  expect(ok.status).toBe(200);
  const forged = await app.request("/webhooks/t1", {
    method: "POST",
    headers: { ...json, "x-signature": `t=${t},v1=${"0".repeat(64)}` },
    body,
  });
  expect(forged.status).toBe(200);

  await app.request("/webhooks/t1/config", {
    method: "PUT",
    headers: json,
    body: JSON.stringify({ status: 500 }),
  });
  const failing = await app.request("/webhooks/t1", { method: "POST", headers: json, body });
  expect(failing.status).toBe(500);

  const got = (await (await app.request("/webhooks/t1")).json()) as {
    deliveries: { signature_valid: boolean | null; event: string | null }[];
  };
  expect(got.deliveries.map((d) => d.signature_valid)).toEqual([true, false, false]);
  expect(got.deliveries[0]?.event).toBe("order.paid");

  expect((await app.request("/webhooks/t1", { method: "DELETE" })).status).toBe(204);
  const reset = (await (await app.request("/webhooks/t1")).json()) as { deliveries: unknown[] };
  expect(reset.deliveries).toEqual([]);
  expect((await app.request("/webhooks/bad%20name")).status).toBe(400);
});
