//! Data portability through the router (WP13b): roles, fresh sign-in for downloads and
//! privacy requests (A9), the import lifecycle, exports and GDPR access/erasure.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use serde_json::json;
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
    let shop = testkit::storefront::shop(&runtime, "port").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        shop,
        runtime,
        _jwks: jwks,
    }
}

fn stale(user: &str) -> String {
    let mut c = claims(user);
    c["auth_time"] = json!(now() - 3600);
    sign(&c)
}

#[sqlx::test(migrations = "../../migrations")]
async fn csv_import_lifecycle_through_the_api(db: PgPool) {
    let c = setup(db).await;
    let (boss, clerk) = (sign(&claims("boss")), sign(&claims("clerk")));
    let csv =
        "order_number;placed_at;e-mail;currency;total\nA-1;2024-01-02;a@example.com;CZK;10,50\n";
    let body = json!({
        "kind": "orders", "market_id": c.shop.cz, "upload_size": csv.len(),
        "mapping": { "email": "e-mail" }
    });
    let (status, _, _) = Call::post("/admin/v1/data-imports", body.clone())
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, err, _) = Call::post(
        "/admin/v1/data-imports",
        json!({"kind": "orders", "market_id": c.shop.cz, "upload_size": 21 * 1024 * 1024}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("file_too_large"))
    );
    let (status, created, _) = Call::post("/admin/v1/data-imports", body)
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["upload"]["headers"]["content-type"], "text/csv");
    let id: Uuid = created["import"]["id"].as_str().unwrap().parse().unwrap();
    let key = Path::from(format!("data-import-uploads/{}/{id}.csv", c.shop.tenant));
    c.s.storage
        .private
        .put(&key, PutPayload::from(csv.as_bytes().to_vec()))
        .await
        .unwrap();

    let uri = format!("/admin/v1/data-imports/{id}/analyze");
    let (status, err, _) = Call::post(&uri, json!({"mapping": {"password": "x"}}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_mapping"))
    );
    let (status, _, _) = Call::post(&uri, json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    commerce::portability::imports::run_step(
        &c.runtime,
        &c.s.storage,
        c.shop.tenant,
        id,
        "analyze",
    )
    .await
    .unwrap();
    let get = format!("/admin/v1/data-imports/{id}");
    let (_, run, _) = Call::get(&get)
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(run["status"], "analyzed", "{run}");
    assert_eq!(run["report"]["records"], 1);

    let (status, _, _) = Call::post(&format!("/admin/v1/data-imports/{id}/apply"), json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    commerce::portability::imports::run_step(&c.runtime, &c.s.storage, c.shop.tenant, id, "apply")
        .await
        .unwrap();
    let (_, run, _) = Call::get(&get)
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(run["status"], "applied", "{run}");

    // Staff may read the archive.
    let (status, page, _) = Call::get("/admin/v1/archived-orders?q=A-1")
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0]["total_minor"], 1050);
    let (status, list, _) = Call::get("/admin/v1/data-imports")
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, list["items"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn exports_and_privacy_requests_need_a_fresh_sign_in(db: PgPool) {
    let c = setup(db).await;
    let (boss, clerk) = (sign(&claims("boss")), sign(&claims("clerk")));
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query("INSERT INTO customers (tenant_id, email, name, locale) VALUES ($1, 'anna@example.com', 'Anna', 'cs')")
        .bind(c.shop.tenant)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Export: admin only, one at a time, download needs a fresh sign-in.
    let (status, _, _) = Call::post("/admin/v1/data-exports", json!({}))
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, err, _) = Call::post("/admin/v1/data-exports", json!({}))
        .tenant(c.shop.tenant)
        .token(&stale("boss"))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (status, e, _) = Call::post("/admin/v1/data-exports", json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, err, _) = Call::post("/admin/v1/data-exports", json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::CONFLICT, Some("export_busy"))
    );
    let id: Uuid = e["id"].as_str().unwrap().parse().unwrap();
    let download = format!("/admin/v1/data-exports/{id}/download");
    let (status, _, _) = Call::post(&download, json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not ready yet");
    commerce::portability::export::run(&c.runtime, &c.s.storage, c.shop.tenant, id)
        .await
        .unwrap();
    let (status, err, _) = Call::post(&download, json!({}))
        .tenant(c.shop.tenant)
        .token(&stale("boss"))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (status, link, _) = Call::post(&download, json!({}))
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        link["url"]
            .as_str()
            .unwrap()
            .contains(&format!("exports/{}/{id}.zip", c.shop.tenant))
    );

    // Access: fresh sign-in, a JSON attachment.
    let (status, _, _) = Call::post(
        "/admin/v1/privacy/access",
        json!({"email": "anna@example.com"}),
    )
    .tenant(c.shop.tenant)
    .token(&stale("boss"))
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, doc, res) = Call::post(
        "/admin/v1/privacy/access",
        json!({"email": "Anna@example.com"}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["customer"]["name"], "Anna");
    assert!(
        res.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );

    // Erasure: staff cannot; the confirmation must match; then it is gone.
    let body = json!({"email": "anna@example.com", "confirm_email": "anna@example.com"});
    let (status, _, _) = Call::post("/admin/v1/privacy/erasure", body.clone())
        .tenant(c.shop.tenant)
        .token(&clerk)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, err, _) = Call::post(
        "/admin/v1/privacy/erasure",
        json!({"email": "anna@example.com", "confirm_email": "x@example.com"}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert_eq!(
        (status, err["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("confirmation_mismatch")
        )
    );
    let (status, report, _) = Call::post("/admin/v1/privacy/erasure", body)
        .tenant(c.shop.tenant)
        .token(&boss)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert!(report["customer_id"].is_string());
    let (_, doc, _) = Call::post(
        "/admin/v1/privacy/access",
        json!({"email": "anna@example.com"}),
    )
    .tenant(c.shop.tenant)
    .token(&boss)
    .send(&c.s)
    .await;
    assert!(doc["customer"].is_null());
}
