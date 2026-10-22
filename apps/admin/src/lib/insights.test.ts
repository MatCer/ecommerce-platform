import * as i18n from "@solid-primitives/i18n";
import { describe, expect, it } from "vitest";
import { cs } from "../i18n/cs.ts";
import { en } from "../i18n/en.ts";
import { sk } from "../i18n/sk.ts";
import type { Schemas } from "./api.ts";
import { detectionParams, measureText } from "./insights.ts";

const dictionaries = { cs, sk, en };
type Locale = keyof typeof dictionaries;

const translator = (locale: Locale) =>
  i18n.translator(() => i18n.flatten(dictionaries[locale]), i18n.resolveTemplate);

function sentence(d: Schemas["Detection"], locale: Locale): string {
  return translator(locale)(`insights.${d.kind}`, detectionParams(d, locale)).replaceAll(
    "\u00a0",
    " ",
  );
}

function measure(d: Schemas["Detection"], m: Schemas["InsightMeasure"], locale: Locale): string {
  return measureText(d, m, locale, translator(locale)).replaceAll("\u00a0", " ");
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

describe("before/after measures", () => {
  it("states a funnel step's rate over the sessions that reached the step", () => {
    const m = { value: 0.059, samples: 1000 };
    expect(measure(funnel, m, "en")).toBe("5.9% of 1,000 sessions");
    expect(measure(funnel, m, "cs")).toBe("5,9 % z 1 000 relací");
    expect(measure(funnel, m, "sk")).toBe("5,9 % z 1 000 relácií");
  });

  it("states a product's add-to-cart rate over its views", () => {
    const m = { value: 0.012, samples: 1250 };
    expect(measure(product, m, "en")).toBe("1.2% of 1,250 views");
    expect(measure(product, m, "cs")).toBe("1,2 % z 1 250 zobrazení");
    expect(measure(product, m, "sk")).toBe("1,2 % z 1 250 zobrazení");
  });

  it("states a page's p75 in the vital's unit over its measurements", () => {
    expect(measure(lcp, { value: 3412, samples: 412 }, "en")).toBe(
      "p75 3,412 ms from 412 measurements",
    );
    expect(measure({ ...lcp, metric: "INP" }, { value: 340.4, samples: 412 }, "cs")).toBe(
      "p75 340 ms z 412 měření",
    );
    expect(measure({ ...lcp, metric: "CLS" }, { value: 0.234, samples: 412 }, "sk")).toBe(
      "p75 0,23 z 412 meraní",
    );
  });

  it("says there is no data instead of a zero when the window has none", () => {
    const m = { value: null, samples: 0 };
    expect(measure(funnel, m, "en")).toBe("No data");
    expect(measure(product, m, "cs")).toBe("Žádná data");
    expect(measure(lcp, m, "sk")).toBe("Žiadne údaje");
  });
});
