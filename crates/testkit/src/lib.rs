//! Test helpers shared across crates (dev-dependency only).
//!
//! `#[sqlx::test]` hands tests a pool connected as `app_owner` (it creates the per-test
//! database). Application code runs as `app_runtime`, so tests that exercise RLS or grants use
//! [`runtime_pool`], which switches every connection to that role.

// Test support code: panicking on setup failures is the desired behaviour.
#![allow(clippy::unwrap_used)]

pub mod catalog;
pub mod pricing;
pub mod storefront;

use std::sync::Arc;

use object_store::memory::InMemory;
use platform::config::S3Config;
use platform::storage::Storage;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

/// Object storage that is always reachable and starts empty. Presigned URLs point at
/// `http://s3.test` (signing needs no server); media URLs at `http://media.test/`.
pub fn memory_storage() -> Storage {
    let cfg = S3Config::from_lookup(&|k| {
        Some(
            match k {
                "S3_ENDPOINT" => "http://s3.test",
                "S3_ACCESS_KEY_ID" => "test",
                "S3_SECRET_ACCESS_KEY" => "test-secret",
                "S3_BUCKET_PUBLIC" => "public",
                "S3_BUCKET_PRIVATE" => "private",
                "MEDIA_BASE_URL" => "http://media.test/",
                _ => return None,
            }
            .to_owned(),
        )
    })
    .unwrap();
    let s3 = Storage::s3(&cfg).unwrap();
    Storage {
        public: Arc::new(InMemory::new()),
        private: Arc::new(InMemory::new()),
        ..s3
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

/// A Meilisearch client for tests that must not reach one (nothing listens on port 1).
pub fn dead_meili() -> commerce::search::Meili {
    commerce::search::Meili::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1".parse().unwrap(),
        "unused".into(),
        std::time::Duration::from_secs(1),
    )
}

/// The real Meilisearch of `make test-search` with the worker's admin key (`MEILI_URL`,
/// `MEILI_ADMIN_KEY`), for `#[ignore]`d search integration tests. Index names contain the
/// test's tenant UUID, so tests never share an index.
pub fn meili() -> commerce::search::Meili {
    meili_with("MEILI_ADMIN_KEY")
}

/// Like [`meili`] with the API's search-only key (`MEILI_SEARCH_KEY`): queries in tests run
/// with the same permissions as in production (A27).
pub fn meili_search() -> commerce::search::Meili {
    meili_with("MEILI_SEARCH_KEY")
}

fn meili_with(key_var: &str) -> commerce::search::Meili {
    let url = std::env::var("MEILI_URL").expect("MEILI_URL (run via `make test-search`)");
    let key = std::env::var(key_var).expect("Meilisearch key (run via `make test-search`)");
    commerce::search::Meili::new(
        reqwest::Client::new(),
        url.parse().unwrap(),
        key,
        std::time::Duration::from_secs(30),
    )
}
