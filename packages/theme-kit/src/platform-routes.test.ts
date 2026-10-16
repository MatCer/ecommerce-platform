import { expect, test } from "vitest";
import cases from "../../../fixtures/platform-paths.json" with { type: "json" };
import { isPlatformPath } from "./platform-routes.ts";

test("platform path cases shared with crates/commerce redirects", () => {
  for (const p of cases.reserved) expect(isPlatformPath(p), p).toBe(true);
  for (const p of cases.allowed) expect(isPlatformPath(p), p).toBe(false);
});
