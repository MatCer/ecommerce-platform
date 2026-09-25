import { describe, expect, it } from "vitest";
import {
  dailySeries,
  groupEvents,
  niceMax,
  presetRange,
  resolveRange,
  validRange,
  vitalRating,
} from "./analytics.ts";

const now = new Date("2026-09-25T22:30:00Z");

describe("date ranges", () => {
  it("presets end today (UTC) and include both ends", () => {
    expect(presetRange(7, now)).toEqual({ from: "2026-09-19", to: "2026-09-25" });
    expect(presetRange(30, now)).toEqual({ from: "2026-08-27", to: "2026-09-25" });
  });

  it("accepts real dates in order, at most 366 days apart", () => {
    expect(validRange("2026-01-01", "2026-01-01")).toBe(true);
    expect(validRange("2025-01-01", "2026-01-02")).toBe(true);
    expect(validRange("2025-01-01", "2026-01-03")).toBe(false);
    expect(validRange("2026-02-01", "2026-01-31")).toBe(false);
    expect(validRange("2026-02-30", "2026-03-01")).toBe(false);
    expect(validRange("", "2026-03-01")).toBe(false);
  });

  it("reads the URL: a valid custom range wins, then the preset, then 30 days", () => {
    expect(resolveRange({ from: "2026-09-01", to: "2026-09-10", range: "7" }, now)).toEqual({
      preset: null,
      from: "2026-09-01",
      to: "2026-09-10",
    });
    expect(resolveRange({ range: "7" }, now)).toEqual({ preset: 7, ...presetRange(7, now) });
    expect(resolveRange({ range: "12", from: "2026-09-10", to: "2026-09-01" }, now)).toEqual({
      preset: 30,
      ...presetRange(30, now),
    });
  });
});

describe("dailySeries", () => {
  it("fills every day of the range with zeros, per currency, sorted", () => {
    const rows = [
      { date: "2026-09-02", currency: "EUR", revenue_minor: 500, orders: 1 },
      { date: "2026-09-01", currency: "CZK", revenue_minor: 1000, orders: 2 },
    ];
    expect(dailySeries(rows, "2026-09-01", "2026-09-03")).toEqual([
      {
        currency: "CZK",
        days: [
          { date: "2026-09-01", revenue_minor: 1000, orders: 2 },
          { date: "2026-09-02", revenue_minor: 0, orders: 0 },
          { date: "2026-09-03", revenue_minor: 0, orders: 0 },
        ],
      },
      {
        currency: "EUR",
        days: [
          { date: "2026-09-01", revenue_minor: 0, orders: 0 },
          { date: "2026-09-02", revenue_minor: 500, orders: 1 },
          { date: "2026-09-03", revenue_minor: 0, orders: 0 },
        ],
      },
    ]);
  });
});

describe("niceMax", () => {
  it("rounds the axis top up to 1, 2, 2.5 or 5 times a power of ten", () => {
    expect(niceMax(0)).toBe(1);
    expect(niceMax(7)).toBe(10);
    expect(niceMax(12)).toBe(20);
    expect(niceMax(2400)).toBe(2500);
    expect(niceMax(26_000)).toBe(50_000);
    expect(niceMax(100)).toBe(100);
  });
});

describe("vitalRating", () => {
  it("uses the Core Web Vitals thresholds (good up to and including the limit)", () => {
    expect(vitalRating("LCP", 2500)).toBe("good");
    expect(vitalRating("LCP", 2501)).toBe("needsImprovement");
    expect(vitalRating("LCP", 4001)).toBe("poor");
    expect(vitalRating("INP", 200)).toBe("good");
    expect(vitalRating("INP", 500)).toBe("needsImprovement");
    expect(vitalRating("INP", 501)).toBe("poor");
    expect(vitalRating("CLS", 0.1)).toBe("good");
    expect(vitalRating("CLS", 0.2)).toBe("needsImprovement");
    expect(vitalRating("CLS", 0.3)).toBe("poor");
    expect(vitalRating("FID", 1)).toBeNull();
  });
});

describe("groupEvents", () => {
  it("groups event types by their prefix, keeping order", () => {
    expect(groupEvents(["order.created", "product.created", "order.paid", "ping"])).toEqual([
      { group: "order", events: ["order.created", "order.paid"] },
      { group: "product", events: ["product.created"] },
      { group: "ping", events: ["ping"] },
    ]);
  });
});
