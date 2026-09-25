import { expect, test } from "vitest";
import { clsOf, inpOf, metric } from "./vitals.ts";

const shift = (startTime: number, value: number, hadRecentInput = false) => ({
  startTime,
  value,
  hadRecentInput,
});

test("CLS is the largest session window, input-driven shifts excluded", () => {
  expect(clsOf([])).toBe(0);
  // One window: gaps < 1 s.
  expect(clsOf([shift(0, 0.02), shift(500, 0.03), shift(1400, 0.01)])).toBeCloseTo(0.06);
  // A gap of 1 s or more starts a new window; the larger one counts.
  expect(clsOf([shift(0, 0.02), shift(2000, 0.05), shift(2500, 0.01)])).toBeCloseTo(0.06);
  // Windows are capped at 5 s even with small gaps.
  const steady = Array.from({ length: 12 }, (_, i) => shift(i * 900, 0.01));
  expect(clsOf(steady)).toBeCloseTo(0.06);
  expect(clsOf([shift(0, 0.3, true), shift(100, 0.01)])).toBeCloseTo(0.01);
});

test("INP is the worst interaction, one skipped per 50 (≈ p98)", () => {
  expect(inpOf([])).toBe(0);
  expect(inpOf([40, 120, 80])).toBe(120);
  const many = [...Array.from({ length: 99 }, () => 50), 900];
  expect(inpOf(many)).toBe(50); // 100 interactions: the single outlier is skipped
});

test("ratings follow the web.dev thresholds", () => {
  expect(metric("LCP", 2400).rating).toBe("good");
  expect(metric("LCP", 3000).rating).toBe("needs-improvement");
  expect(metric("INP", 600).rating).toBe("poor");
  expect(metric("CLS", 0.1).rating).toBe("good");
});
