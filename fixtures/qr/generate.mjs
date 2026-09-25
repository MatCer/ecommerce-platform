// Golden vectors for PAY by square 1.2.0 (spec A25), generated ONCE with the reference
// `bysquare` npm library and committed as `paybysquare.json`. `commerce::payments::qr` must
// reproduce every `qr` string byte for byte.
//
// Regenerate (only when adding cases; the library version is pinned below):
//   dir=$(mktemp -d) && (cd "$dir" && pnpm init >/dev/null && pnpm add bysquare@4.0.2)
//   node fixtures/qr/generate.mjs "$dir/node_modules/bysquare" > fixtures/qr/paybysquare.json
import { pathToFileURL } from "node:url";
import { join } from "node:path";

const root = process.argv[2];
if (!root) throw new Error("usage: node generate.mjs <path to node_modules/bysquare>");
const pay = await import(pathToFileURL(join(root, "lib/pay/index.js")).href);

// Inputs mirror what the shop sends: amount in minor units, one payment order, one account.
const cases = [
  {
    name: "basic",
    amount_minor: 1290,
    iban: "SK9611000000002918599669",
    bic: "TATRSKBX",
    variable_symbol: "100001",
    note: "Objednávka 100001",
    beneficiary_name: "Demo obchod s.r.o.",
  },
  {
    name: "whole_amount_without_bic",
    amount_minor: 10000,
    iban: "SK3112000000198742637541",
    bic: null,
    variable_symbol: "100002",
    note: "Objednavka 100002",
    beneficiary_name: "Demo",
  },
  {
    name: "diacritics_tabs_and_star",
    amount_minor: 4250,
    iban: "SK9611000000002918599669",
    bic: "TATRSKBXXXX",
    variable_symbol: "123",
    note: "Ľubovoľná\tpoznámka * čšťžýáíé",
    beneficiary_name: "Ľubomír Šťastný – Žilina",
  },
  {
    name: "large_amount_long_note",
    amount_minor: 12345678,
    iban: "SK0809000000000123123123",
    bic: "GIBASKBX",
    variable_symbol: "9999999999",
    note: "Platba za objednávku číslo 9999999999 v obchode Demo obchod",
    beneficiary_name: "Veľmi dlhý názov obchodníka s.r.o.",
  },
  {
    name: "five_cents",
    amount_minor: 5,
    iban: "SK9611000000002918599669",
    bic: "TATRSKBX",
    variable_symbol: "100003",
    note: "",
    beneficiary_name: "Demo",
  },
];

const out = cases.map((c) => {
  const model = {
    payments: [
      {
        type: pay.PaymentOptions.PaymentOrder,
        amount: c.amount_minor / 100,
        currencyCode: "EUR",
        variableSymbol: c.variable_symbol,
        paymentNote: c.note || undefined,
        bankAccounts: [c.bic ? { iban: c.iban, bic: c.bic } : { iban: c.iban }],
        beneficiary: { name: c.beneficiary_name },
      },
    ],
  };
  const qr = pay.encode(structuredClone(model));
  pay.removeDiacritics(model);
  return { ...c, payload: pay.serialize(model), qr };
});
console.log(JSON.stringify({ generator: "bysquare@4.0.2", version: "1.2.0", cases: out }, null, 2));
