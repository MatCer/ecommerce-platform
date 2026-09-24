//! Test helpers shared across crates (dev-dependency only).
//!
//! `#[sqlx::test]` hands tests a pool connected as `app_owner` (it creates the per-test
//! database). Application code runs as `app_runtime`, so tests that exercise RLS or grants use
//! [`runtime_pool`], which switches every connection to that role.

// Test support code: panicking on setup failures is the desired behaviour.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use object_store::memory::InMemory;
use platform::storage::Storage;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

/// Object storage that is always reachable and starts empty.
pub fn memory_storage() -> Storage {
    Storage {
        public: Arc::new(InMemory::new()),
        private: Arc::new(InMemory::new()),
    }
}

/// A pool on the same test database whose connections run as `app_runtime`
/// (`app_owner` may `SET ROLE app_runtime`, see docker/postgres/init-roles.sh).
pub async fn runtime_pool(owner: &PgPool, max_connections: u32) -> PgPool {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute("SET ROLE app_runtime").await?;
                Ok(())
            })
        })
        .connect_with((*owner.connect_options()).clone())
        .await
        .unwrap()
}

/// Inserts an active tenant with a default `cz` market; returns `(tenant_id, market_id)`.
pub async fn tenant(runtime: &PgPool, slug: &str) -> (Uuid, Uuid) {
    let tenant_id: Uuid = sqlx::query_scalar(
        "INSERT INTO platform.tenants (slug, name) VALUES ($1, $1) RETURNING id",
    )
    .bind(slug)
    .fetch_one(runtime)
    .await
    .unwrap();
    let mut tx = platform::db::tenant_tx(runtime, tenant_id).await.unwrap();
    let market_id: Uuid = sqlx::query_scalar(
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale, locales, is_default)
         VALUES ($1, 'cz', 'Česko', '{CZ}', 'CZK', 'cs', '{cs}', true) RETURNING id",
    )
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (tenant_id, market_id)
}

/// Adds `user_id` to the tenant's staff with `role`.
pub async fn staff(runtime: &PgPool, tenant_id: Uuid, user_id: &str, role: &str) {
    let mut tx = platform::db::tenant_tx(runtime, tenant_id).await.unwrap();
    sqlx::query(
        "INSERT INTO staff_members (tenant_id, user_id, email, role) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(format!("{user_id}@example.test"))
    .bind(role)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
