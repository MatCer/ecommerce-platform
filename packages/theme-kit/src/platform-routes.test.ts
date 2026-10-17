import { expect, test } from "vitest";
import cases from "../../../fixtures/platform-paths.json" with { type: "json" };
import { isPlatformPath, platformSegment } from "./platform-routes.ts";

test("platform path cases shared with crates/commerce redirects", () => {
  for (const p of cases.reserved) expect(isPlatformPath(p), p).toBe(true);
  for (const p of cases.allowed) expect(isPlatformPath(p), p).toBe(false);
});

test("platformSegment names the reserved segment a path decodes to", () => {
  for (const p of cases.reserved) expect(platformSegment(p), p).not.toBeNull();
  for (const p of cases.allowed) expect(platformSegment(p), p).toBeNull();
  expect(platformSegment("/CHECKOUT")).toBe("checkout");
  expect(platformSegment("/%5Fp/cart")).toBe("_p");
  expect(platformSegment("/cs/CHECKOUT")).toBe("checkout");
  expect(platformSegment("/en-GB/o/x")).toBe("o");
  expect(platformSegment("/x/../withdraw")).toBe("withdraw");
  expect(platformSegment("/_checkout/b1/a.js")).toBe("_checkout");
});
