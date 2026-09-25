import { describe, expect, it } from "vitest";
import {
  codify,
  DraftError,
  draftFromProduct,
  draftToInput,
  eanValid,
  emptyDraft,
  move,
  type Product,
  type ProductOption,
  slugify,
  variantMatrix,
} from "./product-form.ts";

const color: ProductOption = {
  code: "color",
  name_i18n: { cs: "Barva" },
  values: [
    { code: "red", name_i18n: { cs: "Červená" } },
    { code: "blue", name_i18n: { cs: "Modrá" } },
  ],
};
const size: ProductOption = {
  code: "size",
  name_i18n: { cs: "Velikost" },
  values: [
    { code: "s", name_i18n: { cs: "S" } },
    { code: "m", name_i18n: { cs: "M" } },
  ],
};

describe("slugify / codify", () => {
  it("folds Czech and Slovak diacritics", () => {
    expect(slugify("Tričko Ľahké — Žluté 100 %")).toBe("tricko-lahke-zlute-100");
    expect(slugify("  --  ")).toBe("");
    expect(codify("Barva látky")).toBe("barva-latky");
  });
});

describe("variantMatrix", () => {
  it("builds every combination with derived SKUs and one default", () => {
    const v = variantMatrix([color, size], [], "ts basic");
    expect(v.map((x) => x.sku)).toEqual([
      "TS-BASIC-RED-S",
      "TS-BASIC-RED-M",
      "TS-BASIC-BLUE-S",
      "TS-BASIC-BLUE-M",
    ]);
    expect(v.filter((x) => x.is_default)).toHaveLength(1);
    expect(v[0]?.option_values).toEqual({ color: "red", size: "s" });
  });

  it("keeps existing variants (id, SKU, EAN) whose combination survives", () => {
    const first = variantMatrix([color], [], "TS");
    const edited = first.map((x, i) => ({
      ...x,
      id: `id-${i}`,
      ean: i === 1 ? "4006381333931" : "",
    }));
    const next = variantMatrix([color, size], edited, "TS");
    expect(next).toHaveLength(4);
    const blue = variantMatrix([color], edited, "TS").find((x) => x.option_values.color === "blue");
    expect(blue?.id).toBe("id-1");
    expect(blue?.ean).toBe("4006381333931");
  });

  it("gives a product without options exactly one variant", () => {
    const v = variantMatrix([], [], "MUG");
    expect(v).toEqual([{ sku: "MUG", ean: "", weight_g: "", option_values: {}, is_default: true }]);
  });
});

describe("eanValid", () => {
  it("checks GTIN check digits", () => {
    expect(eanValid("4006381333931")).toBe(true);
    expect(eanValid("4006381333932")).toBe(false);
    expect(eanValid("96385074")).toBe(true);
    expect(eanValid("123")).toBe(false);
  });
});

describe("draftToInput", () => {
  it("omits untranslated locales, blank optional fields and empty GPSR parties", () => {
    const d = emptyDraft();
    d.translations.cs = { ...d.translations.cs, name: " Hrnek ", slug: "hrnek" };
    d.warnings.cs = "Horké";
    d.unit_measure = "l";
    d.unit_quantity = "0,33";
    d.variants = [{ sku: "MUG", ean: "", weight_g: "350", option_values: {}, is_default: true }];
    const input = draftToInput(d);
    expect(input.translations).toEqual([
      {
        locale: "cs",
        name: "Hrnek",
        slug: "hrnek",
        short_description: "",
        description_html: "",
        seo_title: null,
        seo_description: null,
      },
    ]);
    expect(input.gpsr).toEqual({
      manufacturer: null,
      eu_responsible_person: null,
      safety_info: {},
      warnings: { cs: "Horké" },
    });
    expect(input.unit_quantity).toBe(0.33);
    expect(input.variants?.[0]).toEqual({
      sku: "MUG",
      ean: null,
      weight_g: 350,
      option_values: {},
      is_default: true,
    });
    expect(input.brand).toBeNull();
  });

  it("round-trips a product", () => {
    const p: Product = {
      id: "p1",
      status: "active",
      brand: "Acme",
      gpsr: {
        manufacturer: { name: "Acme", address: "Praha", email: "a@acme.cz" },
        safety_info: {},
        warnings: {},
      },
      translations: [
        { locale: "sk", name: "Hrnček", slug: "hrncek", description_html: "<p>x</p>" },
      ],
      options: [color],
      variants: [{ id: "v1", sku: "A", option_values: { color: "red" }, is_default: true }],
      category_ids: ["c1"],
      media: [{ asset_id: "a1", alt_i18n: { sk: "foto" } }],
      parameters: [],
      tax_categories: { CZ: "reduced" },
      created_at: "",
      updated_at: "",
    };
    const input = draftToInput(draftFromProduct(p));
    expect(input.translations?.map((t) => t.locale)).toEqual(["sk"]);
    expect(input.variants?.[0]?.id).toBe("v1");
    expect(input.gpsr?.manufacturer?.email).toBe("a@acme.cz");
    expect(input.tax_categories).toEqual({ CZ: "reduced" });
    expect(input.media).toEqual([{ asset_id: "a1", alt_i18n: { sk: "foto" } }]);
  });
});

describe("draftToInput validation", () => {
  it("drops blank option and value translations", () => {
    const d = emptyDraft();
    d.options = [
      {
        code: "size",
        name_i18n: { cs: "Velikost", en: "" },
        values: [{ code: "s", name_i18n: { cs: "S", sk: " " } }],
      },
    ];
    expect(draftToInput(d).options).toEqual([
      {
        code: "size",
        name_i18n: { cs: "Velikost" },
        values: [{ code: "s", name_i18n: { cs: "S" } }],
      },
    ]);
  });

  it("rejects non-numeric or fractional weights instead of clearing them", () => {
    const d = emptyDraft();
    d.variants = [{ sku: "A", ean: "", weight_g: "abc", option_values: {}, is_default: true }];
    expect(() => draftToInput(d)).toThrow(DraftError);
    d.variants = [{ sku: "A", ean: "", weight_g: "1.5", option_values: {}, is_default: true }];
    expect(() => draftToInput(d)).toThrow("invalid_weight");
    d.unit_measure = "l";
    d.unit_quantity = "x";
    d.variants = [];
    expect(() => draftToInput(d)).toThrow("invalid_unit");
  });

  it("refuses matrices above the variant limit before building them", () => {
    const many = (code: string): ProductOption => ({
      code,
      name_i18n: { cs: code },
      values: Array.from({ length: 10 }, (_, i) => ({ code: `v${i}`, name_i18n: { cs: `${i}` } })),
    });
    expect(() => variantMatrix([many("a"), many("b"), many("c")], [], "X")).toThrow(
      "too_many_variants",
    );
    expect(variantMatrix([many("a"), many("b")], [], "X")).toHaveLength(100);
  });
});

describe("move", () => {
  it("moves and clamps", () => {
    expect(move([1, 2, 3], 0, 1)).toEqual([2, 1, 3]);
    expect(move([1, 2, 3], 2, 1)).toEqual([1, 2, 3]);
    expect(move([1, 2, 3], 2, -2)).toEqual([3, 1, 2]);
  });
});
