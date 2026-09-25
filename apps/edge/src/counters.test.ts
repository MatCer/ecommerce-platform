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

test("counts per template and day without identifiers, searches folded", () => {
  const c = new Counters();
  c.page(site, "product", now);
  c.page(site, "product", now);
  c.page(site, "home", now);
  c.search(site, "  Modré   Tričko ", now);
  c.search(site, "modré tričko", now);
  c.search(site, "   ", now);
  const batch = c.drain();
  expect(batch.counters).toEqual([
    { tenant_id: "t1", market_id: "m1", day: "2026-10-01", template: "product", requests: 2 },
    { tenant_id: "t1", market_id: "m1", day: "2026-10-01", template: "home", requests: 1 },
  ]);
  expect(batch.searches).toEqual([
    { tenant_id: "t1", day: "2026-10-01", locale: "cs", query: "modré tričko", count: 2 },
  ]);
  expect(c.size).toBe(0);
});

test("a failed flush keeps the counts for the next one", async () => {
  const c = new Counters();
  c.page(site, "home", now);
  const sent: unknown[] = [];
  await expect(
    c.flush("http://api", "tok", async () => new Response(null, { status: 503 })),
  ).rejects.toThrow();
  c.page(site, "home", now);
  await c.flush("http://api", "tok", async (req) => {
    expect(req.headers.get("authorization")).toBe("Bearer tok");
    sent.push(await req.json());
    return new Response("{}", { status: 200 });
  });
  expect(sent).toEqual([
    {
      counters: [
        { tenant_id: "t1", market_id: "m1", day: "2026-10-01", template: "home", requests: 2 },
      ],
      searches: [],
    },
  ]);
  expect(c.size).toBe(0);
});
