import { expect, test } from "vitest";
import { app } from "./app.ts";

test("healthz reports ok", async () => {
  const res = await app.request("/healthz");
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ status: "ok" });
});
