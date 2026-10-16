//! Invoice data model (A15, A17): golden files of the structured documents (not PDF bytes) for
//! each scenario, and one PDF render smoke test through the Typst CLI.
//!
//! Regenerate the golden files after an intended change with `UPDATE_GOLDEN=1 cargo test -p
//! commerce --test invoicing` and review the diff.
#![allow(clippy::unwrap_used)]

use chrono::NaiveDate;
use commerce::invoicing::document::{
    self, BankAccountRef, Customer, Document, Issue, OrderSource, OriginalRef, PaymentInfo, Rate,
    Reversed, SourceCharge, SourceLine, Supplier, refunded_charge_lines, refunded_goods_line,
    reverse_units,
};
use commerce::money::Currency;
use commerce::pricing::cart::{ChargeKind, ChargePortion};
use commerce::tax::TaxRate;
use uuid::Uuid;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn rate(r: &str) -> TaxRate {
    r.parse().unwrap()
}

fn cz_supplier() -> Supplier {
    Supplier {
        name: "Demo obchod s.r.o.".into(),
        street: "Dlouhá 12".into(),
        city: "Praha 1".into(),
        postal_code: "110 00".into(),
        country: "CZ".into(),
        company_id: "12345678".into(),
        vat_id: Some("CZ12345678".into()),
        sk_ic_dph: None,
        registry: Some("C 12345 vedená u Městského soudu v Praze".into()),
        email: Some("obchod@example.cz".into()),
        phone: None,
    }
}

fn customer(country: &str) -> Customer {
    Customer {
        name: "Jana Nováková".into(),
        company: None,
        street: "Krátká 8".into(),
        city: "Brno".into(),
        postal_code: "602 00".into(),
        country: country.into(),
        email: "jana@example.test".into(),
    }
}

fn portion(r: &str, gross: i64, vat: i64) -> ChargePortion {
    ChargePortion {
        tax_rate: rate(r),
        gross_minor: gross,
        vat_minor: vat,
        net_minor: gross - vat,
    }
}

/// Two T-shirts (21 %, a 20 CZK coupon share) and a mug (12 %); shipping split across the
/// goods' rates (A15 step 5).
fn cz_order() -> OrderSource {
    OrderSource {
        number: "100001".into(),
        locale: "cs-CZ".into(),
        currency: Currency::Czk,
        vat_payer: true,
        lines: vec![
            SourceLine {
                id: id(1),
                name: "Bavlněné tričko".into(),
                options_label: "modrá, M".into(),
                sku: "TEE-BLU-M".into(),
                quantity: 2,
                unit_gross_minor: 12_900,
                total_minor: 23_800,
                tax_rate: rate("21"),
                tax_minor: 4_131,
            },
            SourceLine {
                id: id(2),
                name: "Keramický hrnek".into(),
                options_label: String::new(),
                sku: "MUG-01".into(),
                quantity: 1,
                unit_gross_minor: 24_900,
                total_minor: 24_900,
                tax_rate: rate("12"),
                tax_minor: 2_668,
            },
        ],
        charges: vec![SourceCharge {
            kind: ChargeKind::Shipping,
            name: "Zásilkovna – výdejní místo".into(),
            total_minor: 7_900,
            tax_minor: 1_103,
            portions: vec![portion("21", 3_861, 670), portion("12", 4_039, 433)],
        }],
        customer: customer("CZ"),
    }
}

fn bank() -> Option<BankAccountRef> {
    Some(BankAccountRef {
        iban: "CZ6508000000192000145399".into(),
        bic: Some("GIBACZPX".into()),
        name: "Demo obchod s.r.o.".into(),
    })
}

fn issue(number: &str, duzp: NaiveDate, method: &str, paid: bool, rate: Option<Rate>) -> Issue {
    Issue {
        number: number.into(),
        issued_on: d(2026, 9, 25),
        taxable_supply_date: duzp,
        due_on: d(2026, 9, 25),
        supplier: cz_supplier(),
        payment: PaymentInfo {
            method: method.into(),
            variable_symbol: "100001".into(),
            paid,
            bank_account: bank(),
        },
        rate,
    }
}

fn golden(name: &str, doc: &Document) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    let actual = serde_json::to_string_pretty(doc).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {}: run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(actual, expected, "golden file {name} differs");
}

fn eur_rate() -> Rate {
    Rate {
        currency: Currency::Eur,
        fixing_date: d(2026, 9, 24),
        amount: 1,
        rate_milli: 24_305,
    }
}

fn assert_consistent(doc: &Document) {
    let sum = |f: fn(&document::DocLine) -> i64| doc.lines.iter().map(f).sum::<i64>();
    assert_eq!(doc.totals.gross_minor, sum(|l| l.gross_minor));
    assert_eq!(doc.totals.vat_minor, sum(|l| l.vat_minor));
    assert_eq!(
        doc.totals.net_minor + doc.totals.vat_minor,
        doc.totals.gross_minor
    );
    let recap_vat: i64 = doc.vat_recap.iter().map(|r| r.vat_minor).sum();
    assert_eq!(recap_vat, doc.totals.vat_minor, "the recap covers all VAT");
}

#[test]
fn prepaid_invoice_is_dated_on_the_payment_and_matches_the_allocations() {
    let src = cz_order();
    let doc = document::invoice(
        &src,
        issue("FV202600001", d(2026, 9, 24), "bank_transfer", true, None),
    );
    assert_consistent(&doc);
    // A15: the invoice is the persisted allocation, never re-priced.
    assert_eq!(doc.totals.gross_minor, 23_800 + 24_900 + 7_900);
    assert_eq!(doc.totals.vat_minor, 4_131 + 2_668 + 1_103);
    assert_eq!(doc.taxable_supply_date, d(2026, 9, 24));
    assert_eq!(doc.vat_recap.len(), 2);
    assert!(doc.czk_recap.is_none());
    golden("invoice-prepaid-czk", &doc);
}

#[test]
fn cod_invoice_is_dated_on_dispatch_with_the_payment_fee() {
    let mut src = cz_order();
    src.charges.push(SourceCharge {
        kind: ChargeKind::PaymentFee,
        name: String::new(),
        total_minor: 3_900,
        tax_minor: 548,
        portions: vec![portion("21", 1_906, 331), portion("12", 1_994, 214)],
    });
    let doc = document::invoice(
        &src,
        issue("FV202600002", d(2026, 9, 25), "cod", false, None),
    );
    assert_consistent(&doc);
    assert!(!doc.payment.paid);
    golden("invoice-cod-czk", &doc);
}

#[test]
fn eur_document_of_a_cz_vat_payer_has_the_czk_recap() {
    let mut src = cz_order();
    src.currency = Currency::Eur;
    src.locale = "sk-SK".into();
    src.customer = customer("SK");
    for l in &mut src.lines {
        l.unit_gross_minor /= 25;
        l.total_minor /= 25;
        l.tax_minor /= 25;
    }
    src.charges[0] = SourceCharge {
        kind: ChargeKind::Shipping,
        name: "Packeta – výdajné miesto".into(),
        total_minor: 290,
        tax_minor: 41,
        portions: vec![portion("21", 142, 25), portion("12", 148, 16)],
    };
    let doc = document::invoice(
        &src,
        issue(
            "FV202600003",
            d(2026, 9, 24),
            "stripe",
            true,
            Some(eur_rate()),
        ),
    );
    assert_consistent(&doc);
    let czk = doc.czk_recap.as_ref().unwrap();
    assert_eq!(czk.rate.rate_milli, 24_305);
    for (eur, czk_row) in doc.vat_recap.iter().zip(&czk.rows) {
        assert_eq!(czk_row.vat_minor, eur_rate().to_czk(eur.vat_minor));
    }
    golden("invoice-eur-czk-recap", &doc);
}

#[test]
fn slovak_supplier_shows_dic_and_ic_dph_and_needs_no_czk_recap() {
    let mut src = cz_order();
    src.currency = Currency::Eur;
    src.locale = "sk".into();
    let mut i = issue(
        "FV202600001",
        d(2026, 9, 24),
        "bank_transfer",
        true,
        Some(eur_rate()),
    );
    i.supplier = Supplier {
        name: "Demo obchod s.r.o.".into(),
        street: "Obchodná 1".into(),
        city: "Bratislava".into(),
        postal_code: "811 06".into(),
        country: "SK".into(),
        company_id: "50123456".into(),
        vat_id: Some("2120123456".into()),
        sk_ic_dph: Some("SK2120123456".into()),
        registry: Some("Obchodný register Mestského súdu Bratislava III, vložka 12345/B".into()),
        email: None,
        phone: None,
    };
    let doc = document::invoice(&src, i);
    assert!(doc.czk_recap.is_none(), "only CZ VAT payers recap in CZK");
    assert_eq!(doc.supplier.sk_ic_dph.as_deref(), Some("SK2120123456"));
    golden("invoice-sk-supplier", &doc);
}

#[test]
fn non_vat_payer_invoices_without_vat() {
    let mut src = cz_order();
    src.vat_payer = false;
    let doc = document::invoice(
        &src,
        issue(
            "FV202600004",
            d(2026, 9, 24),
            "bank_transfer",
            true,
            Some(eur_rate()),
        ),
    );
    assert!(doc.vat_recap.is_empty() && doc.czk_recap.is_none());
    assert!(
        doc.lines
            .iter()
            .all(|l| l.vat_rate.is_none() && l.vat_minor == 0)
    );
    assert_eq!(doc.totals.gross_minor, doc.totals.net_minor);
    golden("invoice-non-vat-payer", &doc);
}

#[test]
fn partial_credit_note_reverses_the_original_allocations() {
    let src = cz_order();
    let tee = &src.lines[0];
    // One of the two T-shirts, then the other one and the shipping.
    let (g1, v1) = reverse_units(
        tee.quantity,
        tee.total_minor,
        tee.tax_minor,
        Reversed::default(),
        1,
    );
    assert_eq!((g1, v1), (11_900, 2_066));
    let first = document::credit_note(
        &src,
        issue("DB202600001", d(2026, 9, 25), "bank_transfer", true, None),
        vec![refunded_goods_line(tee, 1, g1, v1)],
        OriginalRef {
            id: id(9),
            number: "FV202600001".into(),
            issued_on: d(2026, 9, 24),
        },
        Some("withdrawal".into()),
    );
    assert_consistent(&first);
    assert_eq!(first.totals.gross_minor, -11_900);
    golden("credit-note-partial", &first);

    let done = Reversed {
        quantity: 1,
        gross_minor: g1,
        vat_minor: v1,
    };
    let (g2, v2) = reverse_units(tee.quantity, tee.total_minor, tee.tax_minor, done, 1);
    // The residual goes to the last unit: both refunds return exactly the allocation.
    assert_eq!((g1 + g2, v1 + v2), (tee.total_minor, tee.tax_minor));
    let mut lines = vec![refunded_goods_line(tee, 1, g2, v2)];
    lines.extend(refunded_charge_lines(&src.charges[0], &src.locale, true));
    let second = document::credit_note(
        &src,
        issue("DB202600002", d(2026, 9, 26), "bank_transfer", true, None),
        lines,
        OriginalRef {
            id: id(9),
            number: "FV202600001".into(),
            issued_on: d(2026, 9, 24),
        },
        None,
    );
    assert_consistent(&second);
    assert_eq!(second.totals.gross_minor, -(g2 + 7_900));
}

/// The one PDF smoke test: the worker's renderer and the invoice template produce a PDF.
/// Needs the Typst CLI (`TYPST_BIN` or `typst` on PATH; CI installs it).
#[tokio::test]
async fn invoice_pdf_renders() {
    let typst = commerce::documents::Typst {
        bin: std::env::var("TYPST_BIN")
            .unwrap_or_else(|_| "typst".into())
            .into(),
    };
    let doc = document::invoice(
        &cz_order(),
        issue("FV202600001", d(2026, 9, 24), "bank_transfer", true, None),
    );
    let pdf = typst
        .render(
            commerce::documents::Template::Invoice,
            &document::render_model(&doc),
            &[],
        )
        .await
        .expect("typst renders the invoice (is the Typst CLI installed?)");
    assert!(pdf.starts_with(b"%PDF-"), "not a PDF");
    assert!(pdf.len() > 5_000);
}

#[test]
fn partial_credit_notes_add_up_to_the_original_czk_recap() {
    let mut src = cz_order();
    src.currency = Currency::Eur;
    src.charges.clear();
    src.lines = vec![SourceLine {
        id: id(7),
        name: "Odznak".into(),
        options_label: String::new(),
        sku: "PIN".into(),
        quantity: 2,
        unit_gross_minor: 6,
        total_minor: 12,
        tax_rate: rate("21"),
        tax_minor: 2,
    }];
    let inv = document::invoice(
        &src,
        issue(
            "FV202600009",
            d(2026, 9, 24),
            "stripe",
            true,
            Some(eur_rate()),
        ),
    );
    let orig = inv.czk_recap.clone().unwrap();
    // €0.02 VAT × 24.305 = CZK 0.4861 → 0.49; each €0.01 alone → 0.24.
    assert_eq!(orig.rows[0].vat_minor, 49);
    let line = src.lines[0].clone();
    let original = OriginalRef {
        id: id(9),
        number: "FV202600009".into(),
        issued_on: d(2026, 9, 24),
    };
    let note = |n: &str, done: Reversed, prior: &[Document]| {
        let (g, v) = reverse_units(2, 12, 2, done, 1);
        let mut cn = document::credit_note(
            &src,
            issue(n, d(2026, 9, 25), "stripe", true, Some(eur_rate())),
            vec![refunded_goods_line(&line, 1, g, v)],
            original.clone(),
            None,
        );
        let mut czk = cn.czk_recap.clone().unwrap();
        document::settle_czk_residual(&inv, &orig, prior, &cn.vat_recap, &mut czk);
        cn.czk_recap = Some(czk);
        (cn, g, v)
    };
    let (first, g1, v1) = note("DB202600001", Reversed::default(), &[]);
    let done = Reversed {
        quantity: 1,
        gross_minor: g1,
        vat_minor: v1,
    };
    let (second, _, _) = note("DB202600002", done, std::slice::from_ref(&first));
    let czk = |d: &Document| d.czk_recap.as_ref().unwrap().rows[0].clone();
    assert_eq!(-(czk(&first).vat_minor + czk(&second).vat_minor), 49);
    assert_eq!(
        -(czk(&first).net_minor + czk(&second).net_minor),
        orig.rows[0].net_minor
    );
}
