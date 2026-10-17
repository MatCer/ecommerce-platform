import { t } from "@platform/storefront-sdk/format";
import type { CheckoutAddress, CheckoutView } from "@platform/storefront-sdk/types";
import { describe, expect, test } from "vitest";
import cs from "../../../../crates/commerce/src/storefront/messages/cs.json";
import en from "../../../../crates/commerce/src/storefront/messages/en.json";
import sk from "../../../../crates/commerce/src/storefront/messages/sk.json";
import { addressLine, checkoutPage, regionName, reviewFacts } from "./checkout-steps";

const money = (amount_minor: number) => ({
  amount_minor,
  currency: "CZK" as const,
  formatted: `${amount_minor / 100} Kč`,
});

const JANA: CheckoutAddress = {
  name: "Jana Nováková",
  company: "Firma s.r.o.",
  street: "Dlouhá 12",
  city: "Praha",
  postal_code: "110 00",
  country: "CZ",
  phone: null,
};
const PETR: CheckoutAddress = {
  name: "Petr Novák",
  company: null,
  street: "Krátká 2",
  city: "Bratislava",
  postal_code: "811 01",
  country: "SK",
  phone: null,
};
const HOME = "11111111-1111-1111-1111-111111111111";
const PICKUP = "22222222-2222-2222-2222-222222222222";

function view(over: Partial<CheckoutView> = {}): CheckoutView {
  return {
    cart: {
      currency: "CZK",
      discount: money(0),
      id: "33333333-3333-3333-3333-333333333333",
      item_count: 0,
      lines: [],
      ship_to_country: "SK",
      subtotal: money(10000),
      total: money(10000),
      vat: [],
      vat_total: money(0),
      version: 3,
    },
    legal: {
      privacy_url: "/privacy",
      terms_url: "/terms",
      text_version: "1",
      withdrawal_url: "/withdrawal",
    },
    missing: [],
    payment_methods: [
      { fee: money(0), kind: "bank_transfer", name: "Bankovní převod", selectable: true },
      { fee: money(3000), kind: "cod", name: "Dobírka", selectable: true },
    ],
    shipping_methods: [
      {
        carrier: "ppl",
        cod_allowed: true,
        cod_fee: money(3000),
        id: HOME,
        name: "PPL domů",
        needs_pickup_point: false,
        price: money(9900),
      },
      {
        carrier: "packeta_pickup",
        cod_allowed: true,
        cod_fee: money(3000),
        id: PICKUP,
        name: "Zásilkovna",
        needs_pickup_point: true,
        price: money(7900),
      },
    ],
    ship_to_countries: ["SK", "CZ"],
    totals: {
      discount: money(0),
      payment_fee: money(0),
      shipping: money(0),
      subtotal: money(10000),
      total: money(10000),
      vat: [],
      vat_total: money(0),
    },
    ...over,
  };
}

const at = (query = "") => new URL(`https://shop.test/checkout${query}`);
const form = (entries: [string, string][]) => {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
};
const region = (code: string) => ({ CZ: "Česko", SK: "Slovensko" })[code] ?? code;

const NOTHING = view();
const CONTACT = view({ email: "jana@example.test", phone: "+420 601 234 567" });
const BOTH = view({ email: "jana@example.test", phone: null, billing_address: JANA });
const ELSEWHERE = view({ ...BOTH, shipping_address: PETR });

describe("checkoutPage opens one editor from the view, edit and the draft", () => {
  test("a fresh checkout opens contact with blank values", () => {
    expect(checkoutPage(NOTHING, at())).toEqual({
      kind: "contact",
      values: { email: "", phone: "" },
      problem: null,
      canCancel: false,
    });
  });

  test("a saved contact opens addresses with blanks in the first ship-to country", () => {
    const page = checkoutPage(CONTACT, at());
    expect(page).toMatchObject({ kind: "addresses", problem: null, canCancel: false });
    if (page.kind !== "addresses") throw new Error(page.kind);
    for (const a of [page.values.billing, page.values.shipping])
      expect(a).toMatchObject({ name: "", street: "", city: "", postal_code: "", country: "SK" });
    expect(page.values.elsewhere).toBe(false);
  });

  test("both saved is the review", () => {
    expect(checkoutPage(BOTH, at())).toEqual({
      kind: "review",
      problem: null,
      choices: null,
      refusal: null,
    });
  });

  test("edit=contact opens the saved contact with a cancel", () => {
    expect(checkoutPage(CONTACT, at("?edit=contact"))).toEqual({
      kind: "contact",
      values: { email: "jana@example.test", phone: "+420 601 234 567" },
      problem: null,
      canCancel: true,
    });
  });

  test("edit=addresses opens the saved addresses with a cancel", () => {
    expect(checkoutPage(ELSEWHERE, at("?edit=addresses"))).toEqual({
      kind: "addresses",
      values: { billing: JANA, shipping: PETR, elsewhere: true },
      problem: null,
      canCancel: true,
    });
  });

  test("a missing email wins over edit=addresses", () => {
    expect(checkoutPage(NOTHING, at("?edit=addresses")).kind).toBe("contact");
  });

  test.each(["payment", "review", "__proto__", "CONTACT", ""])(
    "an unknown edit %j on a complete checkout is the review",
    (edit) => {
      expect(checkoutPage(BOTH, at(`?edit=${edit}`)).kind).toBe("review");
    },
  );

  test("a rejected contact keeps the exact draft and puts the problem on its field", () => {
    const draft = form([
      ["_pt", "token"],
      ["email", " Eva@Example.test "],
      ["phone", "abc"],
    ]);
    expect(checkoutPage(BOTH, at("?edit=contact&error=invalid_phone"), draft)).toEqual({
      kind: "contact",
      values: { email: " Eva@Example.test ", phone: "abc" },
      problem: { step: "contact", field: "phone", message: "address.invalid" },
      canCancel: true,
    });
  });

  test("a rejected first contact has no cancel", () => {
    const draft = form([
      ["email", "eva@example"],
      ["phone", ""],
    ]);
    expect(checkoutPage(NOTHING, at("?edit=contact&error=invalid_email"), draft)).toEqual({
      kind: "contact",
      values: { email: "eva@example", phone: "" },
      problem: { step: "contact", field: "email", message: "account.invalid_email" },
      canCancel: false,
    });
  });

  const addressDraft = (elsewhere: boolean) =>
    form([
      ["_pt", "token"],
      ["billing_name", " Jana "],
      ["billing_company", ""],
      ["billing_street", "Dlouhá 12"],
      ["billing_postal_code", "110 00"],
      ["billing_city", "Praha"],
      ["billing_country", "CZ"],
      ...(elsewhere ? ([["ship_elsewhere", "1"]] as [string, string][]) : []),
      ["shipping_name", "Petr"],
      ["shipping_company", "Sklad"],
      ["shipping_street", ""],
      ["shipping_postal_code", "811 01"],
      ["shipping_city", "Bratislava"],
      ["shipping_country", "AT"],
    ]);

  test("a rejected address keeps both groups and the checkbox, the problem on the form", () => {
    expect(
      checkoutPage(CONTACT, at("?edit=addresses&error=invalid_address"), addressDraft(true)),
    ).toEqual({
      kind: "addresses",
      values: {
        billing: {
          name: " Jana ",
          company: "",
          street: "Dlouhá 12",
          postal_code: "110 00",
          city: "Praha",
          country: "CZ",
        },
        shipping: {
          name: "Petr",
          company: "Sklad",
          street: "",
          postal_code: "811 01",
          city: "Bratislava",
          country: "AT",
        },
        elsewhere: true,
      },
      problem: { step: "addresses", field: null, message: "address.invalid" },
      canCancel: false,
    });
  });

  test("an unticked checkbox in the draft wins over a saved delivery address", () => {
    const page = checkoutPage(
      ELSEWHERE,
      at("?edit=addresses&error=ship_to_not_allowed"),
      addressDraft(false),
    );
    expect(page).toMatchObject({
      kind: "addresses",
      values: { elsewhere: false },
      problem: { step: "addresses", field: null, message: "checkout.ship_to_not_allowed" },
      canCancel: true,
    });
  });

  test("a problem of another step than the open editor is page-level", () => {
    expect(checkoutPage(BOTH, at("?error=invalid_email"))).toEqual({
      kind: "review",
      problem: { step: null, field: null, message: "account.invalid_email" },
      choices: null,
      refusal: null,
    });
    expect(checkoutPage(CONTACT, at("?error=invalid_phone"))).toMatchObject({
      kind: "addresses",
      problem: { step: null, field: null, message: "address.invalid" },
    });
  });

  test("a stale page is a page-level notice", () => {
    expect(checkoutPage(BOTH, at("?error=stale_page")).problem).toEqual({
      step: null,
      field: null,
      message: "checkout.stale_page",
    });
  });

  test.each(["failed", "nope", "toString", "__proto__", "constructor", "hasOwnProperty"])(
    "the code %s reads as failed",
    (code) => {
      expect(checkoutPage(CONTACT, at(`?edit=addresses&error=${code}`)).problem).toEqual({
        step: null,
        field: null,
        message: "checkout.details_failed",
      });
    },
  );

  test("the view is never mutated", () => {
    const before = structuredClone(ELSEWHERE);
    checkoutPage(ELSEWHERE, at("?edit=addresses&error=invalid_address"), addressDraft(false));
    checkoutPage(ELSEWHERE, at("?edit=contact"), form([["email", "x@y.z"]]));
    expect(ELSEWHERE).toEqual(before);
  });
});

describe("a placement the API refused is the review again with what was posted", () => {
  const placed = form([
    ["_pt", "token"],
    ["shipping", HOME],
    ["payment", "cod"],
    ["version", "3"],
    ["total_minor", "10000"],
    ["accept_terms", "1"],
    ["accept_withdrawal", "1"],
    ["review_invites", "1"],
    ["notes", " Zvonit dvakrát "],
  ]);

  test("keeps the ticks and the note and hands the refusal to the island", () => {
    expect(checkoutPage(BOTH, at("?error=cart_changed"), placed)).toEqual({
      kind: "review",
      problem: null,
      choices: {
        acceptTerms: true,
        acceptWithdrawal: true,
        emailMarketing: false,
        reviewInvites: true,
        notes: " Zvonit dvakrát ",
      },
      refusal: "cart_changed",
    });
  });

  test("only exactly 1 is a tick", () => {
    const page = checkoutPage(
      BOTH,
      at("?error=failed"),
      form([
        ["accept_terms", "on"],
        ["accept_withdrawal", "true"],
        ["email_marketing", "1"],
      ]),
    );
    expect(page).toEqual({
      kind: "review",
      problem: null,
      choices: {
        acceptTerms: false,
        acceptWithdrawal: false,
        emailMarketing: true,
        reviewInvites: false,
        notes: "",
      },
      refusal: "failed",
    });
  });

  test("a GET review keeps the page-level notice and no choices", () => {
    expect(checkoutPage(BOTH, at("?error=cart_changed"))).toEqual({
      kind: "review",
      problem: { step: null, field: null, message: "checkout.details_failed" },
      choices: null,
      refusal: null,
    });
  });
});

describe("addressLine", () => {
  test("joins the address with the country's name, company only when present", () => {
    expect(addressLine(JANA, region)).toBe(
      "Jana Nováková, Firma s.r.o., Dlouhá 12, 110 00 Praha, Česko",
    );
    for (const company of [null, ""])
      expect(addressLine({ ...JANA, company }, region)).toBe(
        "Jana Nováková, Dlouhá 12, 110 00 Praha, Česko",
      );
  });
});

describe("regionName", () => {
  test("names a country in the locale and keeps a code it cannot name", () => {
    const cs = regionName("cs");
    expect(cs("CZ")).toBe("Česko");
    for (const code of ["", "zz1", "<b>"]) expect(cs(code)).toBe(code);
    expect(regionName("not a locale!")("CZ")).toBe("CZ");
  });
});

describe("reviewFacts shows what stands next to the place button", () => {
  const point = {
    city: "Praha",
    country: "CZ",
    id: "123",
    name: "Z-BOX Praha 1",
    street: "Dlouhá 1",
    zip: "110 00",
  };

  test("a pickup method with its point", () => {
    expect(
      reviewFacts(
        view({
          ...ELSEWHERE,
          shipping_method_id: PICKUP,
          pickup_point: point,
          payment_method: "cod",
        }),
        region,
      ),
    ).toEqual({
      method: "Zásilkovna",
      destination: {
        label: "checkout.pickup_selected",
        line: "Z-BOX Praha 1, Dlouhá 1, 110 00 Praha",
      },
      payment: "Dobírka",
      email: "jana@example.test",
    });
  });

  test("a pickup method without a point has no destination", () => {
    expect(
      reviewFacts(view({ ...ELSEWHERE, shipping_method_id: PICKUP }), region).destination,
    ).toBeNull();
  });

  test("home delivery goes to the delivery address, else the billing address", () => {
    expect(
      reviewFacts(view({ ...ELSEWHERE, shipping_method_id: HOME }), region).destination,
    ).toEqual({
      label: "checkout.delivery_address",
      line: "Petr Novák, Krátká 2, 811 01 Bratislava, Slovensko",
    });
    expect(
      reviewFacts(
        view({ ...BOTH, shipping_method_id: HOME, payment_method: "bank_transfer" }),
        region,
      ),
    ).toEqual({
      method: "PPL domů",
      destination: {
        label: "checkout.delivery_address",
        line: "Jana Nováková, Firma s.r.o., Dlouhá 12, 110 00 Praha, Česko",
      },
      payment: "Bankovní převod",
      email: "jana@example.test",
    });
  });

  test("nothing chosen has no facts", () => {
    expect(reviewFacts(NOTHING, region)).toEqual({
      method: null,
      destination: null,
      payment: null,
      email: null,
    });
  });
});

describe("the new checkout messages", () => {
  const keys = [
    "checkout.save_continue",
    "checkout.confirmation_to",
    "checkout.details_failed",
    "checkout.stale_page",
    "checkout.payment_not_completed",
  ];
  const catalogs: [string, Record<string, string>][] = [
    ["en", en],
    ["cs", cs],
    ["sk", sk],
  ];

  test.each(catalogs)("%s has every key, formatted without a placeholder left", (_, m) => {
    for (const key of keys) {
      expect(m[key]?.trim(), key).toBeTruthy();
      expect(t(m, key, { email: "jana@example.test" }), key).not.toMatch(/[{}]/);
    }
    expect(t(m, "checkout.confirmation_to", { email: "jana@example.test" })).toContain(
      "jana@example.test",
    );
  });
});
