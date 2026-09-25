//! Business modules (spec §6). No HTTP routing here: this crate must run anywhere.
//! Modules are folders (or files) inside this crate, never separate crates.
//!
//! Tenant-scoped functions take `&mut platform::db::TenantTx`, so they cannot run outside a
//! tenant transaction (spec A8).

pub mod adtracking;
pub mod ai;
pub mod analytics;
pub mod audit;
pub mod capability;
pub mod carriers;
pub mod cart;
pub mod catalog;
pub mod checkout;
pub mod consent;
pub mod content;
pub mod customers;
pub mod documents;
pub mod feeds;
pub mod fulfillment;
pub mod id;
pub mod idempotency;
pub mod inventory;
pub mod invoicing;
pub mod marketing;
pub mod markets;
pub mod media;
pub mod money;
pub mod notifications;
pub mod ops;
pub mod orders;
pub mod payments;
pub mod pricing;
pub mod privacy;
pub mod promotions;
pub mod recommendations;
pub mod redirects;
pub mod refunds;
pub mod reviews;
pub mod search;
pub mod shipping;
pub mod staff;
pub mod storefront;
pub mod tax;
pub mod tenancy;
pub mod themes;
pub mod webhooks;
pub mod withdrawals;

/// Postgres `unique_violation` (23505).
fn unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23505"))
}
