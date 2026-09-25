//! Data portability (spec §10.8, A28, A29): CSV imports of customers, historical orders and
//! newsletter subscribers ([`imports`]), the archive of imported orders ([`archived`]) and the
//! full tenant export ([`export`]). GDPR access/erasure lives in [`crate::privacy`].

pub mod archived;
pub mod export;
mod import_customers;
mod import_orders;
mod import_subscribers;
pub mod imports;
pub mod table;
