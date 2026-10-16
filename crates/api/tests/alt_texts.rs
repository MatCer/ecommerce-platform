#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::ai::fields::product_input;
use commerce::catalog::products::{self, ProductMedia};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

mod common;
use common::*;

const BACKFILL: &str = "/admin/v1/ai/alt-texts/backfill";

#[sqlx::test(migrations = "../../migrations")]
async fn admins_switch_alt_texts_and_queue_a_backfill(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "alt").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, shop.tenant, "boss", "admin").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (clerk, boss) = (sign(&claims("clerk")), sign(&claims("boss")));
    let post = |token| {
        Call::post(BACKFILL, json!({}))
            .token(token)
            .tenant(shop.tenant)
    };

    let mut tx = platform::db::tenant_tx(&runtime, shop.tenant)
        .await
        .unwrap();
    let asset = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO assets (id, tenant_id, status, key) VALUES ($1, $2, 'ready', 'originals/x')",
    )
    .bind(asset)
    .bind(shop.tenant)
    .execute(&mut *tx)
    .await
    .unwrap();
    let mut input = product_input(&products::get(&mut tx, shop.product).await.unwrap());
    input.media = vec![ProductMedia {
        asset_id: asset,
        variant_sku: None,
        alt_i18n: [("cs".to_owned(), "Tričko".to_owned())].into(),
    }];
    products::replace(&mut tx, "clerk", shop.product, &input)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (status, body, _) = post(&clerk).send(&s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("insufficient_role"))
    );
    let (status, body, _) = post(&boss).send(&s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("ai_alt_texts_disabled"))
    );

    let on = json!({ "review_summaries": false, "alt_texts": true });
    let (status, body, _) = Call::put("/admin/v1/ai/settings", on.clone())
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!((status, body), (StatusCode::OK, on.clone()));
    let (_, body, _) = Call::get("/admin/v1/ai/settings")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(body, on);

    let (status, body, _) = post(&boss).send(&s).await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "queued": 1 })),
        "sk is missing"
    );
    let jobs: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM queue.jobs WHERE kind = 'ai.alt_text'")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        jobs,
        [json!({ "product_id": shop.product, "backfill": true })]
    );

    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 0 WHERE id = $1")
        .bind(shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    let (status, body, _) = post(&boss).send(&s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::PAYMENT_REQUIRED, Some("ai_quota_exceeded"))
    );
}
