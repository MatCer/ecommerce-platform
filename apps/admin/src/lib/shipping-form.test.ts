import { describe, expect, it } from "vitest";
import { cleanTranslations, shippingForm, shippingInput } from "./shipping-form.ts";

describe("shipping form", () => {
  const valid = () => ({ ...shippingForm(), names: { cs: " Doprava ", en: "" } });
  it("converts decimal money, omits blank names and clears a disabled COD fee", () => {
    expect(
      shippingInput({ ...valid(), price: "12,50", freeOver: "100", codFee: "invalid" }, "market"),
    ).toMatchObject({
      market_id: "market",
      name_i18n: { cs: "Doprava" },
      price_minor: 1250,
      free_over_minor: 10000,
      cod_fee_minor: 0,
      weight_tiers: [],
    });
  });
  it("preserves zero thresholds and validates COD prices", () => {
    expect(
      shippingInput({ ...valid(), freeOver: "0", cod: true, codFee: "1.25" }, "m"),
    ).toMatchObject({ free_over_minor: 0, cod_fee_minor: 125 });
    expect(shippingInput({ ...valid(), cod: true, codFee: "" }, "m")).toBeNull();
  });
  it("requires positive, strictly ascending whole gram tiers", () => {
    for (const grams of ["0", "-1", "1.5", "", "9007199254740992"]) {
      expect(shippingInput({ ...valid(), tiers: [{ grams, price: "1" }] }, "m")).toBeNull();
    }
    expect(
      shippingInput(
        {
          ...valid(),
          tiers: [
            { grams: "100", price: "1" },
            { grams: "100", price: "2" },
          ],
        },
        "m",
      ),
    ).toBeNull();
    expect(
      shippingInput(
        {
          ...valid(),
          tiers: [
            { grams: "100", price: "1.50" },
            { grams: "200", price: "2" },
          ],
        },
        "m",
      )?.weight_tiers,
    ).toEqual([
      { up_to_g: 100, price_minor: 150 },
      { up_to_g: 200, price_minor: 200 },
    ]);
  });
  it("hydrates existing methods without losing translations, tiers or zero values", () => {
    const method = {
      id: "method",
      market_id: "m",
      carrier: "personal_pickup" as const,
      name_i18n: { cs: "Osobní odběr", de: "Abholung" },
      description_i18n: { de: "Beschreibung" },
      price_minor: 125,
      free_over_minor: 0,
      cod_allowed: true,
      cod_fee_minor: 250,
      weight_tiers: [{ up_to_g: 1000, price_minor: 345 }],
      active: false,
      position: -2,
      created_at: "2026-09-25T00:00:00Z",
      updated_at: "2026-09-25T00:00:00Z",
    };
    const { id: _id, created_at: _created, updated_at: _updated, ...input } = method;
    expect(shippingInput(shippingForm(method), "m")).toEqual(input);
    expect(cleanTranslations({ cs: "  ", sk: " Dobierka ", de: "Nachnahme" })).toEqual({
      sk: "Dobierka",
      de: "Nachnahme",
    });
  });
  it("rejects blank names, malformed amounts and non-integer positions", () => {
    expect(shippingInput(shippingForm(), "m")).toBeNull();
    for (const patch of [
      { price: "-1" },
      { freeOver: "x" },
      { position: "" },
      { position: "1.2" },
      { position: "2147483648" },
    ]) {
      expect(shippingInput({ ...valid(), ...patch }, "m")).toBeNull();
    }
  });
});
