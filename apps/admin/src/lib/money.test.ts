import { describe, expect, it } from "vitest";
import {
  formatMoney,
  fromLocalInput,
  instant,
  minorToInput,
  parseMoney,
  parsePercent,
  toLocalInput,
} from "./money.ts";

describe("money", () => {
  it("parses decimal input with comma or dot and grouping spaces", () => {
    expect(parseMoney("129,90")).toBe(12990);
    expect(parseMoney("1 299.5")).toBe(129950);
    expect(parseMoney("7")).toBe(700);
    expect(parseMoney("0.05")).toBe(5);
  });

  it("rejects negative, over-precise and garbage input", () => {
    for (const bad of ["", "-1", "1.234", "abc", "1,2,3", "1e3"])
      expect(parseMoney(bad)).toBeNull();
  });

  it("round-trips through the input format", () => {
    expect(minorToInput(12990)).toBe("129.90");
    expect(parseMoney(minorToInput(12990))).toBe(12990);
    expect(minorToInput(null)).toBe("");
  });

  it("formats per locale", () => {
    expect(formatMoney(12990, "CZK", "cs").replace(/\s/g, " ")).toBe("129,90 Kč");
    expect(formatMoney(12990, "EUR", "en")).toBe("€129.90");
  });

  it("converts percent to basis points within (0, 100]", () => {
    expect(parsePercent("12,5")).toBe(1250);
    expect(parsePercent("100")).toBe(10000);
    expect(parsePercent("0")).toBeNull();
    expect(parsePercent("101")).toBeNull();
  });

  it("keeps the original instant when the field was not edited", () => {
    const original = "2026-09-25T10:15:37.123Z";
    expect(instant(toLocalInput(original), original)).toBe(original);
    expect(instant("2030-01-01T00:00", original)).toBe(fromLocalInput("2030-01-01T00:00"));
    expect(instant("", original)).toBeNull();
  });

  it("round-trips datetime-local values", () => {
    const iso = fromLocalInput("2026-10-01T09:30");
    expect(iso).not.toBeNull();
    expect(toLocalInput(iso)).toBe("2026-10-01T09:30");
    expect(fromLocalInput("")).toBeNull();
  });
});
