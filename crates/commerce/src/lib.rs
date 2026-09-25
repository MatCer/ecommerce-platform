//! Business modules (spec §6). No HTTP routing here: this crate must run anywhere.
//! Modules are folders (or files) inside this crate, never separate crates.
//!
//! Tenant-scoped functions take `&mut platform::db::TenantTx`, so they cannot run outside a
//! tenant transaction (spec A8).

pub mod audit;
pub mod capability;
pub mod cart;
pub mod catalog;
pub mod id;
pub mod idempotency;
pub mod inventory;
pub mod markets;
pub mod media;
pub mod money;
pub mod notifications;
pub mod orders;
pub mod pricing;
pub mod promotions;
pub mod redirects;
pub mod search;
pub mod staff;
pub mod storefront;
pub mod tax;
pub mod tenancy;
pub mod themes;

/// Postgres `unique_violation` (23505).
fn unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23505"))
}
