import { expect, test } from "vitest";
import { Counters, templateOf } from "./counters.ts";
import type { Site } from "./sites.ts";

const site = {
  tenant_id: "t1",
  market_id: "m1",
  locale: "cs",
} as unknown as Site;
const now = new Date("2026-10-01T10:00:00Z");

test("route templates", () => {
  expect(templateOf("/")).toBe("home");
  expect(templateOf("/c/trika")).toBe("category");
  expect(templateOf("/p/tricko")).toBe("product");
  expect(templateOf("/search")).toBe("search");
  expect(templateOf("/pages/obchodni-podminky")).toBe("page");
  expect(templateOf("/blog")).toBe("blog");
  expect(templateOf("/blog/post")).toBe("blog");
  expect(templateOf("/nope")).toBe("other");
});

type Sent = { batch_id: string; counters: { template: string; requests: number }[] };

test("counts per template and day; a failed batch is resent unchanged, then new counts", async () => {
  const c = new Counters();
  c.page(site, "product", now);
  c.page(site, "product", now);
  c.page(site, "home", now);
  const sent: Sent[] = [];
  const answer = (status: number) => async (req: Request) => {
    expect(req.headers.get("authorization")).toBe("Bearer tok");
    sent.push((await req.json()) as Sent);
    return new Response(null, { status });
  };
  await expect(c.flush("http://api", "tok", answer(503))).rejects.toThrow();
  expect(c.size).toBe(2);
  c.page(site, "home", now);
  await c.flush("http://api", "tok", answer(200));
  expect(c.size).toBe(0);
  const [failed, resent, next] = sent;
  expect(resent?.batch_id).toBe(failed?.batch_id);
  expect(resent?.counters.map((r) => [r.template, r.requests])).toEqual([
    ["product", 2],
    ["home", 1],
  ]);
  expect(next?.batch_id).not.toBe(failed?.batch_id);
  expect(next?.counters.map((r) => [r.template, r.requests])).toEqual([["home", 1]]);
  expect(JSON.stringify(sent)).not.toMatch(/ip|cookie|query/i);
});
