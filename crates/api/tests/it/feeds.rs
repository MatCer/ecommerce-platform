//! Feeds Admin API and served export feeds (WP13a, A28), search synonyms.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;

use crate::common::*;

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

impl Ctx {
    async fn as_user(&self, token: &str, call: Call<'_>) -> (StatusCode, Value) {
        let (s, b, _) = call
            .token(token)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await;
        (s, b)
    }

    async fn count(&self, sql: &'static str) -> i64 {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap()
    }

    async fn sql(&self, sql: &'static str) {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
}

fn store(c: &Ctx, url: &str) -> Value {
    json!({ "source": "store", "market_id": c.shop.cz, "url": url })
}

#[sqlx::test(migrations = "../../migrations")]
async fn store_imports_are_owner_admin_only_https_and_audited(db: PgPool) {
    let c = setup(db).await;
    testkit::staff(&c.runtime, c.shop.tenant, "manager", "admin").await;
    let manager = sign(&claims("manager"));
    let shop = store(&c, "https://shop.example/");

    let (status, _) = c
        .as_user(&c.clerk, Call::post("/admin/v1/imports", shop.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(c.count("SELECT count(*) FROM import_runs").await, 0);
    assert_eq!(
        c.count("SELECT count(*) FROM audit_log WHERE entity = 'import_run'")
            .await,
        0
    );

    let (status, created) = c
        .as_user(&manager, Call::post("/admin/v1/imports", shop.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["run"]["source"], "store");
    assert_eq!(created["run"]["status"], "analyzing");
    let (status, busy) = c.admin(Call::post("/admin/v1/imports", shop.clone())).await;
    assert_eq!(
        (status, busy["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_busy"))
    );
    c.sql("UPDATE import_runs SET status = 'failed'").await;
    let (status, _) = c.admin(Call::post("/admin/v1/imports", shop)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        c.count(
            "SELECT count(*) FROM audit_log WHERE action = 'import.created'
             AND entity = 'import_run' AND diff ->> 'source' = 'store'"
        )
        .await,
        2
    );

    for (url, code) in [
        ("http://shop.example/", "https_required"),
        ("https://127.0.0.1/", "blocked_url"),
        ("https://[::1]/", "blocked_url"),
    ] {
        let (status, body) = c
            .admin(Call::post("/admin/v1/imports", store(&c, url)))
            .await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(code)),
            "{url}"
        );
    }
    let mut drafts = store(&c, "https://shop.example/");
    drafts["activate"] = json!(true);
    let (status, body) = c.admin(Call::post("/admin/v1/imports", drafts)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("store_drafts_only"))
    );
    assert_eq!(c.count("SELECT count(*) FROM import_runs").await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn brand_needs_admin_and_a_suggestion(db: PgPool) {
    let c = setup(db).await;
    let (_, created) = c
        .admin(Call::post(
            "/admin/v1/imports",
            store(&c, "https://shop.example/"),
        ))
        .await;
    let id = created["run"]["id"].as_str().unwrap().to_owned();
    let brand = format!("/admin/v1/imports/{id}/brand");

    let (status, _) = c
        .as_user(&c.clerk, Call::post(&brand, json!({ "name": true })))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = c.admin(Call::post(&brand, json!({}))).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("nothing_to_apply"))
    );
    let (status, body) = c.admin(Call::post(&brand, json!({ "name": true }))).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("no_brand_suggestion")),
        "the analysis has not run yet"
    );

    let report = commerce::feeds::import::ImportReport {
        site: Some(commerce::feeds::store::SiteReport {
            name: Some("Nová Kočka".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query("UPDATE import_runs SET status = 'analyzed', report = $1")
        .bind(serde_json::to_value(&report).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, body) = c.admin(Call::post(&brand, json!({ "logo": true }))).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("no_brand_suggestion")),
        "no logo was found"
    );
    let (status, body) = c.admin(Call::post(&brand, json!({ "name": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "name": "Nová Kočka", "logo_asset_id": null }));
    assert_eq!(
        c.count("SELECT count(*) FROM audit_log WHERE action = 'import.brand_applied'")
            .await,
        1
    );
}
