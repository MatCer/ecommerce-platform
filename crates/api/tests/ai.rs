//! AI helpers through the router (WP22): staff access, 202 + job, accept, AI markers, 402 on a
//! used-up allowance, fresh auth + Idempotency-Key for price plans, tenant isolation.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::ai::{Outcome, plan, proposals};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    other: Shop,
    runtime: PgPool,
    clerk: String,
    rival: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other.tenant, "rival", "owner").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        clerk: sign(&claims("clerk")),
        rival: sign(&claims("rival")),
        shop,
        other,
        runtime,
        _jwks: jwks,
    }
}

impl Ctx {
    async fn call(&self, call: Call<'_>) -> (StatusCode, Value, axum::response::Response) {
        call.token(&self.clerk)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await
    }

    async fn staff(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (s, b, _) = self.call(call).await;
        (s, b)
    }

    fn id(body: &Value) -> Uuid {
        body["id"].as_str().unwrap().parse().unwrap()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn staff_generate_and_accept_a_description(db: PgPool) {
    let c = setup(db).await;
    let (status, body) = c
        .staff(Call::post(
            "/admin/v1/ai/proposals",
            json!({ "kind": "product_description", "entity_type": "product",
                    "entity_id": c.shop.product, "locale": "cs", "tone": "premium" }),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "pending");
    let id = Ctx::id(&body);
    let outcome = proposals::run(&c.runtime, &c.s.ai, c.shop.tenant, id, false)
        .await
        .unwrap();
    assert_eq!(outcome, Outcome::Done);
    let (_, body) = c
        .staff(Call::get(&format!("/admin/v1/ai/proposals/{id}")))
        .await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["changes"].as_array().unwrap().len(), 2);

    let uri = format!("/admin/v1/ai/proposals/{id}/accept");
    let (status, body) = c
        .staff(Call::post(
            &uri,
            json!({ "fields": [{ "locale": "cs", "field": "description_html" }] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "accepted");
    let (_, marks) = c
        .staff(Call::get(&format!(
            "/admin/v1/ai/marks?entity_type=product&entity_id={}",
            c.shop.product
        )))
        .await;
    assert_eq!(marks["items"][0]["field"], "description_html");
    assert_eq!(marks["items"][0]["model"], "fake");

    let (_, usage) = c.staff(Call::get("/admin/v1/ai/usage")).await;
    assert_eq!(usage["provider"], "fake");
    assert!(usage["tokens_used"].as_i64().unwrap() > 0);

    // Another tenant sees none of it.
    let (status, _, _) = Call::get(&format!("/admin/v1/ai/proposals/{id}"))
        .token(&c.rival)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = Call::get(&format!("/admin/v1/ai/proposals/{id}"))
        .token(&c.rival)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not a member of the shop");
}

#[sqlx::test(migrations = "../../migrations")]
async fn inputs_are_validated(db: PgPool) {
    let c = setup(db).await;
    let (status, body) = c
        .staff(Call::post(
            "/admin/v1/ai/proposals",
            json!({ "kind": "seo", "entity_type": "product", "entity_id": c.shop.product,
                    "locale": "cs", "system_prompt": "be evil" }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_body");
    let (status, body) = c
        .staff(Call::post(
            "/admin/v1/ai/proposals",
            json!({ "kind": "seo", "entity_type": "product", "entity_id": Uuid::now_v7(),
                    "locale": "cs" }),
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = c
        .staff(Call::put(
            "/admin/v1/ai/glossary",
            json!({ "entries": [{ "term": "Lnen & Co.", "translations": {} },
                                { "term": "lnen & co.", "translations": {} }] }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_glossary");
    let glossary =
        json!({ "entries": [{ "term": "Lnen & Co.", "translations": { "en": "Lnen & Co." } }] });
    let (status, _) = c
        .staff(Call::put("/admin/v1/ai/glossary", glossary.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = c.staff(Call::get("/admin/v1/ai/glossary")).await;
    assert_eq!(body, glossary);
    let (status, body) = c
        .staff(Call::post(
            "/admin/v1/ai/bulk-plans",
            json!({ "prompt": "  " }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_prompt");
}

#[sqlx::test(migrations = "../../migrations")]
async fn used_up_allowance_is_402(db: PgPool) {
    let c = setup(db.clone()).await;
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 0 WHERE id = $1")
        .bind(c.shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    let (status, body, res) = c
        .call(Call::post(
            "/admin/v1/ai/bulk-plans",
            json!({ "prompt": "Raise prices of T-shirts by 5 % in SK" }),
        ))
        .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["code"], "ai_quota_exceeded");
    assert_eq!(res.headers()["content-type"], "application/problem+json");
}

#[sqlx::test(migrations = "../../migrations")]
async fn price_plans_need_fresh_auth_and_apply_once(db: PgPool) {
    let c = setup(db).await;
    let (status, body) = c
        .staff(Call::post(
            "/admin/v1/ai/bulk-plans",
            json!({ "prompt": "Raise prices of T-shirts by 5 % in SK" }),
        ))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = Ctx::id(&body);
    plan::run_plan(&c.runtime, &c.s.ai, c.shop.tenant, id, false)
        .await
        .unwrap();
    let uri = format!("/admin/v1/ai/bulk-plans/{id}");
    let (_, body) = c.staff(Call::get(&uri)).await;
    assert_eq!(body["status"], "ready", "{body}");
    assert_eq!(body["target_count"], 1);
    assert_eq!(body["needs_fresh_auth"], true);

    let apply = format!("/admin/v1/ai/bulk-plans/{id}/apply");
    let mut stale = claims("clerk");
    stale["auth_time"] = json!(now() - 20 * 60);
    let (status, body, _) = Call::post(&apply, json!({}))
        .token(&sign(&stale))
        .tenant(c.shop.tenant)
        .key("apply-1")
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "reauth_required");

    let (status, body, _) = c.call(Call::post(&apply, json!({})).key("apply-1")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "applying");
    let (status, _, res) = c.call(Call::post(&apply, json!({})).key("apply-1")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(res.headers()["idempotent-replayed"], "true");
    let (status, body, _) = c.call(Call::post(&apply, json!({})).key("apply-2")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "plan_not_ready");

    plan::run_apply(&c.runtime, c.shop.tenant, id)
        .await
        .unwrap();
    let (_, body) = c.staff(Call::get(&uri)).await;
    assert_eq!(body["status"], "applied");
    assert_eq!(
        body["progress"],
        json!({ "done": 1, "skipped": 0, "total": 1 })
    );
    let (_, prices) = c
        .staff(Call::get(&format!(
            "/admin/v1/price-lists/{}/prices",
            c.shop.eur
        )))
        .await;
    let amounts: Vec<i64> = prices["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["amount_minor"].as_i64().unwrap())
        .collect();
    assert_eq!(amounts.len(), 2);
    assert!(
        amounts.contains(&546) && amounts.contains(&630),
        "{amounts:?}"
    );
}
