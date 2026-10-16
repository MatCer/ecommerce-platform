#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::ai::{Ai, Outcome, review_summary};
use serde_json::json;
use sqlx::PgPool;

mod common;
use common::*;

#[sqlx::test(migrations = "../../migrations")]
async fn admins_switch_summaries_and_the_product_page_shows_them(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "sum").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, shop.tenant, "boss", "admin").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (clerk, boss) = (sign(&claims("clerk")), sign(&claims("boss")));

    let (status, body, _) = Call::get("/admin/v1/ai/settings")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "review_summaries": false, "alt_texts": false })
        )
    );
    let on = json!({ "review_summaries": true, "alt_texts": false });
    let (status, body, _) = Call::put("/admin/v1/ai/settings", on.clone())
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("insufficient_role"))
    );
    let (status, _, _) = Call::put(
        "/admin/v1/ai/settings",
        json!({ "review_summaries": true, "alt_texts": false, "other": 1 }),
    )
    .token(&boss)
    .tenant(shop.tenant)
    .send(&s)
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body, _) = Call::put("/admin/v1/ai/settings", on.clone())
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "review_summaries": true, "alt_texts": false })
        )
    );
    let (_, log, _) = Call::get("/admin/v1/audit-log?limit=1")
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(log["items"][0]["action"], "ai.settings_updated");
    assert_eq!(log["items"][0]["actor"], "boss");

    let mut tx = platform::db::tenant_tx(&runtime, shop.tenant)
        .await
        .unwrap();
    for rating in [5, 5, 4, 4, 5] {
        sqlx::query(
            "INSERT INTO reviews (tenant_id, product_id, customer_name, rating, body, verified,
                                  locale, status, published_at)
             VALUES ($1, $2, 'Jana', $3, 'Sedí.', true, 'cs', 'published', now())",
        )
        .bind(shop.tenant)
        .bind(shop.product)
        .bind(rating)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    assert_eq!(
        review_summary::run(&runtime, &Ai::fake(), shop.tenant, shop.product, false)
            .await
            .unwrap(),
        Outcome::Done
    );

    let (status, body, _) = Call::get(&format!("/storefront/v1/pages/product/{}", shop.slug))
        .header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let summary = &body["reviews"]["ai_summary"];
    assert_eq!(
        (
            &summary["text"],
            &summary["locale"],
            &summary["review_count"]
        ),
        (
            &json!(
                "Ukázkové shrnutí od demo AI: zákazníci hodnotí „Product TEE“ průměrně 4,6 z 5."
            ),
            &json!("cs"),
            &json!(5)
        )
    );
    assert!(summary["generated_at"].is_string());
}
