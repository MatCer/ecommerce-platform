//! Reviews through the router (WP16): the review link on the storefront API (read vs. submit,
//! single use, validation, no-store), moderation in the admin API (staff roles, transitions,
//! reply, tenant isolation) and the product page model with JSON-LD.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::{Shop, raw_order};

mod common;
use common::*;

fn sf<'a>(call: Call<'a>, shop: &Shop) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
        .header("x-client-ip", "203.0.113.7")
}

async fn token(runtime: &PgPool, shop: &Shop) -> String {
    let order = raw_order(runtime, shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let mut tx = platform::db::tenant_tx(runtime, shop.tenant).await.unwrap();
    let t = commerce::reviews::issue_tokens(&mut tx, order, Utc::now())
        .await
        .unwrap()
        .remove(0)
        .token;
    tx.commit().await.unwrap();
    t
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_link_moderation_and_product_page(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "rev").await;
    let other = testkit::storefront::shop(&runtime, "rev-other").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other.tenant, "stranger", "owner").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let t = token(&runtime, &shop).await;

    // Reading the link changes nothing and is never cached.
    let uri = format!("/storefront/v1/reviews/invitation?token={t}");
    let (status, body, res) = sf(Call::get(&uri), &shop).send(&s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["product_name"], "Tričko");
    assert_eq!(res.headers()["cache-control"], "no-store");
    let (status, _, _) = sf(Call::get(&uri), &other).send(&s).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another shop's token");

    let review = |rating: i64| json!({ "token": t, "rating": rating, "name": "Jana", "title": "", "body": "Hezké.\u{0}" });
    let (status, body, _) = sf(Call::post("/storefront/v1/reviews", review(9)), &shop)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_rating"))
    );
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/reviews",
            json!({ "token": t, "rating": 5, "name": "x", "body": "x", "html": "<b>" }),
        ),
        &shop,
    )
    .send(&s)
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields refused"
    );
    let (status, body, _) = sf(Call::post("/storefront/v1/reviews", review(5)), &shop)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::CREATED, Some("pending"))
    );
    let (status, _, _) = sf(Call::post("/storefront/v1/reviews", review(5)), &shop)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "single use");

    // Moderation: the queue, a reply, publish.
    let clerk = sign(&claims("clerk"));
    let (status, body, _) = Call::get("/admin/v1/reviews?status=pending")
        .token(&clerk)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let item = &body["items"][0];
    assert_eq!(item["body"], "Hezké.", "control characters dropped");
    assert_eq!(item["verified"], true);
    let id = item["id"].as_str().unwrap().to_owned();

    let stranger = sign(&claims("stranger"));
    let (status, body, _) = Call::get("/admin/v1/reviews")
        .token(&stranger)
        .tenant(other.tenant)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["items"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(0))
    );
    let (status, _, _) = Call::put(
        &format!("/admin/v1/reviews/{id}/status"),
        json!({ "status": "published" }),
    )
    .token(&stranger)
    .tenant(other.tenant)
    .send(&s)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-tenant moderation");

    let (status, body, _) = Call::put(
        &format!("/admin/v1/reviews/{id}/status"),
        json!({ "status": "hidden" }),
    )
    .token(&clerk)
    .tenant(shop.tenant)
    .send(&s)
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_transition"))
    );
    let (status, body, _) = Call::put(
        &format!("/admin/v1/reviews/{id}/reply"),
        json!({ "reply": "Díky!" }),
    )
    .token(&clerk)
    .tenant(shop.tenant)
    .send(&s)
    .await;
    assert_eq!(
        (status, body["reply"].as_str()),
        (StatusCode::OK, Some("Díky!"))
    );
    let (status, body, _) = Call::put(
        &format!("/admin/v1/reviews/{id}/status"),
        json!({ "status": "published" }),
    )
    .token(&clerk)
    .tenant(shop.tenant)
    .send(&s)
    .await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("published"))
    );

    // The product page model shows it, with the rating in the Product JSON-LD.
    let (status, body, _) = sf(
        Call::get(&format!("/storefront/v1/pages/product/{}", shop.slug)),
        &shop,
    )
    .send(&s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["reviews"]["summary"]["count"], 1);
    assert_eq!(body["reviews"]["items"][0]["reply"], "Díky!");
    assert_eq!(
        body["seo"]["json_ld"][0]["aggregateRating"]["ratingValue"],
        "5.0"
    );

    // Without a staff membership: refused.
    let (status, _, _) = Call::get("/admin/v1/reviews")
        .token(&stranger)
        .tenant(shop.tenant)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
