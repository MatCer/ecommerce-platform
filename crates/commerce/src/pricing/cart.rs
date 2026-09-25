//! The cart pricing engine (spec §10.1, A15, A16): one pure function, `price_cart`.
//!
//! Order of operations (A15), all in integer minor units:
//! 1. line base gross = effective unit price × quantity (sales are already in the unit price);
//! 2. the coupon's goods discount is allocated to the lines proportionally to their base
//!    gross with the largest-remainder method;
//! 3. VAT per line on the discounted gross: `round_half_up(gross × r / (100 + r))`;
//! 4. shipping and payment fees are ancillary: their gross is split across the goods' VAT
//!    rates proportionally to the goods' discounted gross per rate (largest remainder) and
//!    VAT is computed per portion;
//! 5. cash rounding last, as its own charge; outside the VAT base unless configured.
//!
//! The cart (WP6) and order placement (WP10) build [`CartInput`] from effective prices
//! (`pricing::effective_prices`), VAT liabilities (`tax::resolve`) and a coupon evaluated by
//! `promotions::coupons::evaluate`, then persist the resulting allocations on the order.

use std::collections::BTreeMap;

use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::markets::invalid;
use crate::money::{Currency, allocate, div_round_half_up, to_minor};
use crate::tax::{TaxRate, vat_from_gross};

/// Upper bound for any single amount (10^12 minor units); keeps every product in range.
pub const MAX_AMOUNT: i64 = 1_000_000_000_000;
pub const MAX_LINES: usize = 500;
pub const MAX_QUANTITY: u32 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CartInput {
    pub currency: Currency,
    /// A non-VAT-payer charges no VAT: every VAT amount is zero and there is no recap.
    pub vat_payer: bool,
    pub lines: Vec<LineInput>,
    pub coupon: Option<AppliedCoupon>,
    /// Shipping gross (before a free-shipping coupon).
    pub shipping_minor: Option<i64>,
    pub payment_fee_minor: Option<i64>,
    /// Rate for ancillary fees when there is no goods value to split by (e.g. a 100 % coupon):
    /// the liable country's standard rate.
    pub fallback_rate: TaxRate,
    pub cash_rounding: Option<CashRounding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LineInput {
    /// The cart line id, echoed in the result.
    pub id: Uuid,
    pub quantity: u32,
    /// Effective gross unit price (sales applied), from the price intervals.
    pub unit_price_minor: i64,
    pub tax_rate: TaxRate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppliedCoupon {
    pub code: String,
    pub discount: CouponDiscount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CouponDiscount {
    /// Percent of the goods, in basis points (1500 = 15 %).
    Percent { basis_points: u32 },
    /// A fixed amount off the goods (capped at the goods total).
    Fixed { amount_minor: i64 },
    /// Zeroes the shipping charge.
    FreeShipping,
}

/// Cash rounding (A16): the total is rounded half up to a multiple of `increment_minor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CashRounding {
    pub increment_minor: i64,
    /// Whether the rounding difference is part of the VAT base (split like a fee).
    pub in_vat_base: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tender {
    Cash,
    Card,
    Unknown,
}

/// The cash rounding rule for a payment (A16): only cash is rounded; CZK to whole koruna,
/// EUR in Slovakia to €0.05. `in_vat_base` comes from the tax profile.
///
/// Accountant review: the default (rounding outside the VAT base) is standard CZ practice;
/// confirm per jurisdiction before enabling `in_vat_base`.
pub fn cash_rounding(
    currency: Currency,
    country: &str,
    tender: Tender,
    in_vat_base: bool,
) -> Option<CashRounding> {
    if tender != Tender::Cash {
        return None;
    }
    let increment_minor = match (currency, country) {
        (Currency::Czk, _) => 100,
        (Currency::Eur, "SK") => 5,
        _ => return None,
    };
    Some(CashRounding {
        increment_minor,
        in_vat_base,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PricedCart {
    pub currency: Currency,
    pub lines: Vec<PricedLine>,
    /// Shipping, payment fee and cash rounding, in that order, when present.
    pub charges: Vec<PricedCharge>,
    /// Per-rate totals of everything in the VAT base, ascending by rate.
    pub vat_recap: Vec<VatRecapRow>,
    pub coupon_code: Option<String>,
    /// Goods after the coupon.
    pub goods_minor: i64,
    /// Coupon discount on goods and shipping.
    pub discount_minor: i64,
    pub vat_minor: i64,
    pub total_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PricedLine {
    pub id: Uuid,
    pub quantity: u32,
    pub unit_price_minor: i64,
    pub tax_rate: TaxRate,
    /// unit price × quantity.
    pub base_minor: i64,
    /// This line's share of the coupon.
    pub discount_minor: i64,
    /// base − discount: what the customer pays for the line, VAT included.
    pub gross_minor: i64,
    pub vat_minor: i64,
    pub net_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    Shipping,
    PaymentFee,
    Rounding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PricedCharge {
    pub kind: ChargeKind,
    pub base_minor: i64,
    pub discount_minor: i64,
    /// Payable gross; may be negative for a rounding charge.
    pub gross_minor: i64,
    pub vat_minor: i64,
    pub net_minor: i64,
    /// The gross split across VAT rates (empty when outside the VAT base).
    pub portions: Vec<ChargePortion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChargePortion {
    pub tax_rate: TaxRate,
    pub gross_minor: i64,
    pub vat_minor: i64,
    pub net_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VatRecapRow {
    pub tax_rate: TaxRate,
    pub net_minor: i64,
    pub vat_minor: i64,
    pub gross_minor: i64,
}

fn overflow() -> Error {
    invalid("amount_overflow", "an amount is out of range")
}

fn check_amount(field: &str, v: i64) -> Result<(), Error> {
    if (0..=MAX_AMOUNT).contains(&v) {
        Ok(())
    } else {
        Err(invalid(
            "invalid_cart",
            format!("{field} must be between 0 and {MAX_AMOUNT}"),
        ))
    }
}

impl CartInput {
    fn validate(&self) -> Result<(), Error> {
        if self.lines.len() > MAX_LINES {
            return Err(invalid(
                "invalid_cart",
                format!("at most {MAX_LINES} lines"),
            ));
        }
        for l in &self.lines {
            if !(1..=MAX_QUANTITY).contains(&l.quantity) {
                return Err(invalid(
                    "invalid_cart",
                    format!("quantity must be 1-{MAX_QUANTITY}"),
                ));
            }
            check_amount("unit_price_minor", l.unit_price_minor)?;
        }
        check_amount("shipping_minor", self.shipping_minor.unwrap_or(0))?;
        check_amount("payment_fee_minor", self.payment_fee_minor.unwrap_or(0))?;
        match self.coupon.as_ref().map(|c| c.discount) {
            Some(CouponDiscount::Percent { basis_points })
                if !(1..=10_000).contains(&basis_points) =>
            {
                return Err(invalid("invalid_cart", "coupon percent must be 0.01-100 %"));
            }
            Some(CouponDiscount::Fixed { amount_minor }) if amount_minor <= 0 => {
                return Err(invalid("invalid_cart", "coupon amount must be positive"));
            }
            Some(CouponDiscount::Fixed { amount_minor }) => check_amount("coupon", amount_minor)?,
            _ => {}
        }
        if self
            .cash_rounding
            .is_some_and(|r| !(1..=10_000).contains(&r.increment_minor))
        {
            return Err(invalid(
                "invalid_cart",
                "rounding increment must be 1-10000",
            ));
        }
        Ok(())
    }
}

/// Splits a (possibly negative) gross over `weights` with the largest-remainder method.
fn split_signed(gross: i64, weights: &[i64]) -> Option<Vec<i64>> {
    let parts = allocate(gross.checked_abs()?, weights)?;
    Some(if gross < 0 {
        parts.into_iter().map(|p| -p).collect()
    } else {
        parts
    })
}

/// A15 step 4/5: an ancillary charge's gross split across the goods' rates.
fn portions(
    gross: i64,
    goods_by_rate: &BTreeMap<TaxRate, i64>,
    fallback: TaxRate,
) -> Result<Vec<ChargePortion>, Error> {
    if gross == 0 {
        return Ok(vec![]);
    }
    let (rates, weights): (Vec<TaxRate>, Vec<i64>) = goods_by_rate
        .iter()
        .filter(|(_, g)| **g > 0)
        .map(|(r, g)| (*r, *g))
        .unzip();
    let split = if weights.is_empty() {
        vec![(fallback, gross)]
    } else {
        rates
            .into_iter()
            .zip(split_signed(gross, &weights).ok_or_else(overflow)?)
            .collect()
    };
    Ok(split
        .into_iter()
        .filter(|(_, g)| *g != 0)
        .map(|(rate, g)| {
            let vat = vat_from_gross(g, rate);
            ChargePortion {
                tax_rate: rate,
                gross_minor: g,
                vat_minor: vat,
                net_minor: g - vat,
            }
        })
        .collect())
}

fn charge(
    kind: ChargeKind,
    base: i64,
    discount: i64,
    in_vat_base: bool,
    goods_by_rate: &BTreeMap<TaxRate, i64>,
    fallback: TaxRate,
) -> Result<PricedCharge, Error> {
    let gross = base - discount;
    let portions = if in_vat_base {
        portions(gross, goods_by_rate, fallback)?
    } else {
        vec![]
    };
    let vat: i64 = portions.iter().map(|p| p.vat_minor).sum();
    Ok(PricedCharge {
        kind,
        base_minor: base,
        discount_minor: discount,
        gross_minor: gross,
        vat_minor: vat,
        net_minor: gross - vat,
        portions,
    })
}

/// `total` (>= 0) rounded half up to a multiple of `increment`.
fn round_to(total: i64, increment: i64) -> i64 {
    (total + increment / 2) / increment * increment
}

/// The cash rounding charge for a payable `subtotal` (A15 step 6, A16), or `None` when it is
/// already a multiple of the increment. Also used at COD collection, when the tender becomes
/// known after the order was priced: `goods_by_rate` are the goods' discounted gross per rate.
pub fn rounding_charge(
    subtotal: i64,
    rule: CashRounding,
    goods_by_rate: &BTreeMap<TaxRate, i64>,
    fallback: TaxRate,
) -> Result<Option<PricedCharge>, Error> {
    if !(1..=10_000).contains(&rule.increment_minor) || !(0..=MAX_AMOUNT * 10).contains(&subtotal) {
        return Err(overflow());
    }
    // A positive payment never rounds to zero: it is at least one increment (SK: €0.05 for
    // €0.01-0.02, MF SR guidance on rounding from 2022-07-01).
    let rounded = match round_to(subtotal, rule.increment_minor) {
        0 if subtotal > 0 => rule.increment_minor,
        rounded => rounded,
    };
    let diff = rounded - subtotal;
    if diff == 0 {
        return Ok(None);
    }
    charge(
        ChargeKind::Rounding,
        diff,
        0,
        rule.in_vat_base,
        goods_by_rate,
        fallback,
    )
    .map(Some)
}

/// Prices a cart per A15. Pure and deterministic: the same input always gives the same
/// allocation. Errors: `422 invalid_cart` for out-of-range input.
pub fn price_cart(input: &CartInput) -> Result<PricedCart, Error> {
    input.validate()?;
    let rate_of = |r: TaxRate| if input.vat_payer { r } else { TaxRate::ZERO };

    // 1. Line base gross. Bounded inputs: 10^12 × 10^4 × 500 fits in i64.
    let bases: Vec<i64> = input
        .lines
        .iter()
        .map(|l| l.unit_price_minor * i64::from(l.quantity))
        .collect();
    let goods_base: i64 = bases.iter().sum();

    // 2. Coupon discount on goods, allocated by largest remainder.
    let goods_discount = match input.coupon.as_ref().map(|c| c.discount) {
        Some(CouponDiscount::Percent { basis_points }) => to_minor(div_round_half_up(
            i128::from(goods_base) * i128::from(basis_points),
            10_000,
        ))
        .ok_or_else(overflow)?,
        Some(CouponDiscount::Fixed { amount_minor }) => amount_minor.min(goods_base),
        Some(CouponDiscount::FreeShipping) | None => 0,
    };
    let discounts = if goods_discount > 0 {
        allocate(goods_discount, &bases).ok_or_else(overflow)?
    } else {
        vec![0; bases.len()]
    };

    // 3. VAT per line.
    let mut goods_by_rate: BTreeMap<TaxRate, i64> = BTreeMap::new();
    let lines: Vec<PricedLine> = input
        .lines
        .iter()
        .zip(bases.iter().zip(&discounts))
        .map(|(l, (base, discount))| {
            let rate = rate_of(l.tax_rate);
            let gross = base - discount;
            let vat = vat_from_gross(gross, rate);
            *goods_by_rate.entry(rate).or_default() += gross;
            PricedLine {
                id: l.id,
                quantity: l.quantity,
                unit_price_minor: l.unit_price_minor,
                tax_rate: rate,
                base_minor: *base,
                discount_minor: *discount,
                gross_minor: gross,
                vat_minor: vat,
                net_minor: gross - vat,
            }
        })
        .collect();
    let goods: i64 = lines.iter().map(|l| l.gross_minor).sum();
    let fallback = rate_of(input.fallback_rate);

    // 4. Ancillary charges.
    let mut charges = Vec::new();
    if let Some(shipping) = input.shipping_minor {
        let free = matches!(
            input.coupon.as_ref().map(|c| c.discount),
            Some(CouponDiscount::FreeShipping)
        );
        let discount = if free { shipping } else { 0 };
        charges.push(charge(
            ChargeKind::Shipping,
            shipping,
            discount,
            true,
            &goods_by_rate,
            fallback,
        )?);
    }
    if let Some(fee) = input.payment_fee_minor {
        charges.push(charge(
            ChargeKind::PaymentFee,
            fee,
            0,
            true,
            &goods_by_rate,
            fallback,
        )?);
    }

    // 5. Cash rounding, last.
    let subtotal = goods + charges.iter().map(|c| c.gross_minor).sum::<i64>();
    if let Some(r) = input.cash_rounding
        && let Some(c) = rounding_charge(subtotal, r, &goods_by_rate, fallback)?
    {
        charges.push(c);
    }

    // A non-payer shows no recap; a payer's recap covers goods and taxed charge portions.
    let mut recap: BTreeMap<TaxRate, VatRecapRow> = BTreeMap::new();
    if input.vat_payer {
        let rows = lines
            .iter()
            .map(|l| (l.tax_rate, l.gross_minor, l.vat_minor))
            .chain(
                charges
                    .iter()
                    .flat_map(|c| &c.portions)
                    .map(|p| (p.tax_rate, p.gross_minor, p.vat_minor)),
            );
        for (rate, gross, vat) in rows {
            let row = recap.entry(rate).or_insert(VatRecapRow {
                tax_rate: rate,
                net_minor: 0,
                vat_minor: 0,
                gross_minor: 0,
            });
            row.gross_minor += gross;
            row.vat_minor += vat;
            row.net_minor += gross - vat;
        }
    }

    let total = goods + charges.iter().map(|c| c.gross_minor).sum::<i64>();
    Ok(PricedCart {
        currency: input.currency,
        vat_minor: lines.iter().map(|l| l.vat_minor).sum::<i64>()
            + charges.iter().map(|c| c.vat_minor).sum::<i64>(),
        discount_minor: goods_discount + charges.iter().map(|c| c.discount_minor).sum::<i64>(),
        coupon_code: input.coupon.as_ref().map(|c| c.code.clone()),
        goods_minor: goods,
        total_minor: total,
        lines,
        charges,
        vat_recap: recap.into_values().collect(),
    })
}

/// Amounts reversed for returned units of an order line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct Reversal {
    pub gross_minor: i64,
    pub discount_minor: i64,
    pub vat_minor: i64,
    pub net_minor: i64,
}

/// Reverses `returning` units of `line` after `already_returned` units were reversed (A15).
/// Partial refunds in any split add up exactly to the original allocation, never refund a
/// negative net, and the rounding residual lands on the last units returned.
pub fn reverse_line(
    line: &PricedLine,
    already_returned: u32,
    returning: u32,
) -> Result<Reversal, Error> {
    let upto = already_returned
        .checked_add(returning)
        .filter(|u| *u <= line.quantity && returning > 0)
        .ok_or_else(|| {
            invalid(
                "invalid_return_quantity",
                "cannot return more than was ordered",
            )
        })?;
    // Each component is spread over the units as evenly as possible, the extra minor units
    // on the last units: unit i of q carries floor(c/q), plus 1 for the last c mod q units.
    // Gross and VAT use the same layout, so every unit (and any run of units) has
    // 0 <= VAT <= gross, and all units together give back exactly the original amounts.
    let qty = i128::from(line.quantity);
    let upto_all = |component: i64, k: u32| {
        let (c, k) = (i128::from(component), i128::from(k));
        let (base, extra) = (c.div_euclid(qty), c.rem_euclid(qty));
        k * base + (k - (qty - extra)).max(0)
    };
    let part = |component: i64| {
        to_minor(upto_all(component, upto) - upto_all(component, already_returned))
            .ok_or_else(overflow)
    };
    let gross = part(line.gross_minor)?;
    let vat = part(line.vat_minor)?;
    Ok(Reversal {
        gross_minor: gross,
        discount_minor: part(line.discount_minor)?,
        vat_minor: vat,
        net_minor: gross - vat,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn line(n: u128, qty: u32, unit: i64, rate: u32) -> LineInput {
        LineInput {
            id: Uuid::from_u128(n),
            quantity: qty,
            unit_price_minor: unit,
            tax_rate: TaxRate(rate),
        }
    }

    fn cart(lines: Vec<LineInput>) -> CartInput {
        CartInput {
            currency: Currency::Czk,
            vat_payer: true,
            lines,
            coupon: None,
            shipping_minor: None,
            payment_fee_minor: None,
            fallback_rate: TaxRate(2100),
            cash_rounding: None,
        }
    }

    fn coupon(discount: CouponDiscount) -> Option<AppliedCoupon> {
        Some(AppliedCoupon {
            code: "SLEVA".into(),
            discount,
        })
    }

    #[test]
    fn worked_example_czk() {
        // 2 × 121,00 (21 %) + 1 × 112,00 (12 %), 10 % coupon, shipping 99,00, COD fee 30,00,
        // paid in cash -> rounded to whole koruna.
        let input = CartInput {
            coupon: coupon(CouponDiscount::Percent { basis_points: 1000 }),
            shipping_minor: Some(9900),
            payment_fee_minor: Some(3000),
            cash_rounding: cash_rounding(Currency::Czk, "CZ", Tender::Cash, false),
            ..cart(vec![line(1, 2, 12_100, 2100), line(2, 1, 11_200, 1200)])
        };
        let p = price_cart(&input).unwrap();
        // Goods 354,00; coupon 35,40 split 24,20 / 11,20.
        assert_eq!(p.lines[0].discount_minor, 2420);
        assert_eq!(p.lines[1].discount_minor, 1120);
        assert_eq!(p.lines[0].gross_minor, 21_780);
        assert_eq!(p.lines[0].vat_minor, 3780); // 217,80 × 21/121
        assert_eq!(p.lines[1].gross_minor, 10_080);
        assert_eq!(p.lines[1].vat_minor, 1080); // 100,80 × 12/112
        assert_eq!(p.goods_minor, 31_860);
        // Shipping 99,00 split 217,80 : 100,80 -> 67,68 / 31,32.
        let ship = &p.charges[0];
        assert_eq!(ship.portions.len(), 2);
        assert_eq!(ship.portions[0].tax_rate, TaxRate(1200));
        assert_eq!(ship.portions[0].gross_minor, 3132);
        assert_eq!(ship.portions[1].gross_minor, 6768);
        // Subtotal 318,60 + 99 + 30 = 447,60 -> 448,00 cash: rounding +0,40 outside VAT.
        let rounding = p.charges.last().unwrap();
        assert_eq!(rounding.kind, ChargeKind::Rounding);
        assert_eq!(rounding.gross_minor, 40);
        assert_eq!(rounding.vat_minor, 0);
        assert!(rounding.portions.is_empty());
        assert_eq!(p.total_minor, 44_800);
        assert_eq!(p.discount_minor, 3540);
        let recap_gross: i64 = p.vat_recap.iter().map(|r| r.gross_minor).sum();
        assert_eq!(recap_gross, 44_760);
    }

    #[test]
    fn eur_sk_cash_rounding_to_five_cents() {
        let r = cash_rounding(Currency::Eur, "SK", Tender::Cash, false);
        assert_eq!(r.map(|r| r.increment_minor), Some(5));
        assert_eq!(
            cash_rounding(Currency::Eur, "SK", Tender::Card, false),
            None
        );
        assert_eq!(
            cash_rounding(Currency::Eur, "DE", Tender::Cash, false),
            None
        );
        assert_eq!(
            cash_rounding(Currency::Pln, "PL", Tender::Cash, false),
            None
        );
        for (total, rounded) in [
            (1, 5),
            (2, 5),
            (0, 0),
            (1291, 1290),
            (1292, 1290),
            (1293, 1295),
            (1294, 1295),
            (1295, 1295),
        ] {
            let p = price_cart(&CartInput {
                currency: Currency::Eur,
                cash_rounding: r,
                ..cart(vec![line(1, 1, total, 2300)])
            })
            .unwrap();
            assert_eq!(p.total_minor, rounded, "{total}");
        }
        // CZK: 0,50 rounds up, 0,49 down.
        let czk = cash_rounding(Currency::Czk, "CZ", Tender::Cash, false);
        for (total, rounded) in [(10_050, 10_100), (10_049, 10_000)] {
            let p = price_cart(&CartInput {
                cash_rounding: czk,
                ..cart(vec![line(1, 1, total, 2100)])
            })
            .unwrap();
            assert_eq!(p.total_minor, rounded);
        }
    }

    #[test]
    fn taxed_rounding_is_split_like_a_fee() {
        let p = price_cart(&CartInput {
            cash_rounding: Some(CashRounding {
                increment_minor: 100,
                in_vat_base: true,
            }),
            ..cart(vec![line(1, 1, 10_030, 2100)])
        })
        .unwrap();
        let r = p.charges.last().unwrap();
        assert_eq!(r.gross_minor, -30);
        assert_eq!(r.portions.len(), 1);
        assert_eq!(r.vat_minor, -5); // -0,30 × 21/121 = -0,052 -> -0,05
        assert_eq!(p.total_minor, 10_000);
        assert_eq!(p.vat_recap[0].gross_minor, 10_000);
    }

    #[test]
    fn fixed_coupon_is_capped_and_free_shipping_zeroes_shipping() {
        let p = price_cart(&CartInput {
            coupon: coupon(CouponDiscount::Fixed {
                amount_minor: 1_000_000,
            }),
            shipping_minor: Some(9900),
            ..cart(vec![line(1, 1, 5000, 2100)])
        })
        .unwrap();
        assert_eq!(p.goods_minor, 0);
        // No goods value to split by: shipping uses the fallback rate.
        assert_eq!(p.charges[0].portions[0].tax_rate, TaxRate(2100));
        assert_eq!(p.total_minor, 9900);

        let p = price_cart(&CartInput {
            coupon: coupon(CouponDiscount::FreeShipping),
            shipping_minor: Some(9900),
            ..cart(vec![line(1, 1, 5000, 2100)])
        })
        .unwrap();
        assert_eq!(p.charges[0].gross_minor, 0);
        assert_eq!(p.charges[0].discount_minor, 9900);
        assert!(p.charges[0].portions.is_empty());
        assert_eq!(p.total_minor, 5000);
        assert_eq!(p.discount_minor, 9900);
    }

    #[test]
    fn non_vat_payer_charges_no_vat() {
        let p = price_cart(&CartInput {
            vat_payer: false,
            shipping_minor: Some(9900),
            ..cart(vec![line(1, 3, 12_100, 2100)])
        })
        .unwrap();
        assert_eq!(p.vat_minor, 0);
        assert!(p.vat_recap.is_empty());
        assert!(p.lines.iter().all(|l| l.tax_rate == TaxRate::ZERO));
        assert_eq!(p.total_minor, 3 * 12_100 + 9900);
    }

    #[test]
    fn rejects_out_of_range_input() {
        for bad in [
            cart(vec![line(1, 0, 100, 2100)]),
            cart(vec![line(1, 1, -1, 2100)]),
            cart(vec![line(1, MAX_QUANTITY + 1, 100, 2100)]),
            CartInput {
                shipping_minor: Some(-5),
                ..cart(vec![])
            },
            CartInput {
                coupon: coupon(CouponDiscount::Percent { basis_points: 0 }),
                ..cart(vec![])
            },
            CartInput {
                cash_rounding: Some(CashRounding {
                    increment_minor: 0,
                    in_vat_base: false,
                }),
                ..cart(vec![])
            },
        ] {
            assert_eq!(price_cart(&bad).unwrap_err().code(), "invalid_cart");
        }
    }

    #[test]
    fn reversal_basics() {
        let p = price_cart(&CartInput {
            coupon: coupon(CouponDiscount::Fixed { amount_minor: 100 }),
            ..cart(vec![line(1, 3, 1000, 2100)])
        })
        .unwrap();
        let l = &p.lines[0];
        let a = reverse_line(l, 0, 1).unwrap();
        let b = reverse_line(l, 1, 2).unwrap();
        assert_eq!(a.gross_minor + b.gross_minor, l.gross_minor);
        assert_eq!(a.discount_minor + b.discount_minor, 100);
        assert_eq!(
            reverse_line(l, 2, 2).unwrap_err().code(),
            "invalid_return_quantity"
        );
        assert!(reverse_line(l, 0, 0).is_err());
        // Review case: 5 units, gross 4, VAT 1 -> no unit refunds VAT without gross.
        let tiny = PricedLine {
            id: Uuid::nil(),
            quantity: 5,
            unit_price_minor: 1,
            tax_rate: TaxRate(2100),
            base_minor: 5,
            discount_minor: 1,
            gross_minor: 4,
            vat_minor: 1,
            net_minor: 3,
        };
        for k in 0..5 {
            let r = reverse_line(&tiny, k, 1).unwrap();
            assert!(
                r.net_minor >= 0 && r.vat_minor <= r.gross_minor,
                "{k}: {r:?}"
            );
        }
    }

    fn arb_cart() -> impl Strategy<Value = CartInput> {
        let rates = prop::sample::select(vec![0u32, 500, 1000, 1200, 1350, 2100, 2300, 2700]);
        let line = (1u32..50, 0i64..5_000_000, rates);
        let discount = prop_oneof![
            (1u32..=10_000).prop_map(|basis_points| CouponDiscount::Percent { basis_points }),
            (1i64..20_000_000).prop_map(|amount_minor| CouponDiscount::Fixed { amount_minor }),
            Just(CouponDiscount::FreeShipping),
        ];
        (
            prop::collection::vec(line, 0..12),
            prop::option::of(discount),
            prop::option::of(0i64..50_000),
            prop::option::of(0i64..10_000),
            any::<bool>(),
            prop::option::of((prop::sample::select(vec![5i64, 100]), any::<bool>())),
        )
            .prop_map(
                |(lines, discount, shipping, fee, payer, rounding)| CartInput {
                    currency: Currency::Czk,
                    vat_payer: payer,
                    lines: lines
                        .into_iter()
                        .enumerate()
                        .map(|(i, (qty, unit, rate))| LineInput {
                            id: Uuid::from_u128(i as u128),
                            quantity: qty,
                            unit_price_minor: unit,
                            tax_rate: TaxRate(rate),
                        })
                        .collect(),
                    coupon: discount.map(|discount| AppliedCoupon {
                        code: "X".into(),
                        discount,
                    }),
                    shipping_minor: shipping,
                    payment_fee_minor: fee,
                    fallback_rate: TaxRate(2100),
                    cash_rounding: rounding.map(|(increment_minor, in_vat_base)| CashRounding {
                        increment_minor,
                        in_vat_base,
                    }),
                },
            )
    }

    proptest! {
        #[test]
        fn cart_invariants(input in arb_cart()) {
            let p = price_cart(&input).unwrap();
            // Totals equal the sum of the lines and charges.
            let lines: i64 = p.lines.iter().map(|l| l.gross_minor).sum();
            let charges: i64 = p.charges.iter().map(|c| c.gross_minor).sum();
            prop_assert_eq!(p.total_minor, lines + charges);
            prop_assert_eq!(p.goods_minor, lines);
            prop_assert!(p.total_minor >= 0);
            for l in &p.lines {
                prop_assert_eq!(l.base_minor, l.unit_price_minor * i64::from(l.quantity));
                prop_assert_eq!(l.gross_minor, l.base_minor - l.discount_minor);
                prop_assert!(l.gross_minor >= 0 && l.discount_minor >= 0);
                prop_assert_eq!(l.net_minor + l.vat_minor, l.gross_minor);
            }
            for c in &p.charges {
                prop_assert_eq!(c.net_minor + c.vat_minor, c.gross_minor);
                if !c.portions.is_empty() {
                    prop_assert_eq!(c.portions.iter().map(|x| x.gross_minor).sum::<i64>(), c.gross_minor);
                }
                if c.kind != ChargeKind::Rounding {
                    prop_assert!(c.gross_minor >= 0);
                }
            }
            // The VAT recap is consistent with the lines and charge portions.
            let recap_vat: i64 = p.vat_recap.iter().map(|r| r.vat_minor).sum();
            prop_assert_eq!(recap_vat, p.vat_minor);
            for r in &p.vat_recap {
                prop_assert_eq!(r.net_minor + r.vat_minor, r.gross_minor);
            }
            if input.vat_payer {
                let taxed: i64 = lines + p.charges.iter().flat_map(|c| &c.portions).map(|x| x.gross_minor).sum::<i64>();
                prop_assert_eq!(p.vat_recap.iter().map(|r| r.gross_minor).sum::<i64>(), taxed);
            } else {
                prop_assert!(p.vat_recap.is_empty());
                prop_assert_eq!(p.vat_minor, 0);
            }
            // Cash rounding lands on the increment, moving at most half of it.
            if let Some(r) = input.cash_rounding {
                prop_assert_eq!(p.total_minor % r.increment_minor, 0);
                let diff = p.charges.iter().find(|c| c.kind == ChargeKind::Rounding).map_or(0, |c| c.gross_minor);
                let subtotal = p.total_minor - diff;
                prop_assert!(diff.abs() * 2 <= r.increment_minor || (subtotal > 0 && p.total_minor == r.increment_minor));
                prop_assert_eq!(subtotal > 0, p.total_minor > 0);
            }
            // Deterministic.
            prop_assert_eq!(price_cart(&input).unwrap(), p);
        }

        #[test]
        fn partial_refunds_sum_to_the_original(
            input in arb_cart(),
            cuts in prop::collection::vec(1u32..50, 1..8),
        ) {
            let p = price_cart(&input).unwrap();
            for l in &p.lines {
                let mut done = 0;
                let mut total = Reversal::default();
                for c in cuts.iter().chain(std::iter::once(&u32::MAX)) {
                    let n = (*c).min(l.quantity - done);
                    if n == 0 { continue; }
                    let r = reverse_line(l, done, n).unwrap();
                    prop_assert!(r.gross_minor >= 0 && r.vat_minor >= 0 && r.discount_minor >= 0);
                    prop_assert!(r.vat_minor <= r.gross_minor && r.net_minor >= 0);
                    total.gross_minor += r.gross_minor;
                    total.vat_minor += r.vat_minor;
                    total.net_minor += r.net_minor;
                    total.discount_minor += r.discount_minor;
                    done += n;
                }
                prop_assert_eq!(done, l.quantity);
                prop_assert_eq!(total.gross_minor, l.gross_minor);
                prop_assert_eq!(total.vat_minor, l.vat_minor);
                prop_assert_eq!(total.net_minor, l.net_minor);
                prop_assert_eq!(total.discount_minor, l.discount_minor);
            }
        }
    }
}
