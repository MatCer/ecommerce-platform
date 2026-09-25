import { describe, expect, it } from "vitest";
import { flowConfig, stepCount } from "./flows.ts";

describe("flow settings form", () => {
  it("builds abandoned-cart delays from the filled step fields", () => {
    expect(flowConfig("abandoned_cart", ["1", "24", ""], null)).toEqual({
      delays_hours: [1, 24],
      coupon_percent: null,
    });
    expect(flowConfig("abandoned_cart", ["2", "", ""], "10")).toEqual({
      delays_hours: [2],
      coupon_percent: 10,
    });
  });

  it("mirrors the API rules: increasing, first cart step >= 1 h, at most 90 days", () => {
    expect(flowConfig("abandoned_cart", ["24", "1", ""], null)).toBe("flows.errDelays");
    expect(flowConfig("abandoned_cart", ["0", "", ""], null)).toBe("flows.errDelays");
    expect(flowConfig("abandoned_cart", ["", "", ""], null)).toBe("flows.errDelays");
    expect(flowConfig("abandoned_cart", ["1", "", "5"], null)).toBe("flows.errDelays");
    expect(flowConfig("review_invite", ["2161"], null)).toBe("flows.errDelays");
    expect(flowConfig("review_invite", ["1.5"], null)).toBe("flows.errDelays");
    expect(flowConfig("review_invite", ["168"], null)).toEqual({
      delays_hours: [168],
      coupon_percent: null,
    });
  });

  it("accepts a coupon of 1-50 % on abandoned carts only", () => {
    expect(flowConfig("abandoned_cart", ["1"], "51")).toBe("flows.errCoupon");
    expect(flowConfig("abandoned_cart", ["1"], "0")).toBe("flows.errCoupon");
    expect(flowConfig("review_invite", ["168"], "10")).toBe("flows.errCoupon");
  });

  it("knows how many steps each flow may have", () => {
    expect(stepCount("abandoned_cart")).toBe(3);
    expect(stepCount("review_invite")).toBe(1);
  });
});
