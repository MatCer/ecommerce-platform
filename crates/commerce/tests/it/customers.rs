//! Customer credential races (A5): rate limits and password changes under concurrency.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use argon2::password_hash::PasswordHasher;
use chrono::Utc;
use commerce::customers::{self, MagicLinkRequest, PasswordLogin};
use commerce::storefront::{self, PublicUrls};
use platform::db::tenant_tx;
use sqlx::PgPool;
use uuid::Uuid;

fn hash(password: &str) -> String {
    argon2::Argon2::default()
        .hash_password(password.as_bytes())
        .unwrap()
        .to_string()
}

async fn login(db: &PgPool, tenant: Uuid, market: Uuid, password: &str) -> bool {
    let mut tx = tenant_tx(db, tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap();
    let out = customers::login(
        &mut tx,
        &ctx,
        &PasswordLogin {
            email: "race@example.test".into(),
            password: password.into(),
            redirect: None,
        },
        None,
        None,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    out.is_some()
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_old_password_cannot_sign_in_across_a_concurrent_change(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "race").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO customers (tenant_id, email, locale, password_hash, email_verified_at)
         VALUES ($1, 'race@example.test', 'cs', $2, now())",
    )
    .bind(shop.tenant)
    .bind(hash("old password 123"))
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // A password change in flight: the row is locked and the new hash not yet committed.
    let mut change = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("SELECT id FROM customers WHERE email = 'race@example.test' FOR UPDATE")
        .execute(&mut *change)
        .await
        .unwrap();
    sqlx::query("UPDATE customers SET password_hash = $1 WHERE email = 'race@example.test'")
        .bind(hash("new password 456"))
        .execute(&mut *change)
        .await
        .unwrap();
    let (db2, tenant, market) = (runtime.clone(), shop.tenant, shop.cz);
    let attempt =
        tokio::spawn(async move { login(&db2, tenant, market, "old password 123").await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!attempt.is_finished(), "login waits for the change");
    change.commit().await.unwrap();
    assert!(!attempt.await.unwrap(), "the old password no longer works");
    assert!(login(&runtime, shop.tenant, shop.cz, "new password 456").await);
}

#[sqlx::test(migrations = "../../migrations")]
async fn parallel_magic_link_requests_share_one_allowance(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "burst").await;
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let db = runtime.clone();
            let (tenant, market) = (shop.tenant, shop.cz);
            tokio::spawn(async move {
                let mut tx = tenant_tx(&db, tenant).await.unwrap();
                let ctx =
                    storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
                        .await
                        .unwrap();
                let r = customers::request_magic_link(
                    &mut tx,
                    &ctx,
                    &MagicLinkRequest {
                        email: "burst@example.test".into(),
                        redirect: None,
                    },
                    Some(&[7u8; 32]),
                )
                .await;
                if r.is_ok() {
                    tx.commit().await.unwrap();
                }
                r.is_ok()
            })
        })
        .collect();
    let mut accepted = 0;
    for t in tasks {
        accepted += usize::from(t.await.unwrap());
    }
    assert_eq!(accepted, 3, "the per-email limit holds under concurrency");
}
