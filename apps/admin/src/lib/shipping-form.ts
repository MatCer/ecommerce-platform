import type { Schemas } from "./api.ts";
import { minorToInput, parseMoney } from "./money.ts";

export interface ShippingForm {
  carrier: Schemas["Carrier"];
  names: Record<string, string>;
  descriptions: Record<string, string>;
  price: string;
  freeOver: string;
  tiers: { grams: string; price: string }[];
  cod: boolean;
  codFee: string;
  active: boolean;
  position: string;
}

export function shippingForm(method?: Schemas["ShippingMethod"]): ShippingForm {
  return {
    carrier: method?.carrier ?? "packeta_pickup",
    names: { ...method?.name_i18n },
    descriptions: { ...method?.description_i18n },
    price: minorToInput(method?.price_minor ?? 0),
    freeOver: minorToInput(method?.free_over_minor),
    tiers:
      method?.weight_tiers.map((tier) => ({
        grams: String(tier.up_to_g),
        price: minorToInput(tier.price_minor),
      })) ?? [],
    cod: method?.cod_allowed ?? false,
    codFee: minorToInput(method?.cod_fee_minor ?? 0),
    active: method?.active ?? true,
    position: String(method?.position ?? 0),
  };
}

export function cleanTranslations(values: Record<string, string>): Record<string, string> {
  return Object.fromEntries(
    Object.entries(values)
      .map(([key, value]) => [key, value.trim()])
      .filter(([, value]) => value !== ""),
  );
}

export function shippingInput(
  form: ShippingForm,
  marketId: string,
): Schemas["ShippingMethodInput"] | null {
  const price = parseMoney(form.price);
  const freeOver = form.freeOver.trim() === "" ? null : parseMoney(form.freeOver);
  const codFee = form.cod ? parseMoney(form.codFee) : 0;
  const position = Number(form.position);
  const names = cleanTranslations(form.names);
  if (
    !marketId ||
    !Object.keys(names).length ||
    price === null ||
    codFee === null ||
    (form.freeOver.trim() !== "" && freeOver === null) ||
    !/^-?\d+$/.test(form.position) ||
    !Number.isInteger(position) ||
    position < -2147483648 ||
    position > 2147483647
  )
    return null;
  const tiers: Schemas["WeightTier"][] = [];
  let previous = 0;
  for (const row of form.tiers) {
    const grams = Number(row.grams);
    const amount = parseMoney(row.price);
    if (
      !/^\d+$/.test(row.grams) ||
      !Number.isSafeInteger(grams) ||
      grams <= previous ||
      amount === null
    )
      return null;
    tiers.push({ up_to_g: grams, price_minor: amount });
    previous = grams;
  }
  return {
    market_id: marketId,
    carrier: form.carrier,
    name_i18n: names,
    description_i18n: cleanTranslations(form.descriptions),
    price_minor: price,
    free_over_minor: freeOver,
    weight_tiers: tiers,
    cod_allowed: form.cod,
    cod_fee_minor: codFee,
    active: form.active,
    position,
  };
}
