//! Recommendations (spec §11.2, §7.6, A20).
//!
//! - [`rollup`]: product stats per day and market (consented events + orders), co-purchases,
//!   decayed scores, the search popularity and customer affinity. Run hourly by the worker.
//! - [`collections`]: merchant-curated lists (manual, seasonal with a schedule window).
//! - [`settings`]: per-tenant strategy switches and excluded products.
//! - [`engine`]: the strategies, the fallback chain and visibility filtering, with an
//!   explanation of every decision for staff.
//!
//! Everything is computed per tenant (RLS); personal signals are used only while the subject's
//! `personalization` consent is granted, resolved on the server at request time (A20).

pub mod collections;
pub mod engine;
pub mod rollup;
pub mod settings;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Hourly rollup job: `{"slot"}` from cron, or `{"backfill": true}` for one tenant.
pub const ROLLUP_JOB: &str = "recommendations.rollup";
/// Scores and co-purchases look this far back.
pub const WINDOW_DAYS: i64 = 90;
/// A pair of products must share at least this many orders to be "bought together".
pub const MIN_SUPPORT: i64 = 3;
/// Half-life of a day's activity in the scores.
pub const HALF_LIFE_DAYS: f64 = 14.0;

/// Weight of an event `age_days` old (1 today, 0.5 after one half-life).
pub fn decay(age_days: f64, half_life_days: f64) -> f64 {
    0.5_f64.powf(age_days.max(0.0) / half_life_days)
}

/// Where a recommended product came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Bought together with the product (or the cart's products) in the last 90 days.
    BoughtTogether,
    /// Best sellers of the market (optionally in a category), time-decayed.
    Bestsellers,
    /// An open seasonal collection, else last year's best sellers of this month.
    Seasonal,
    /// A merchant collection the theme asked for.
    Collection,
    /// Products the visitor looked at (ids from the device, A20).
    RecentlyViewed,
    /// Category/brand affinity of a visitor who granted `personalization`.
    Personalized,
    /// Newest products: the last resort when there are no sales yet.
    Newest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_halves_per_half_life() {
        assert!((decay(0.0, 14.0) - 1.0).abs() < 1e-9);
        assert!((decay(14.0, 14.0) - 0.5).abs() < 1e-9);
        assert!((decay(28.0, 14.0) - 0.25).abs() < 1e-9);
        // Clock skew never weighs more than today.
        assert!((decay(-3.0, 14.0) - 1.0).abs() < 1e-9);
    }
}
