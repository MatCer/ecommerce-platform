import { expect, test } from "vitest";
import {
  diffLines,
  hexOf,
  parseReport,
  runTone,
  statusTone,
  tokenErrors,
  tokenGroups,
} from "./themes.ts";

test("parses the builder report defensively", () => {
  const r = parseReport({
    pipeline: "full",
    failures: ["budget /p/x: JS 58.6 kB gz > 30", 3],
    steps: [
      { name: "lint", status: "passed", ms: 40 },
      {
        name: "budget",
        status: "failed",
        pages: [
          {
            path: "/p/x",
            lcp_ms: 1350,
            tbt_ms: 0,
            cls: 0.01,
            js_gzip: 60_000,
            js_gzip_with_rum: 61_440,
            calls: 3,
            axe: [{ id: "color-contrast", impact: "serious" }],
            failures: ["JS 58.6 kB gz > 30"],
          },
        ],
      },
      { name: "smoke", status: "weird" },
    ],
  });
  expect(r.failures).toEqual(["budget /p/x: JS 58.6 kB gz > 30"]);
  expect(r.steps.map((s) => s.status)).toEqual(["passed", "failed", "skipped"]);
  expect(r.pages[0]).toMatchObject({
    lcpMs: 1350,
    jsKb: 58.6,
    jsRumKb: 60,
    axe: ["color-contrast"],
  });
  expect(parseReport(null)).toEqual({ pipeline: "", steps: [], pages: [], failures: [] });
});

test("token editor validation mirrors the A6 allowlist", () => {
  const t = tokenGroups({
    colors: { buy: "#ff8800", bg: "oklch(0.97 0.003 250)", evil: "url(x)", n: 3 },
    fonts: { sans: "system-ui, sans-serif" },
    radius: { md: "0.5rem", bad: "1em" },
  });
  expect(Object.keys(t.colors)).toEqual(["buy", "bg", "evil"]);
  expect(tokenErrors(t)).toEqual({ "colors.evil": "color", "radius.bad": "length" });
  expect(hexOf("#ff8800")).toBe("#ff8800");
  expect(hexOf("oklch(1 0 0)")).toBeUndefined();
  expect(statusTone("failed")).toBe("error");
  expect(statusTone("building")).toBe("warning");
});

test("tags unified diff lines and run statuses", () => {
  const d = "--- a/src/x.astro\n+++ b/src/x.astro\n@@ -1,2 +1,2 @@\n <h1>x</h1>\n-<p>a</p>\n+<p>b</p>\nBinary file public/a.png: 1 → 2 bytes\n";
  expect(diffLines(d).map((l) => l.kind)).toEqual(["file", "file", "hunk", "ctx", "del", "add", "file"]);
  expect(diffLines("")).toEqual([]);
  expect(runTone("running")).toBe("warning");
  expect(runTone("succeeded")).toBe("success");
  expect(runTone("discarded")).toBe("neutral");
});
