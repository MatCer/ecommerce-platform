import { expect, test } from "vitest";
import { labelPdf } from "./pdf.ts";

test("label PDF has byte-accurate object and stream offsets", () => {
  const pdf = labelPdf("PACKETA (mock)", "Z2000000001", "Jana Nováková (test) \\");
  const text = pdf.toString("ascii");
  expect(text).toMatch(/^%PDF-1.4/);
  expect(text).toContain("/MediaBox [0 0 298 420]");
  const start = Number(/startxref\n(\d+)/.exec(text)?.[1]);
  expect(text.slice(start, start + 4)).toBe("xref");
  const entries = text.slice(start).split("\n").slice(3, 8);
  entries.forEach((entry, index) => {
    expect(text.slice(Number(entry.slice(0, 10)))).toMatch(new RegExp(`^${index + 1} 0 obj`));
  });
  const stream = /\/Length (\d+) >>\nstream\n([\s\S]*?)endstream/.exec(text);
  expect(Buffer.byteLength(stream?.[2] ?? "")).toBe(Number(stream?.[1]));
  expect(text).toContain("Jana Novakova");
});
