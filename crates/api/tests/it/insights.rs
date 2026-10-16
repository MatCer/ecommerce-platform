use std::time::Duration;

use axum::http::StatusCode;
use chrono::{Days, Utc};
use commerce::ai::{Ai, Outcome};
use commerce::insights;
use serde_json::json;
use sqlx::PgPool;

use crate::common::*;

#[sqlx::test(migrations = "../../migrations")]
async fn staff_list_and_dismiss_their_shops_insights(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "ins").await;
    let other = testkit::storefront::shop(&runtime, "ins-other").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other.tenant, "rival", "owner").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (clerk, rival) = (sign(&claims("clerk")), sign(&claims("rival")));

    let day = Utc::now().date_naive() - Days::new(1);
    let mut tx = platform::db::tenant_tx(&runtime, shop.tenant)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO events (id, tenant_id, at, type, market_id, props)
         SELECT gen_random_uuid(), $1, $2 + time '12:00', 'web_vitals', $3,
                '{\"template\": \"product\", \"name\": \"LCP\", \"value\": 4000}'
         FROM generate_series(1, 60)",
    )
    .bind(shop.tenant)
    .bind(day)
    .bind(shop.cz)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO analytics_counters (tenant_id, market_id, day, template, requests)
         VALUES ($1, $2, $3, 'product', 20000)",
    )
    .bind(shop.tenant)
    .bind(shop.cz)
    .bind(day)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        insights::run(&runtime, &Ai::fake(), shop.tenant, day, false)
            .await
            .unwrap(),
        Outcome::Done
    );

    let (status, body, _) = Call::get("/admin/v1/insights")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["items"][0]["id"].as_str().unwrap().to_owned();
    let insight = json!({
        "id": id,
        "detection": { "kind": "slow_page", "template": "product", "metric": "LCP",
                       "p75": 4000.0, "samples": 60, "requests": 20000 },
        "score": 300.0,
        "product_name": null,
        "first_detected_on": day,
        "last_detected_on": day,
        "hypothesis": null,
    });
    assert_eq!(body, json!({ "items": [insight.clone()] }));

    let dismiss = format!("/admin/v1/insights/{id}/dismiss");
    let (status, _, _) = Call::post(&dismiss, json!({}))
        .token(&rival)
        .tenant(other.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another shop's signal");
    let (status, body, _) = Call::get("/admin/v1/insights")
        .token(&rival)
        .tenant(other.tenant)
        .send(&s)
        .await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "items": [] })));

    let (status, body, _) = Call::post(&dismiss, json!({}))
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!((status, body), (StatusCode::OK, insight));
    let (_, body, _) = Call::get("/admin/v1/insights")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(body, json!({ "items": [] }));
    let (status, _, _) = Call::post("/admin/v1/insights/not-a-uuid/dismiss", json!({}))
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_ai_switch_left_out_of_the_body_keeps_its_value(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "sw").await;
    testkit::staff(&runtime, shop.tenant, "boss", "admin").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let boss = sign(&claims("boss"));
    let put = async |body: serde_json::Value| {
        Call::put("/admin/v1/ai/settings", body)
            .token(&boss)
            .tenant(shop.tenant)
            .send(&s)
            .await
    };

    let (status, body, _) = put(json!({ "insights": true })).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "review_summaries": false, "alt_texts": false, "insights": true })
        )
    );
    let (status, body, _) = put(json!({ "review_summaries": true })).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "review_summaries": true, "alt_texts": false, "insights": true })
        )
    );
    let (status, body, _) = put(json!({ "insights": false })).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "review_summaries": true, "alt_texts": false, "insights": false })
        )
    );
    let (status, _, _) = put(json!({ "insights": true, "other": true })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, body, _) = Call::get("/admin/v1/ai/settings")
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        body,
        json!({ "review_summaries": true, "alt_texts": false, "insights": false })
    );
}
