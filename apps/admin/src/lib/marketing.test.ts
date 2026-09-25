import { describe, expect, it } from "vitest";
import {
  blankCondition,
  blankContent,
  blankEmailBlock,
  blockValid,
  type Condition,
  campaignInput,
  contentProblem,
  EMAIL_BLOCK_TYPES,
  fromCondition,
  parseEmails,
  splitList,
  toCondition,
  toRules,
} from "./marketing.ts";

describe("segment rules", () => {
  it("maps every condition to the API and back", () => {
    const conditions: Condition[] = [
      { field: "locale", locales: ["cs", "sk"] },
      { field: "market", market_ids: ["m1"] },
      { field: "purchased_category", category_ids: ["c1"] },
      { field: "purchased_brand", brands: ["Nike", "Adidas"] },
      { field: "order_count", min: 2, max: undefined },
      { field: "total_spent", currency: "EUR", min_minor: 1050, max_minor: undefined },
      { field: "engaged", days: 30 },
      { field: "affinity", dim: "category", keys: ["c1"] },
      { field: "affinity", dim: "brand", keys: ["Nike"] },
    ];
    for (const c of conditions) expect(toCondition(fromCondition(c))).toEqual(c);
    const dated = toCondition(
      fromCondition({ field: "subscribed", after: "2026-01-01T10:00:00Z" }),
    );
    expect(dated).toEqual({
      field: "subscribed",
      after: "2026-01-01T10:00:00.000Z",
      before: undefined,
    });
  });

  it("converts amounts in major units to minor units", () => {
    const f = { ...blankCondition("total_spent", "CZK"), min: "1 299,50", max: "5000" };
    expect(toCondition(f)).toEqual({
      field: "total_spent",
      currency: "CZK",
      min_minor: 129950,
      max_minor: 500000,
    });
    expect(fromCondition({ field: "total_spent", currency: "CZK", min_minor: 129950 }).min).toBe(
      "1299.50",
    );
  });

  it("rejects incomplete, reversed or out-of-range conditions", () => {
    expect(toCondition(blankCondition("locale"))).toBeNull();
    expect(toCondition(blankCondition("order_count"))).toBeNull();
    expect(toCondition({ ...blankCondition("order_count"), min: "5", max: "2" })).toBeNull();
    expect(toCondition({ ...blankCondition("order_count"), min: "-1" })).toBeNull();
    expect(toCondition({ ...blankCondition("total_spent"), min: "abc" })).toBeNull();
    expect(toCondition({ ...blankCondition("engaged"), days: "366" })).toBeNull();
    expect(toCondition(blankCondition("engaged"))).toEqual({ field: "engaged", days: 30 });
    expect(toCondition({ ...blankCondition("purchased_brand"), text: " , " })).toBeNull();
  });

  it("builds rules or points at the first incomplete condition", () => {
    const ok = { ...blankCondition("locale"), ids: ["cs"] };
    expect(toRules("any", [ok])).toEqual({
      rules: { match: "any", conditions: [{ field: "locale", locales: ["cs"] }] },
    });
    expect(toRules("all", [ok, blankCondition("market")])).toEqual({ invalid: 1 });
    expect(toRules("all", [])).toEqual({ rules: { match: "all", conditions: [] } });
    expect(splitList(" a, b ,,c ")).toEqual(["a", "b", "c"]);
  });
});

describe("campaign content", () => {
  it("starts every block type blank and invalid", () => {
    for (const type of EMAIL_BLOCK_TYPES) {
      const b = blankEmailBlock(type);
      expect(b.type).toBe(type);
      expect(blockValid(b)).toBe(type === "personalized_products");
    }
  });

  it("checks blocks like the API", () => {
    expect(blockValid({ type: "text", html: "<p><br></p>" })).toBe(false);
    expect(blockValid({ type: "text", html: "<p>Hi</p>" })).toBe(true);
    expect(blockValid({ type: "button", label: "Shop", href: "/c/tricka" })).toBe(true);
    expect(blockValid({ type: "button", label: "Shop", href: "javascript:alert(1)" })).toBe(false);
    expect(blockValid({ type: "personalized_products", limit: 9 })).toBe(false);
    const ids = Array.from({ length: 13 }, (_, i) => String(i));
    expect(blockValid({ type: "product_grid", product_ids: ids })).toBe(false);
  });

  it("reports the first problem and ignores untouched languages", () => {
    expect(contentProblem({ cs: blankContent(), sk: blankContent() })).toEqual({
      kind: "no_content",
    });
    expect(contentProblem({ cs: { ...blankContent(), subject: "Hi" } })).toEqual({
      kind: "blocks",
      locale: "cs",
    });
    const heading = { type: "heading" as const, text: "" };
    expect(contentProblem({ cs: { subject: "Hi", blocks: [heading] } })).toEqual({
      kind: "block",
      locale: "cs",
      index: 0,
    });
    const good = { subject: "Hi", preheader: "", blocks: [{ ...heading, text: "Autumn" }] };
    expect(contentProblem({ cs: good, en: blankContent() })).toBeNull();
  });

  it("sends explicit fields only and drops empty languages", () => {
    const body = campaignInput(" Autumn ", "", {
      cs: {
        subject: " Hi ",
        preheader: "",
        blocks: [{ type: "image", asset_id: "a1", alt: "x", href: "" }],
      },
      en: blankContent(),
    });
    const both = campaignInput("x", "s1", {
      sk: { subject: "Ahoj", blocks: [{ type: "heading", text: "A" }] },
      cs: { subject: "Ahoj", blocks: [{ type: "heading", text: "A" }] },
    });
    expect(Object.keys(both.content)).toEqual(["cs", "sk"]);
    expect(both.segment_id).toBe("s1");
    expect(body).toEqual({
      name: "Autumn",
      segment_id: null,
      content: {
        cs: { subject: "Hi", preheader: "", blocks: [{ type: "image", asset_id: "a1", alt: "x" }] },
      },
    });
  });

  it("parses 1-5 test recipients", () => {
    expect(parseEmails("a@x.cz, b@y.sk\nc@z.com a@x.cz")).toEqual(["a@x.cz", "b@y.sk", "c@z.com"]);
    expect(parseEmails("")).toBeNull();
    expect(parseEmails("not-an-email")).toBeNull();
    expect(parseEmails("1@x.cz 2@x.cz 3@x.cz 4@x.cz 5@x.cz 6@x.cz")).toBeNull();
  });
});
