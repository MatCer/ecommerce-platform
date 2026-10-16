import * as i18n from "@solid-primitives/i18n";
import { describe, expect, it } from "vitest";
import { cs } from "../i18n/cs.ts";
import { en } from "../i18n/en.ts";
import { sk } from "../i18n/sk.ts";
import type { Schemas } from "./api.ts";
import { detectionParams } from "./insights.ts";

const dictionaries = { cs, sk, en };

function sentence(d: Schemas["Detection"], locale: keyof typeof dictionaries): string {
  const t = i18n.translator(() => i18n.flatten(dictionaries[locale]), i18n.resolveTemplate);
  return t(`insights.${d.kind}`, detectionParams(d, locale)).replaceAll("\u00a0", " ");
}

const funnel: Schemas["Detection"] = {
  kind: "funnel_drop",
  transition: "view_item_to_add_to_cart",
  recent: { from: 1000, to: 59 },
  baseline: { from: 3000, to: 243 },
};
const product: Schemas["Detection"] = {
  kind: "low_add_to_cart",
  product_id: "0190a0b0-0000-7000-8000-000000000001",
  views: 1250,
  adds: 15,
  shop_views: 20000,
  shop_adds: 1280,
};
const lcp: Schemas["Detection"] = {
  kind: "slow_page",
  template: "product",
  metric: "LCP",
  p75: 3412,
  samples: 412,
  requests: 9000,
};

describe("insight sentences", () => {
  it("states a funnel drop with both rates and the counts behind them", () => {
    expect(sentence(funnel, "en")).toBe(
      "Fell to 5.9% in the last 7 days, from 8.1% in the 21 days before (59 of 1,000 against 243 of 3,000).",
    );
    expect(sentence(funnel, "cs")).toBe(
      "Pokles na 5,9 % za posledních 7 dní z 8,1 % za 21 dní předtím (59 z 1 000 oproti 243 z 3 000).",
    );
  });

  it("compares a weak product's add-to-cart rate with the shop's", () => {
    expect(sentence(product, "en")).toBe(
      "Added to cart on 1.2% of views in the last 14 days, against 6.4% across the shop (15 of 1,250 views).",
    );
    expect(sentence(product, "sk")).toBe(
      "Do košíka sa pridáva pri 1,2 % zobrazení za posledných 14 dní, v celom obchode pri 6,4 % (15 z 1 250 zobrazení).",
    );
  });

  it("shows LCP and INP in milliseconds and CLS unitless, each against its good bound", () => {
    expect(sentence(lcp, "en")).toBe(
      "LCP p75 is 3,412 ms, above the good limit of 2,500 ms (412 measurements in the last 14 days).",
    );
    expect(sentence({ ...lcp, metric: "INP", p75: 340.4 }, "cs")).toBe(
      "INP p75 je 340 ms, nad hranicí dobrého výsledku 200 ms (412 měření za posledních 14 dní).",
    );
    expect(sentence({ ...lcp, metric: "CLS", p75: 0.234 }, "en")).toBe(
      "CLS p75 is 0.23, above the good limit of 0.1 (412 measurements in the last 14 days).",
    );
  });
});
