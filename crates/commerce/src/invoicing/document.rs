//! The invoice / credit note data model (spec §7.4, §10.7, A15, A17) and its pure builders.
//!
//! A [`Document`] is what `invoices.document` stores: structured, in minor units, so the PDF
//! and a later e-invoice (ISDOC) export are projections of it. Invoices are built from the
//! order's persisted allocations (A15: never re-priced); credit notes from the reversed
//! allocations of a refund ([`reverse`]).
//!
//! Tested with golden files (`tests/golden/*.json`) on this model, not on PDF bytes.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::money::{Currency, Locale, Money, div_round_half_up};
use crate::pricing::cart::{ChargeKind, ChargePortion};
use crate::tax::TaxRate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Invoice,
    CreditNote,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoice => "invoice",
            Self::CreditNote => "credit_note",
        }
    }

    /// Number prefix of the series (`{prefix}{YYYY}{seq:05}`).
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Invoice => "FV",
            Self::CreditNote => "DB",
        }
    }
}

/// The seller: legal entity (§14) + tax profile (A3). SK sellers carry DIČ and IČ DPH
/// separately (A17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Supplier {
    pub name: String,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
    /// IČO.
    pub company_id: String,
    /// DIČ.
    pub vat_id: Option<String>,
    /// SK IČ DPH.
    pub sk_ic_dph: Option<String>,
    pub registry: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Customer {
    pub name: String,
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BankAccountRef {
    pub iban: String,
    pub bic: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PaymentInfo {
    /// `stripe`, `bank_transfer`, `cod`, `fake`.
    pub method: String,
    /// The order number (A25).
    pub variable_symbol: String,
    /// Paid before the document was issued (prepaid invoices, refunds).
    pub paid: bool,
    pub bank_account: Option<BankAccountRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Goods,
    Shipping,
    PaymentFee,
    Rounding,
}

impl From<ChargeKind> for LineKind {
    fn from(k: ChargeKind) -> Self {
        match k {
            ChargeKind::Shipping => Self::Shipping,
            ChargeKind::PaymentFee => Self::PaymentFee,
            ChargeKind::Rounding => Self::Rounding,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DocLine {
    pub kind: LineKind,
    /// Goods lines: the order line (credit notes reverse its allocation).
    pub order_line_id: Option<Uuid>,
    pub name: String,
    pub sku: Option<String>,
    pub quantity: i32,
    /// Gross per unit (display; `gross_minor` is authoritative).
    pub unit_gross_minor: i64,
    pub gross_minor: i64,
    /// `None`: outside the VAT base (non-VAT payer, cash rounding by default).
    pub vat_rate: Option<TaxRate>,
    pub vat_minor: i64,
    pub net_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RecapRow {
    pub rate: TaxRate,
    pub net_minor: i64,
    pub vat_minor: i64,
    pub gross_minor: i64,
}

/// The ČNB fixing used for the statutory CZK recap (A17): CZK per `amount` units, × 1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Rate {
    pub currency: Currency,
    pub fixing_date: NaiveDate,
    pub amount: i32,
    pub rate_milli: i64,
}

impl Rate {
    /// Converts minor units of the document currency to CZK minor units (half up).
    pub fn to_czk(self, minor: i64) -> i64 {
        let d = 1000 * i128::from(self.amount.max(1));
        i64::try_from(div_round_half_up(
            i128::from(minor) * i128::from(self.rate_milli),
            d,
        ))
        .unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CzkRecap {
    pub rate: Rate,
    pub rows: Vec<RecapRow>,
    pub vat_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Totals {
    pub net_minor: i64,
    pub vat_minor: i64,
    pub gross_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OriginalRef {
    pub id: Uuid,
    pub number: String,
    pub issued_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Document {
    pub kind: DocumentKind,
    pub number: String,
    /// `cs`, `sk` or `en`.
    pub locale: String,
    pub order_number: String,
    pub issued_on: NaiveDate,
    /// DUZP (A17).
    pub taxable_supply_date: NaiveDate,
    pub due_on: NaiveDate,
    pub currency: Currency,
    pub vat_payer: bool,
    pub supplier: Supplier,
    pub customer: Customer,
    pub payment: PaymentInfo,
    pub lines: Vec<DocLine>,
    /// Per VAT rate, in the document currency. Empty for non-VAT payers.
    pub vat_recap: Vec<RecapRow>,
    /// Non-CZK documents of CZ VAT payers: the recap in CZK at the ČNB rate (A17).
    pub czk_recap: Option<CzkRecap>,
    pub totals: Totals,
    /// Credit notes: the invoice they correct.
    pub original: Option<OriginalRef>,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------------------
// Sources: the order as persisted (A15)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLine {
    pub id: Uuid,
    pub name: String,
    pub options_label: String,
    pub sku: String,
    pub quantity: i32,
    pub unit_gross_minor: i64,
    pub total_minor: i64,
    pub tax_rate: TaxRate,
    pub tax_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCharge {
    pub kind: ChargeKind,
    /// The shipping method's name for `shipping`.
    pub name: String,
    pub total_minor: i64,
    pub tax_minor: i64,
    pub portions: Vec<ChargePortion>,
}

/// Everything about the order a document needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderSource {
    pub number: String,
    pub locale: String,
    pub currency: Currency,
    pub vat_payer: bool,
    pub lines: Vec<SourceLine>,
    pub charges: Vec<SourceCharge>,
    pub customer: Customer,
}

/// Issue data decided by the caller (scenario, numbering, rate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: String,
    pub issued_on: NaiveDate,
    pub taxable_supply_date: NaiveDate,
    pub due_on: NaiveDate,
    pub supplier: Supplier,
    pub payment: PaymentInfo,
    /// Required for non-CZK documents of CZ VAT payers ([`needs_czk_recap`]).
    pub rate: Option<Rate>,
}

/// Whether a document in `currency` from this supplier needs the CZK recap (A17).
pub fn needs_czk_recap(vat_payer: bool, supplier_country: &str, currency: Currency) -> bool {
    vat_payer && supplier_country == "CZ" && currency != Currency::Czk
}

fn locale_key(locale: &str) -> &'static str {
    match Locale::from_tag(locale) {
        Locale::Cs => "cs",
        Locale::Sk => "sk",
        Locale::En => "en",
    }
}

/// Charge line names (shipping uses the method's own name).
fn charge_name(kind: ChargeKind, locale: &str, shipping: &str) -> String {
    match (kind, locale_key(locale)) {
        (ChargeKind::Shipping, _) => shipping.to_owned(),
        (ChargeKind::PaymentFee, "cs") => "Poplatek za způsob platby".into(),
        (ChargeKind::PaymentFee, "sk") => "Poplatok za spôsob platby".into(),
        (ChargeKind::PaymentFee, _) => "Payment fee".into(),
        (ChargeKind::Rounding, "cs") => "Zaokrouhlení".into(),
        (ChargeKind::Rounding, "sk") => "Zaokrúhlenie".into(),
        (ChargeKind::Rounding, _) => "Rounding".into(),
    }
}

fn line_name(l: &SourceLine) -> String {
    if l.options_label.is_empty() {
        l.name.clone()
    } else {
        format!("{} ({})", l.name, l.options_label)
    }
}

/// Doc lines of a charge: one per VAT portion (A15 step 5); a charge without VAT (rounding
/// outside the base, non-VAT payers) is one line outside the VAT base.
fn charge_lines(c: &SourceCharge, locale: &str, vat_payer: bool, sign: i64) -> Vec<DocLine> {
    let name = charge_name(c.kind, locale, &c.name);
    let taxed: Vec<&ChargePortion> = c
        .portions
        .iter()
        .filter(|p| p.gross_minor != 0 && vat_payer)
        .collect();
    if taxed.is_empty() || (c.kind == ChargeKind::Rounding && c.tax_minor == 0) {
        return vec![DocLine {
            kind: c.kind.into(),
            order_line_id: None,
            name,
            sku: None,
            quantity: 1,
            unit_gross_minor: sign * c.total_minor,
            gross_minor: sign * c.total_minor,
            vat_rate: None,
            vat_minor: 0,
            net_minor: sign * c.total_minor,
        }];
    }
    taxed
        .into_iter()
        .map(|p| DocLine {
            kind: c.kind.into(),
            order_line_id: None,
            name: name.clone(),
            sku: None,
            quantity: 1,
            unit_gross_minor: sign * p.gross_minor,
            gross_minor: sign * p.gross_minor,
            vat_rate: Some(p.tax_rate),
            vat_minor: sign * p.vat_minor,
            net_minor: sign * p.net_minor,
        })
        .collect()
}

/// VAT recap per rate from the lines (lines outside the VAT base are not part of it).
pub fn recap(lines: &[DocLine]) -> Vec<RecapRow> {
    let mut by_rate: BTreeMap<TaxRate, RecapRow> = BTreeMap::new();
    for l in lines {
        let Some(rate) = l.vat_rate else { continue };
        let row = by_rate.entry(rate).or_insert(RecapRow {
            rate,
            net_minor: 0,
            vat_minor: 0,
            gross_minor: 0,
        });
        row.net_minor += l.net_minor;
        row.vat_minor += l.vat_minor;
        row.gross_minor += l.gross_minor;
    }
    // Highest rate first, like the order recap.
    by_rate.into_values().rev().collect()
}

fn totals(lines: &[DocLine]) -> Totals {
    Totals {
        net_minor: lines.iter().map(|l| l.net_minor).sum(),
        vat_minor: lines.iter().map(|l| l.vat_minor).sum(),
        gross_minor: lines.iter().map(|l| l.gross_minor).sum(),
    }
}

fn czk_recap(rows: &[RecapRow], rate: Rate) -> CzkRecap {
    let rows: Vec<RecapRow> = rows
        .iter()
        .map(|r| {
            let net = rate.to_czk(r.net_minor);
            let vat = rate.to_czk(r.vat_minor);
            RecapRow {
                rate: r.rate,
                net_minor: net,
                vat_minor: vat,
                gross_minor: net + vat,
            }
        })
        .collect();
    CzkRecap {
        vat_minor: rows.iter().map(|r| r.vat_minor).sum(),
        rows,
        rate,
    }
}

fn assemble(
    kind: DocumentKind,
    src: &OrderSource,
    issue: Issue,
    lines: Vec<DocLine>,
    original: Option<OriginalRef>,
    reason: Option<String>,
) -> Document {
    let lines: Vec<DocLine> = if src.vat_payer {
        lines
    } else {
        // A17.3: a non-VAT payer charges no VAT; the whole gross is the price.
        lines
            .into_iter()
            .map(|l| DocLine {
                vat_rate: None,
                vat_minor: 0,
                net_minor: l.gross_minor,
                ..l
            })
            .collect()
    };
    let vat_recap = recap(&lines);
    let czk = issue
        .rate
        .filter(|_| needs_czk_recap(src.vat_payer, &issue.supplier.country, src.currency))
        .map(|r| czk_recap(&vat_recap, r));
    Document {
        kind,
        number: issue.number,
        locale: locale_key(&src.locale).to_owned(),
        order_number: src.number.clone(),
        issued_on: issue.issued_on,
        taxable_supply_date: issue.taxable_supply_date,
        due_on: issue.due_on,
        currency: src.currency,
        vat_payer: src.vat_payer,
        supplier: issue.supplier,
        customer: src.customer.clone(),
        payment: issue.payment,
        totals: totals(&lines),
        lines,
        vat_recap,
        czk_recap: czk,
        original,
        reason,
    }
}

/// The (final) invoice of an order: every line and charge as persisted.
pub fn invoice(src: &OrderSource, issue: Issue) -> Document {
    let mut lines: Vec<DocLine> = src
        .lines
        .iter()
        .map(|l| DocLine {
            kind: LineKind::Goods,
            order_line_id: Some(l.id),
            name: line_name(l),
            sku: Some(l.sku.clone()),
            quantity: l.quantity,
            unit_gross_minor: l.unit_gross_minor,
            gross_minor: l.total_minor,
            vat_rate: Some(l.tax_rate),
            vat_minor: l.tax_minor,
            net_minor: l.total_minor - l.tax_minor,
        })
        .collect();
    for c in &src.charges {
        lines.extend(charge_lines(c, &src.locale, src.vat_payer, 1));
    }
    assemble(DocumentKind::Invoice, src, issue, lines, None, None)
}

// ---------------------------------------------------------------------------------------
// Refund allocation reversal (A15)

/// A goods line to refund: `quantity` more units of an order line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundLine {
    pub order_line_id: Uuid,
    pub quantity: i32,
}

/// What earlier refunds of an order line already reversed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reversed {
    pub quantity: i32,
    pub gross_minor: i64,
    pub vat_minor: i64,
}

/// The reversed allocation of `k` more units of a line of `q` units with total `total` and
/// VAT `vat` (A15): proportional, rounded down (VAT half up); the last units take whatever is
/// left, so refunding a line in any number of steps returns exactly its persisted allocation.
pub fn reverse_units(q: i32, total: i64, vat: i64, done: Reversed, k: i32) -> (i64, i64) {
    if k <= 0 || q <= 0 {
        return (0, 0);
    }
    if done.quantity + k >= q {
        return (total - done.gross_minor, vat - done.vat_minor);
    }
    let (q, k) = (i128::from(q), i128::from(k));
    let upto = i128::from(done.quantity) + k;
    // Cumulative shares avoid drifting: this refund = share(upto) − what was refunded.
    let gross_upto = i128::from(total) * upto / q;
    let vat_upto = div_round_half_up(i128::from(vat) * upto, q);
    (
        i64::try_from(gross_upto).unwrap_or(total) - done.gross_minor,
        i64::try_from(vat_upto).unwrap_or(vat) - done.vat_minor,
    )
}

/// The credit note of a refund: `lines` are positive refunded allocations; the document
/// carries them negated.
pub fn credit_note(
    src: &OrderSource,
    issue: Issue,
    lines: Vec<DocLine>,
    original: OriginalRef,
    reason: Option<String>,
) -> Document {
    let negated = lines
        .into_iter()
        .map(|l| DocLine {
            unit_gross_minor: -l.unit_gross_minor,
            gross_minor: -l.gross_minor,
            vat_minor: -l.vat_minor,
            net_minor: -l.net_minor,
            ..l
        })
        .collect();
    assemble(
        DocumentKind::CreditNote,
        src,
        issue,
        negated,
        Some(original),
        reason,
    )
}

/// A refunded goods line as a (positive) document line.
pub fn refunded_goods_line(l: &SourceLine, quantity: i32, gross: i64, vat: i64) -> DocLine {
    DocLine {
        kind: LineKind::Goods,
        order_line_id: Some(l.id),
        name: line_name(l),
        sku: Some(l.sku.clone()),
        quantity,
        unit_gross_minor: if quantity > 0 {
            gross / i64::from(quantity)
        } else {
            gross
        },
        gross_minor: gross,
        vat_rate: Some(l.tax_rate),
        vat_minor: vat,
        net_minor: gross - vat,
    }
}

/// A refunded charge as (positive) document lines, whole.
pub fn refunded_charge_lines(c: &SourceCharge, locale: &str, vat_payer: bool) -> Vec<DocLine> {
    charge_lines(c, locale, vat_payer, 1)
}

// ---------------------------------------------------------------------------------------
// Render model for the Typst templates: every display string preformatted

fn date(d: NaiveDate, locale: &str) -> String {
    match locale_key(locale) {
        "en" => d.format("%-d %b %Y").to_string(),
        _ => d.format("%-d. %-m. %Y").to_string(),
    }
}

fn rate_label(r: TaxRate, locale: &str) -> String {
    let s = r.to_string();
    match locale_key(locale) {
        "en" => format!("{s}%"),
        _ => format!("{} %", s.replace('.', ",")),
    }
}

fn address(street: &str, postal_code: &str, city: &str, country: &str) -> Vec<String> {
    vec![
        street.to_owned(),
        format!("{postal_code} {city}"),
        country.to_owned(),
    ]
}

/// The JSON the invoice template reads (`data.json`).
pub fn render_model(d: &Document) -> Value {
    let loc = Locale::from_tag(&d.locale);
    let m = |minor: i64| Money::new(minor, d.currency).format(loc);
    let czk = |minor: i64| Money::new(minor, Currency::Czk).format(loc);
    let recap_rows = |rows: &[RecapRow], f: &dyn Fn(i64) -> String| -> Vec<Value> {
        rows.iter()
            .map(|r| {
                json!({ "rate": rate_label(r.rate, &d.locale), "net": f(r.net_minor),
                        "vat": f(r.vat_minor), "gross": f(r.gross_minor) })
            })
            .collect()
    };
    let s = &d.supplier;
    let c = &d.customer;
    let mut customer_address = Vec::new();
    if let Some(company) = c.company.as_ref().filter(|x| !x.is_empty()) {
        customer_address.push(company.clone());
    }
    customer_address.extend(address(&c.street, &c.postal_code, &c.city, &c.country));
    json!({
        "locale": d.locale,
        "kind": d.kind.as_str(),
        "number": d.number,
        "issued_on": date(d.issued_on, &d.locale),
        "taxable_supply_date": date(d.taxable_supply_date, &d.locale),
        "due_on": date(d.due_on, &d.locale),
        "vat_payer": d.vat_payer,
        "currency": d.currency.code(),
        "order_number": d.order_number,
        "variable_symbol": d.payment.variable_symbol,
        "payment_method": d.payment.method,
        "paid": d.payment.paid,
        "bank_account": d.payment.bank_account,
        "supplier": {
            "name": s.name,
            "address": address(&s.street, &s.postal_code, &s.city, &s.country),
            "company_id": s.company_id,
            "vat_id": s.vat_id,
            "sk_ic_dph": s.sk_ic_dph,
            "registry": s.registry,
            "email": s.email,
            "phone": s.phone,
        },
        "customer": { "name": c.name, "address": customer_address, "email": c.email },
        "original": d.original.as_ref().map(|o| json!({
            "number": o.number, "issued_on": date(o.issued_on, &d.locale) })),
        "reason": d.reason,
        "lines": d.lines.iter().map(|l| json!({
            "name": l.name,
            "sku": l.sku.clone().unwrap_or_default(),
            "quantity": l.quantity.to_string(),
            "unit_price": m(l.unit_gross_minor),
            "vat_rate": l.vat_rate.map(|r| rate_label(r, &d.locale)).unwrap_or_default(),
            "net": m(l.net_minor),
            "vat": m(l.vat_minor),
            "total": m(l.gross_minor),
        })).collect::<Vec<_>>(),
        "vat_recap": recap_rows(&d.vat_recap, &m),
        "czk_recap": d.czk_recap.as_ref().map(|r| json!({
            "rate": format!("{},{:03}", r.rate.rate_milli / 1000, r.rate.rate_milli % 1000),
            "amount": r.rate.amount.to_string(),
            "currency": r.rate.currency.code(),
            "rate_date": date(r.rate.fixing_date, &d.locale),
            "rows": recap_rows(&r.rows, &czk),
            "vat": czk(r.vat_minor),
        })),
        "totals": { "net": m(d.totals.net_minor), "vat": m(d.totals.vat_minor),
                    "gross": m(d.totals.gross_minor) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversal_returns_exactly_the_allocation_in_any_steps() {
        // 3 units, total 100.00 after a coupon, VAT 17.36 (21 %).
        let (q, total, vat) = (3, 10_000, 1_736);
        let mut done = Reversed::default();
        let mut parts = vec![];
        for k in [1, 1, 1] {
            let (g, v) = reverse_units(q, total, vat, done, k);
            parts.push((g, v));
            done = Reversed {
                quantity: done.quantity + k,
                gross_minor: done.gross_minor + g,
                vat_minor: done.vat_minor + v,
            };
        }
        assert_eq!(parts, vec![(3_333, 579), (3_333, 578), (3_334, 579)]);
        assert_eq!((done.gross_minor, done.vat_minor), (total, vat));
        // All at once = the persisted allocation; nothing more after that.
        assert_eq!(
            reverse_units(q, total, vat, Reversed::default(), 3),
            (total, vat)
        );
        assert_eq!(reverse_units(q, total, vat, done, 0), (0, 0));
    }

    #[test]
    fn czk_conversion_rounds_half_up_per_amount() {
        let eur = Rate {
            currency: Currency::Eur,
            fixing_date: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap_or_default(),
            amount: 1,
            rate_milli: 24_305,
        };
        // €12.90 × 24.305 = 313.5345 CZK
        assert_eq!(eur.to_czk(1_290), 31_353);
        let huf = Rate {
            amount: 100,
            rate_milli: 6_321,
            ..eur
        };
        // 1000.00 HUF × 6.321 / 100 = 63.21 CZK
        assert_eq!(huf.to_czk(100_000), 6_321);
        assert_eq!(eur.to_czk(-1_290), -31_353);
    }

    #[test]
    fn czk_recap_only_for_foreign_currency_documents_of_cz_vat_payers() {
        assert!(needs_czk_recap(true, "CZ", Currency::Eur));
        assert!(!needs_czk_recap(true, "CZ", Currency::Czk));
        assert!(!needs_czk_recap(false, "CZ", Currency::Eur));
        assert!(!needs_czk_recap(true, "SK", Currency::Eur));
    }
}
