//! Omnibus reference price (Directive 2019/2161, spec §10.2, A18).
//!
//! A price reduction is announced only while a sale sets the price. Its reference is the
//! lowest price in the 30 days before the reduction started:
//! - a chained (progressive) reduction keeps the reference from before its first step: the
//!   chain is the run of back-to-back, non-increasing sale intervals ending with the current
//!   one;
//! - a product younger than 30 days uses the lowest price since launch;
//! - a price history that starts with an import less than 30 days before the reduction is
//!   incomplete: no reduction claim until 30 days of history exist;
//! - published coupons (usable by everyone) count as price reductions in the window.
//!
//! The storefront shows the discount percent and strikethrough only against this reference,
//! never against `compare_at` (which is display-only).

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use super::intervals::{Cause, Interval};
use crate::money::div_round_half_up;

pub const WINDOW_DAYS: i64 = 30;

/// A published coupon's per-unit effect over its validity window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouponWindow {
    pub from: DateTime<Utc>,
    pub to: Option<DateTime<Utc>>,
    pub effect: CouponEffect,
}

/// A coupon's discount on a single unit, which counts only when that unit alone reaches the
/// coupon's minimum subtotal (if any).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CouponEffect {
    pub discount: UnitDiscount,
    pub min_subtotal_minor: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitDiscount {
    Percent { basis_points: u32 },
    Fixed { amount_minor: i64 },
}

impl CouponEffect {
    fn apply(self, price: i64) -> i64 {
        if self.min_subtotal_minor.is_some_and(|m| price < m) {
            return price;
        }
        match self.discount {
            UnitDiscount::Percent { basis_points } => {
                let off = div_round_half_up(i128::from(price) * i128::from(basis_points), 10_000);
                price - i64::try_from(off).unwrap_or(price)
            }
            UnitDiscount::Fixed { amount_minor } => (price - amount_minor).max(0),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct Omnibus {
    /// The price in force at the evaluation time.
    pub current_minor: Option<i64>,
    /// A sale sets the current price (a reduction is being announced).
    pub on_sale: bool,
    /// Start of the current reduction chain.
    pub reduction_started_at: Option<DateTime<Utc>>,
    /// Lowest price in the 30 days before `reduction_started_at`.
    pub reference_minor: Option<i64>,
    /// Whole percent off the reference, rounded down (never overstated).
    pub discount_percent: Option<u32>,
    /// Whether the storefront may show a reduction (strikethrough + percent) at all.
    pub claim: bool,
}

fn overlaps(
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
    w_from: DateTime<Utc>,
    w_to: DateTime<Utc>,
) -> bool {
    from < w_to && to.is_none_or(|t| t > w_from)
}

/// The Omnibus figures at `at`. `history`: all intervals of one (price list, variant),
/// sorted by `valid_from`.
pub fn reference(history: &[Interval], coupons: &[CouponWindow], at: DateTime<Utc>) -> Omnibus {
    let Some(idx) = history
        .iter()
        .position(|i| i.valid_from <= at && i.valid_to.is_none_or(|t| t > at))
    else {
        return Omnibus::default();
    };
    let current = &history[idx];
    let mut out = Omnibus {
        current_minor: Some(current.amount_minor),
        on_sale: current.cause == Cause::Sale,
        ..Omnibus::default()
    };
    if !out.on_sale {
        return out;
    }
    let mut first = idx;
    // A progressive reduction: back-to-back sale steps, each one lower (or equal). A step up
    // (e.g. a deeper overlapping sale expired) starts a new reduction.
    while first > 0
        && history[first - 1].cause == Cause::Sale
        && history[first - 1].valid_to == Some(history[first].valid_from)
        && history[first - 1].amount_minor >= history[first].amount_minor
    {
        first -= 1;
    }
    let start = history[first].valid_from;
    out.reduction_started_at = Some(start);
    let window_from = start - Duration::days(WINDOW_DAYS);
    if history
        .iter()
        .any(|i| i.imported && i.valid_from > window_from && i.valid_from <= start)
    {
        return out;
    }
    let mut lowest: Option<i64> = None;
    for i in history[..first]
        .iter()
        .filter(|i| overlaps(i.valid_from, i.valid_to, window_from, start))
    {
        let mut candidate = i.amount_minor;
        for c in coupons {
            // The coupon was usable while this interval was in force, inside the window.
            let from = i.valid_from.max(window_from);
            let to = i.valid_to.map_or(start, |t| t.min(start));
            if overlaps(c.from, c.to, from, to) {
                candidate = candidate.min(c.effect.apply(i.amount_minor));
            }
        }
        lowest = Some(lowest.map_or(candidate, |l| l.min(candidate)));
    }
    out.reference_minor = lowest;
    if let Some(reference) = lowest.filter(|r| *r > current.amount_minor) {
        out.claim = true;
        let off = i128::from(reference - current.amount_minor) * 100 / i128::from(reference);
        out.discount_percent = u32::try_from(off).ok();
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use uuid::Uuid;

    use super::*;

    fn t(day: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .unwrap_or_default()
            + Duration::days(day)
    }

    fn iv(from: i64, to: Option<i64>, amount: i64, cause: Cause) -> Interval {
        Interval {
            id: Uuid::from_u128(from as u128),
            amount_minor: amount,
            valid_from: t(from),
            valid_to: to.map(t),
            cause,
            sale_id: None,
            imported: false,
        }
    }

    #[test]
    fn no_reduction_without_a_sale() {
        let h = [iv(0, None, 1000, Cause::Base)];
        let o = reference(&h, &[], t(50));
        assert_eq!(o.current_minor, Some(1000));
        assert!(!o.on_sale && !o.claim && o.reference_minor.is_none());
        assert_eq!(reference(&[], &[], t(1)), Omnibus::default());
    }

    #[test]
    fn lowest_in_the_30_days_before_the_sale() {
        // 1000 until day 40, 800 days 40-50 (base drop), 1000 again, sale 900 from day 70.
        let h = [
            iv(0, Some(40), 1000, Cause::Base),
            iv(40, Some(50), 800, Cause::Base),
            iv(50, Some(70), 1000, Cause::Base),
            iv(70, None, 900, Cause::Sale),
        ];
        // Window [40, 70): includes the 800 -> no real reduction, no claim.
        let o = reference(&h, &[], t(75));
        assert_eq!(o.reference_minor, Some(800));
        assert!(o.on_sale && !o.claim);
        // Sale from day 81: window [51, 81) sees only 1000.
        let h2 = [
            iv(0, Some(40), 1000, Cause::Base),
            iv(40, Some(50), 800, Cause::Base),
            iv(50, Some(81), 1000, Cause::Base),
            iv(81, None, 900, Cause::Sale),
        ];
        let o = reference(&h2, &[], t(85));
        assert_eq!(o.reference_minor, Some(1000));
        assert!(o.claim);
        assert_eq!(o.discount_percent, Some(10));
        assert_eq!(o.reduction_started_at, Some(t(81)));
    }

    #[test]
    fn chained_reduction_keeps_the_first_reference() {
        let h = [
            iv(0, Some(100), 1000, Cause::Base),
            iv(100, Some(110), 900, Cause::Sale),
            iv(110, None, 700, Cause::Sale),
        ];
        let o = reference(&h, &[], t(115));
        assert_eq!(o.reduction_started_at, Some(t(100)));
        assert_eq!(o.reference_minor, Some(1000));
        assert_eq!(o.discount_percent, Some(30));
        // A gap at base price breaks the chain.
        let h = [
            iv(0, Some(100), 1000, Cause::Base),
            iv(100, Some(110), 900, Cause::Sale),
            iv(110, Some(111), 1000, Cause::Base),
            iv(111, None, 700, Cause::Sale),
        ];
        let o = reference(&h, &[], t(115));
        assert_eq!(o.reduction_started_at, Some(t(111)));
        assert_eq!(o.reference_minor, Some(900));
        assert_eq!(o.discount_percent, Some(22));
        // A step up inside a run of sales (the deeper sale ended) is a new reduction whose
        // window contains the earlier 700: no claim.
        let h = [
            iv(0, Some(100), 1000, Cause::Base),
            iv(100, Some(110), 700, Cause::Sale),
            iv(110, None, 900, Cause::Sale),
        ];
        let o = reference(&h, &[], t(115));
        assert_eq!(o.reduction_started_at, Some(t(110)));
        assert_eq!(o.reference_minor, Some(700));
        assert!(!o.claim);
    }

    #[test]
    fn young_and_imported_products() {
        // Launched 10 days before the sale: the minimum since launch.
        let h = [
            iv(0, Some(10), 1000, Cause::Base),
            iv(10, None, 800, Cause::Sale),
        ];
        let o = reference(&h, &[], t(12));
        assert_eq!(o.reference_minor, Some(1000));
        assert!(o.claim);
        // On sale from launch: nothing to compare with.
        let h = [iv(0, None, 800, Cause::Sale)];
        let o = reference(&h, &[], t(1));
        assert!(o.on_sale && !o.claim && o.reference_minor.is_none());
        // Imported 10 days before the sale: history incomplete, no claim.
        let mut imported = iv(0, Some(10), 1000, Cause::Base);
        imported.imported = true;
        let h = [imported.clone(), iv(10, None, 800, Cause::Sale)];
        assert!(!reference(&h, &[], t(12)).claim);
        // 30+ days after the import it is fine.
        let mut long = imported;
        long.valid_to = Some(t(40));
        let h = [long, iv(40, None, 800, Cause::Sale)];
        assert!(reference(&h, &[], t(41)).claim);
    }

    #[test]
    fn published_coupons_lower_the_reference() {
        let h = [
            iv(0, Some(100), 1000, Cause::Base),
            iv(100, None, 850, Cause::Sale),
        ];
        // 20 % coupon for everyone during days 80-85: the price was effectively 800.
        let coupon = CouponWindow {
            from: t(80),
            to: Some(t(85)),
            effect: CouponEffect {
                discount: UnitDiscount::Percent { basis_points: 2000 },
                min_subtotal_minor: None,
            },
        };
        let o = reference(&h, std::slice::from_ref(&coupon), t(101));
        assert_eq!(o.reference_minor, Some(800));
        assert!(!o.claim);
        // A coupon that ended before the window does not count.
        let old = CouponWindow {
            to: Some(t(60)),
            ..coupon.clone()
        };
        assert_eq!(reference(&h, &[old], t(101)).reference_minor, Some(1000));
        // A fixed coupon with a minimum above the unit price does not count.
        let fixed = CouponWindow {
            effect: CouponEffect {
                discount: UnitDiscount::Fixed { amount_minor: 300 },
                min_subtotal_minor: Some(5000),
            },
            ..coupon
        };
        assert_eq!(reference(&h, &[fixed], t(101)).reference_minor, Some(1000));
    }
}
