import { describe, expect, it } from "vitest";
import { guessMapping, headerRow, missingRequired } from "./portability.ts";

describe("headerRow", () => {
  it("reads comma and semicolon headers with quotes and a BOM", () => {
    expect(headerRow("﻿Email,Name\r\na@x.cz,Anna")).toEqual(["Email", "Name"]);
    expect(headerRow('"E-mail";"Jméno; příjmení";Telefon\n1;2;3')).toEqual([
      "E-mail",
      "Jméno; příjmení",
      "Telefon",
    ]);
    expect(headerRow('a,"say ""hi""",c')).toEqual(["a", 'say "hi"', "c"]);
    expect(headerRow("")).toEqual([]);
  });
});

describe("guessMapping", () => {
  it("maps same-named columns and reports missing required fields", () => {
    const m = guessMapping("orders", ["Order_Number", "EMAIL", "Total", "Poznámka"]);
    expect(m).toEqual({ order_number: "Order_Number", email: "EMAIL", total: "Total" });
    expect(missingRequired("orders", m)).toEqual(["placed_at", "currency"]);
    expect(missingRequired("subscribers", { email: "E-mail" })).toEqual([]);
  });
});
