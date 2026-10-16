import { describe, expect, it } from "vitest";
import { altField, displayValue, isHtml, kindsFor, minorToMajor } from "./ai.ts";

describe("AI preview helpers", () => {
  it("renders text, block lists and menu labels", () => {
    expect(displayValue("name", "Tričko")).toBe("Tričko");
    expect(displayValue("seo_title", null)).toBe("");
    expect(
      displayValue("blocks", [
        { type: "heading", text: "A <b>", level: 2 },
        { type: "rich_text", html: "<p>B</p>" },
        { type: "faq", items: [{ question: "Q?", answer_html: "<p>A.</p>" }] },
      ]),
    ).toBe("<p>A &lt;b&gt;</p><p>B</p><p>Q?</p><p>A.</p>");
    expect(displayValue("labels", [{ path: "0", label: "Akce & více" }])).toBe(
      "<p>Akce &amp; více</p>",
    );
  });

  it("knows which helpers and renderings apply", () => {
    expect(isHtml("description_html")).toBe(true);
    expect(isHtml("seo_title")).toBe(false);
    expect(kindsFor("menu")).toEqual(["translate"]);
    expect(kindsFor("category")).toContain("category_description");
    expect(minorToMajor(546)).toBe("5.46");
    expect(minorToMajor(-5)).toBe("-0.05");
  });

  it("names an image's alt text mark like the API", () => {
    expect(altField("0192f0c3-6a5e-7c3d-9a41-1f2e3d4c5b6a")).toBe(
      "alt_0192f0c36a5e7c3d9a411f2e3d4c5b6a",
    );
  });
});
