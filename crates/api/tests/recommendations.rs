//! Recommendations through the router: the public variant is cacheable, anything with a cart
//! or consent subject is private (A2); personal contexts need server-side `personalization`
//! consent (A20); admin collections, settings (admin role) and explanations.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject, new_anon_id};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    runtime: PgPool,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "reco").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        shop,
        runtime,
        _jwks: jwks,
    }
}

fn sf<'a>(call: Call<'a>, shop: &Shop) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
}

async fn grant(c: &Ctx, subject: &str, personalization: bool) {
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    consent::record(
        &mut tx,
        &Subject::Anon(subject.into()),
        &ConsentChoice {
            purposes: Purposes {
                analytics: Some(true),
                personalization: Some(personalization),
                ..Purposes::default()
            },
            text_version: "v1".into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn product_ids(body: &Value) -> Vec<String> {
    body["products"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn public_variant_is_cacheable_and_private_variant_is_not(db: PgPool) {
    let c = setup(db).await;
    let uri = format!(
        "/storefront/v1/recommendations?context=category:{}&limit=4",
        c.shop.category
    );
    let (status, body, res) = sf(Call::get(&uri), &c.shop).send(&c.s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cache"]["public"], json!(true));
    assert!(res.headers().get("cache-control").is_none());
    assert_eq!(product_ids(&body), [c.shop.product.to_string()]);
    assert_eq!(body["strategy"], json!("newest"), "no sales yet");

    let subject = new_anon_id();
    let (status, body, res) = sf(Call::get(&uri), &c.shop)
        .header("x-consent-subject", subject.clone())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cache"]["public"], json!(false));
    assert_eq!(res.headers()["cache-control"], "private, no-store");

    for bad in ["product:x", "nope", "cart:1"] {
        let (status, body, _) = sf(
            Call::get(&format!("/storefront/v1/recommendations?context={bad}")),
            &c.shop,
        )
        .send(&c.s)
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(body["code"], "invalid_context");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn recently_viewed_needs_personalization_consent(db: PgPool) {
    let c = setup(db).await;
    let uri = format!(
        "/storefront/v1/recommendations?context=recent&ids={},{}",
        c.shop.product,
        Uuid::now_v7()
    );
    // No subject (SSR, /_p/public): nothing, whatever ids are sent.
    let (_, body, _) = sf(Call::get(&uri), &c.shop).send(&c.s).await;
    assert_eq!(product_ids(&body), Vec::<String>::new());

    let subject = new_anon_id();
    grant(&c, &subject, false).await;
    let call = |subject: &str| {
        sf(Call::get(&uri), &c.shop).header("x-consent-subject", subject.to_owned())
    };
    let (_, body, _) = call(&subject).send(&c.s).await;
    assert_eq!(
        product_ids(&body),
        Vec::<String>::new(),
        "analytics alone is not enough"
    );

    grant(&c, &subject, true).await;
    let (_, body, res) = call(&subject).send(&c.s).await;
    assert_eq!(product_ids(&body), [c.shop.product.to_string()]);
    assert_eq!(body["strategy"], json!("recently_viewed"));
    assert_eq!(res.headers()["cache-control"], "private, no-store");
}

#[sqlx::test(migrations = "../../migrations")]
async fn cart_cross_sell_leaves_out_the_cart(db: PgPool) {
    let c = setup(db).await;
    let (status, _, res) = sf(Call::post("/storefront/v1/cart", json!({})), &c.shop)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/cart/lines",
            json!({ "variant_id": c.shop.variants[0], "quantity": 1 }),
        ),
        &c.shop,
    )
    .header("x-cart-token", token.clone())
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, res) = sf(
        Call::get("/storefront/v1/recommendations?context=cart"),
        &c.shop,
    )
    .header("x-cart-token", token)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res.headers()["cache-control"], "private, no-store");
    assert!(!product_ids(&body).contains(&c.shop.product.to_string()));
    // Without the cart the same product is fair game.
    let (_, body, _) = sf(
        Call::get("/storefront/v1/recommendations?context=cart"),
        &c.shop,
    )
    .send(&c.s)
    .await;
    assert_eq!(product_ids(&body), [c.shop.product.to_string()]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_collections_settings_and_explain(db: PgPool) {
    let c = setup(db).await;
    let owner = sign(&claims("boss"));
    let clerk = sign(&claims("clerk"));

    let input = json!({
        "name": "Podzim",
        "title_i18n": { "cs": "Podzimní tipy" },
        "kind": "seasonal",
        "starts_at": "2026-01-01T00:00:00Z",
        "ends_at": "2099-01-01T00:00:00Z",
        "product_ids": [c.shop.product]
    });
    let (status, created, _) = Call::post("/admin/v1/collections", input.clone())
        .token(&clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["active"], json!(true));
    let id = created["id"].as_str().unwrap().to_owned();

    let mut bad = input.clone();
    bad["ends_at"] = Value::Null;
    let (status, body, _) = Call::post("/admin/v1/collections", bad)
        .token(&clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_schedule"))
    );

    // Home shows the open seasonal collection under its title.
    let (_, home, _) = sf(Call::get("/storefront/v1/pages/home"), &c.shop)
        .send(&c.s)
        .await;
    assert_eq!(home["featured_strategy"], json!("seasonal"));
    assert_eq!(home["featured_title"], json!("Podzimní tipy"));

    // Settings: staff read, only admins write.
    let settings = json!({
        "bestsellers": true, "bought_together": true, "seasonal": false,
        "recently_viewed": true, "personalized": true,
        "excluded_product_ids": [c.shop.product]
    });
    let (status, _, _) = Call::put("/admin/v1/recommendations/settings", settings.clone())
        .token(&clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, saved, _) = Call::put("/admin/v1/recommendations/settings", settings)
        .token(&owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["seasonal"], json!(false));

    // Why: the excluded product is listed with its reason.
    let uri = format!(
        "/admin/v1/recommendations/explain?market_id={}&context=collection:{id}",
        c.shop.cz
    );
    let (status, why, _) = Call::get(&uri)
        .token(&clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{why}");
    assert_eq!(
        why["result"]["chain"],
        json!(["collection", "bestsellers", "newest"])
    );
    assert_eq!(why["result"]["items"], json!([]));
    assert_eq!(why["result"]["skipped"][0]["reason"], json!("excluded"));
    assert_eq!(why["personalization"], json!(false));

    let (status, _, _) = Call::delete(&format!("/admin/v1/collections/{id}"))
        .token(&clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
