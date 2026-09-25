//! Promotions (spec §6, §10.2): sales (automatic discounts in the effective price) and
//! coupons (cart-level codes). Stacking: at most one coupon plus the running sales.

pub mod coupons;
pub mod sales;
