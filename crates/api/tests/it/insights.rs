use std::time::Duration;

use axum::http::StatusCode;
use chrono::{Days, Utc};
use commerce::ai::{Ai, Outcome};
use commerce::insights;
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

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
    let mut insight = json!({
        "id": id,
        "status": "open",
        "detection": { "kind": "slow_page", "template": "product", "metric": "LCP",
                       "p75": 4000.0, "samples": 60, "requests": 20000 },
        "score": 300.0,
        "product_name": null,
        "first_detected_on": day,
        "last_detected_on": day,
        "hypothesis": null,
        "theme_change": null,
        "experiment_id": null,
        "reopened": null,
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
    insight["status"] = json!("dismissed");
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

async fn accepted_revision(runtime: &PgPool, tenant: Uuid) -> (Uuid, Uuid) {
    let (insight, run, rev) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let artifact = "a0000000000000000000000000000001";
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO insight_signals (tenant_id, id, kind, subject, detection, score,
                                      first_detected_on, last_detected_on)
         VALUES ($1, $2, 'slow_page', 'product:LCP', $3, 1, current_date, current_date)",
    )
    .bind(tenant)
    .bind(insight)
    .bind(
        json!({ "kind": "slow_page", "template": "product", "metric": "LCP",
                  "p75": 4000.0, "samples": 60, "requests": 20000 }),
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO platform.theme_artifacts (id, kind) VALUES ($1, 'theme')
         ON CONFLICT DO NOTHING",
    )
    .bind(artifact)
    .execute(&mut *tx)
    .await
    .unwrap();
    let base: Uuid = sqlx::query_scalar(
        "INSERT INTO theme_revisions (tenant_id, number, artifact_id, status, created_by)
         SELECT $1, coalesce(max(number), 0) + 1, $2, 'ready', 'owner' FROM theme_revisions
         RETURNING id",
    )
    .bind(tenant)
    .bind(artifact)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ai_theme_runs (tenant_id, id, base_revision_id, prompt, status, created_by,
                                    insight_id)
         VALUES ($1, $2, $3, 'Make it faster.', 'accepted', 'owner', $4)",
    )
    .bind(tenant)
    .bind(run)
    .bind(base)
    .bind(insight)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO theme_revisions (tenant_id, id, number, artifact_id, origin, change,
                                      source_key, status, created_by, ai_run_id)
         SELECT $1, $2, max(number) + 1, $3, 'custom', 'ai', 'x', 'ready', 'owner', $4
         FROM theme_revisions",
    )
    .bind(tenant)
    .bind(rev)
    .bind(artifact)
    .bind(run)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE ai_theme_runs SET revision_id = $2 WHERE id = $1")
        .bind(run)
        .bind(rev)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (insight, rev)
}

#[sqlx::test(migrations = "../../migrations")]
async fn admins_test_an_insights_revision_and_staff_neither_test_nor_see_it(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "ins-test").await;
    testkit::staff(&runtime, shop.tenant, "boss", "admin").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let (boss, clerk) = (sign(&claims("boss")), sign(&claims("clerk")));
    let (insight, rev) = accepted_revision(&runtime, shop.tenant).await;
    let uri = format!("/admin/v1/insights/{insight}/experiments");
    let test = |token| {
        Call::post(&uri, json!({ "revision_id": rev }))
            .token(token)
            .tenant(shop.tenant)
    };
    let experiment_id = async |token| {
        let (_, body, _) = Call::get("/admin/v1/insights")
            .token(token)
            .tenant(shop.tenant)
            .send(&s)
            .await;
        body["items"][0]["experiment_id"].clone()
    };

    let (status, problem, _) = test(&clerk).send(&s).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("insufficient_role"))
    );
    let (_, none, _) = Call::get("/admin/v1/experiments")
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(none, json!({ "items": [] }));

    let (status, e, _) = test(&boss).send(&s).await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert_eq!(
        (
            &e["status"],
            &e["change"]["kind"],
            &e["change"]["revision_id"]
        ),
        (&json!("draft"), &json!("theme"), &json!(rev))
    );
    let (status, again, _) = test(&boss).send(&s).await;
    assert_eq!((status, &again["id"]), (StatusCode::OK, &e["id"]));
    assert_eq!(experiment_id(&boss).await, e["id"]);
    assert_eq!(experiment_id(&clerk).await, json!(null));
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_insights_experiment_is_prepared_while_another_runs_but_does_not_start(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "ins-busy").await;
    testkit::staff(&runtime, shop.tenant, "boss", "admin").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let boss = sign(&claims("boss"));
    crate::experiments::start(&runtime, shop.tenant).await;
    let (insight, rev) = accepted_revision(&runtime, shop.tenant).await;

    let (status, e, _) = Call::post(
        &format!("/admin/v1/insights/{insight}/experiments"),
        json!({ "revision_id": rev }),
    )
    .token(&boss)
    .tenant(shop.tenant)
    .send(&s)
    .await;
    assert_eq!(
        (status, &e["status"]),
        (StatusCode::OK, &json!("draft")),
        "{e}"
    );
    let start = format!("/admin/v1/experiments/{}/start", e["id"].as_str().unwrap());
    let (status, problem, _) = Call::post_raw(&start, "", "application/json")
        .token(&boss)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::CONFLICT, Some("experiment_running"))
    );
}
