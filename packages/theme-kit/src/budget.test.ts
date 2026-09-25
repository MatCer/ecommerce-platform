import { expect, test } from "vitest";
import { judge, median, type PageResult } from "./budget.ts";

const ok: PageResult = {
  path: "/p/x",
  kind: "product",
  lcpMs: 1200,
  tbtMs: 0,
  cls: 0,
  jsGzip: 24_000,
  jsTransfer: 22_000,
  jsGzipWithRum: 29_000,
  thirdPartyOrigins: [],
  axe: [],
  cspViolations: [],
  subrequests: 2,
};

test("within budget passes; each breach is reported", () => {
  expect(judge(ok)).toEqual([]);
  expect(judge({ ...ok, lcpMs: 1600, jsGzip: 31 * 1024, subrequests: 26 })).toHaveLength(3);
  // The RUM-sampled visit counts too (A26).
  expect(judge({ ...ok, jsGzipWithRum: 30.5 * 1024 })).toEqual(["JS with RUM 30.5 kB gz > 30"]);
});

test("missing measurements fail instead of passing", () => {
  expect(
    judge({ ...ok, lcpMs: median([]), subrequests: Number.NaN, jsGzipWithRum: Number.NaN }),
  ).toEqual(["LCP not measured", "JS with RUM not measured", "calls not measured"]);
});
