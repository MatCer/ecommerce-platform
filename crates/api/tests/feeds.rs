//! Feeds Admin API and served export feeds (WP13a, A28), search synonyms.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    runtime: PgPool,
    owner: String,
    clerk: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        owner: sign(&claims("boss")),
        clerk: sign(&claims("clerk")),
        shop,
        runtime,
        _jwks: jwks,
    }
}

impl Ctx {
    async fn admin(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (s, b, _) = call
            .token(&self.owner)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await;
        (s, b)
    }

    async fn feed(&self, uri: &str, market: uuid::Uuid) -> (StatusCode, String) {
        let (s, b, _) = Call::get(uri)
            .header("x-storefront-token", self.shop.token.clone())
            .header("x-market", market.to_string())
            .send_text(&self.s)
            .await;
        (s, b)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn export_feeds_are_generated_and_served_per_market(db: PgPool) {
    let c = setup(db).await;
    let (status, _) = c
        .feed("/storefront/v1/files/feeds/cz/google.xml", c.shop.cz)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not generated yet");

    let written = commerce::feeds::export::generate(
        &c.runtime,
        &c.s.storage,
        &c.s.public_urls,
        c.shop.tenant,
    )
    .await
    .unwrap();
    assert_eq!(written, 6, "2 markets x 3 channels");

    let (status, google) = c
        .feed("/storefront/v1/files/feeds/cz/google.xml", c.shop.cz)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(google.contains("xmlns:g=\"http://base.google.com/ns/1.0\""));
    assert!(google.contains("<g:price>129.00 CZK</g:price>"), "{google}");
    // Each variant links to its own offer (the product page preselects it).
    assert!(
        google.contains("http://shop.localhost:8080/p/tee-cs?variant=TEE-"),
        "{google}"
    );
    let (_, heureka) = c
        .feed("/storefront/v1/files/feeds/sk/heureka.xml", c.shop.cz)
        .await;
    assert!(heureka.contains("<PRICE_VAT>5.20</PRICE_VAT>"), "{heureka}");
    let (_, zbozi) = c
        .feed("/storefront/v1/files/feeds/cz/zbozi.xml", c.shop.cz)
        .await;
    assert!(zbozi.contains("http://www.zbozi.cz/ns/offer/1.0"));
    for bad in [
        "/storefront/v1/files/feeds/cz/other.xml",
        "/storefront/v1/files/feeds/xx/google.xml",
        "/storefront/v1/files/feeds/cz/google",
    ] {
        assert_eq!(
            c.feed(bad, c.shop.cz).await.0,
            StatusCode::NOT_FOUND,
            "{bad}"
        );
    }

    let (_, list) = c.admin(Call::get("/admin/v1/feeds")).await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 6);
    let google_cz = items
        .iter()
        .find(|f| f["market_code"] == "cz" && f["channel"] == "google")
        .unwrap();
    assert_eq!(
        google_cz["url"],
        "http://shop.localhost:8080/feeds/cz/google.xml"
    );
    assert_eq!(google_cz["items"], 2);
    assert_eq!(
        c.admin(Call::post("/admin/v1/feeds/regenerate", json!({})))
            .await
            .0,
        StatusCode::ACCEPTED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn imports_start_with_an_upload_or_a_url(db: PgPool) {
    let c = setup(db).await;
    let (status, created) = c
        .admin(
            Call::post(
                "/admin/v1/imports",
                json!({ "source": "heureka", "market_id": c.shop.cz, "upload_size": 1000 }),
            )
            .key("imp-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["run"]["status"], "pending");
    assert_eq!(created["upload"]["method"], "PUT");
    let id = created["run"]["id"].as_str().unwrap().to_owned();
    // Applying before a dry run is refused.
    let (status, body) = c
        .admin(Call::post(
            &format!("/admin/v1/imports/{id}/apply"),
            json!({}),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_not_analyzed"))
    );
    let (status, run) = c
        .admin(Call::post(
            &format!("/admin/v1/imports/{id}/analyze"),
            json!({}),
        ))
        .await;
    assert_eq!(
        (status, run["status"].as_str()),
        (StatusCode::ACCEPTED, Some("analyzing"))
    );

    let (status, created) = c
        .admin(Call::post(
            "/admin/v1/imports",
            json!({ "source": "google", "market_id": c.shop.cz, "url": "https://feeds.example/g.xml" }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["run"]["status"], "analyzing");
    assert!(created["upload"].is_null());
    let (status, _) = c
        .admin(Call::post(
            "/admin/v1/imports",
            json!({ "source": "google", "market_id": c.shop.cz, "url": "file:///etc/passwd" }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, list) = c.admin(Call::get("/admin/v1/imports")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);

    let (status, _, _) = Call::post("/admin/v1/feeds/regenerate", json!({}))
        .token(&c.clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "regeneration is owner/admin");
    // Imports change the catalog wholesale: owner/admin only.
    let (status, _, _) = Call::get("/admin/v1/imports")
        .token(&c.clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "../../migrations")]
async fn synonyms_are_saved_and_queued_for_the_indexes(db: PgPool) {
    let c = setup(db).await;
    let (status, saved) = c
        .admin(Call::put(
            "/admin/v1/search/synonyms",
            json!({ "groups": [["mikina", "hoodie"], ["tričko", "triko"]] }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let (_, got) = c.admin(Call::get("/admin/v1/search/synonyms")).await;
    assert_eq!(
        got["groups"],
        json!([["mikina", "hoodie"], ["tričko", "triko"]])
    );
    let (status, _) = c
        .admin(Call::put(
            "/admin/v1/search/synonyms",
            json!({ "groups": [["jen jedno"]] }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
