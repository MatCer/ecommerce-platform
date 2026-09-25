import { describe, expect, it } from "vitest";
import {
  blankBlock,
  countMenu,
  legalInput,
  missingLabel,
  moveItem,
  pageInput,
  parseSynonyms,
  removeItem,
  slugify,
  synonymsText,
  updateMenu,
  validContentHref,
} from "./content-form.ts";

describe("content forms", () => {
  it("strips response-only fields before replacing a page or legal entity", () => {
    const page = {
      id: "id",
      created_at: "now",
      updated_at: "now",
      kind: "legal" as const,
      legal_type: "terms" as const,
      status: "published" as const,
      translations: [{ locale: "cs", title: "Terms", slug: "terms", blocks: [] }],
    };
    expect(Object.keys(pageInput(page)).sort()).toEqual([
      "image_asset_id",
      "kind",
      "legal_type",
      "published_at",
      "status",
      "translations",
    ]);
    expect(pageInput(page).translations).toEqual(page.translations);
    const entity = {
      company_name: "Shop",
      company_id: "1",
      street: "Street",
      city: "City",
      postal_code: "123",
      country: "cz",
      email: "a@b.cz",
      phone: "",
      registry: "",
      returns_address: "",
      updated_at: "now",
    };
    expect(legalInput(entity)).not.toHaveProperty("updated_at");
    expect(legalInput(entity).country).toBe("CZ");
  });
  it("keeps incomplete translations with entered SEO or excerpt for server validation", () => {
    expect(
      pageInput({
        kind: "page",
        translations: [{ locale: "cs", title: "", slug: "", excerpt: "Entered text" }],
      }).translations,
    ).toHaveLength(1);
  });
  it("limits links to shop paths and supported protocols", () => {
    for (const href of [
      "/pages/doprava",
      "https://shop.example/a",
      "mailto:shop@example.test",
      "tel:+420123456789",
    ])
      expect(validContentHref(href)).toBe(true);
    for (const href of [
      "//evil.example",
      "javascript:alert(1)",
      "http://example.test",
      "/\\evil",
      "/bad\npath",
    ])
      expect(validContentHref(href)).toBe(false);
  });
  it("derives shop slugs", () => {
    expect(slugify(" Příliš žluťoučký / kůň ")).toBe("prilis-zlutoucky-kun");
  });
  it("reorders and removes blocks immutably with bounds", () => {
    const blocks = [blankBlock("heading"), blankBlock("faq")];
    expect(moveItem(blocks, 0, 1)).toEqual([blocks[1], blocks[0]]);
    expect(moveItem(blocks, 0, -1)).toEqual(blocks);
    expect(removeItem(blocks, 1)).toEqual([blocks[0]]);
    expect(blocks).toHaveLength(2);
  });
  it("creates independent block defaults", () => {
    const a = blankBlock("faq"),
      b = blankBlock("faq");
    expect(a).not.toBe(b);
    expect(blankBlock("heading")).toEqual({ type: "heading", text: "", level: 2 });
  });
  it("parses and round-trips synonyms, ignoring blank lines", () => {
    expect(parseSynonyms(" boty, obuv\n\n shoes, footwear ")).toEqual({
      groups: [
        ["boty", "obuv"],
        ["shoes", "footwear"],
      ],
    });
    const groups = [["a", "b"]];
    expect(parseSynonyms(synonymsText(groups))).toEqual({ groups });
  });
  it("matches server term and group boundaries", () => {
    for (const s of [
      "a",
      "a,A",
      "a,",
      "a,b\t",
      `${"x".repeat(51)},b`,
      Array.from({ length: 21 }, (_, i) => `${i}`).join(","),
    ])
      expect(parseSynonyms(s).error).toBeDefined();
    expect(parseSynonyms(Array(501).fill("a,b").join("\n")).error).toBe("groups");
    expect(parseSynonyms(`${"😀".repeat(50)},b`).error).toBeUndefined();
    expect(parseSynonyms("")).toEqual({ groups: [] });
  });
  it("updates menu children without changing siblings", () => {
    const child = { link: { type: "url" as const, url: "/a" }, children: [] };
    const tree = [{ ...child, children: [child] }, child];
    const next = updateMenu(tree, [0, 0], (e) => ({ ...e, link: { type: "url", url: "/b" } }));
    expect(next[0]?.children?.[0]?.link).toEqual({ type: "url", url: "/b" });
    expect(tree[0]?.children?.[0]?.link).toEqual({ type: "url", url: "/a" });
    expect(countMenu(tree)).toBe(3);
  });
  it("maps legal field and type labels and preserves product names", () => {
    const labels = { terms: "Terms", company_name: "Company name" };
    expect(missingLabel("terms:cs", labels)).toBe("Terms (cs)");
    expect(missingLabel("company_name", labels)).toBe("Company name");
    expect(missingLabel("Blue shirt", labels)).toBe("Blue shirt");
  });
});
