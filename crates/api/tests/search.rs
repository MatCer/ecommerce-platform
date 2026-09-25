//! Storefront search and search Admin API over HTTP (spec §8.2, §11.1, A27).
#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::*;

struct Shop {
    s: api::AppState,
    runtime: PgPool,
    tenant: Uuid,
    market: Uuid,
    _jwks: Jwks,
}

/// A tenant with staff and a `cz` market; `publish` adds a verified domain for it.
async fn shop(db: &PgPool, slug: &str, publish: bool) -> Shop {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(db, 4).await;
    let (tenant, market) = testkit::tenant(&runtime, slug).await;
    testkit::staff(&runtime, tenant, "boss", "owner").await;
    testkit::staff(&runtime, tenant, "clerk", "staff").await;
    if publish {
        sqlx::query(
            "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
             VALUES ($1 || '.localhost', $2, $3, true, now())",
        )
        .bind(slug)
        .bind(tenant)
        .bind(market)
        .execute(db)
        .await
        .unwrap();
    }
    Shop {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        runtime,
        tenant,
        market,
        _jwks: jwks,
    }
}

impl Shop {
    async fn storefront(
        &self,
        uri: &str,
        locale: &str,
    ) -> (StatusCode, Value, axum::http::HeaderMap) {
        let req = Request::get(uri)
            .header("x-tenant", self.tenant.to_string())
            .header("x-market", self.market.to_string())
            .header("x-locale", locale)
            .body(Body::empty())
            .unwrap();
        let res = api::app(self.s.clone(), false).oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            headers,
        )
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn storefront_context_must_be_a_published_market(db: PgPool) {
    let unpublished = shop(&db, "draft", false).await;
    let (status, body, _) = unpublished
        .storefront("/storefront/v1/search?q=x", "cs")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no verified domain: {body}");

    // Missing or malformed context headers.
    let res = api::app(unpublished.s.clone(), false)
        .oneshot(
            Request::get("/storefront/v1/search")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    drop(unpublished);

    let published = shop(&db, "live", true).await;
    let (status, _, _) = published
        .storefront("/storefront/v1/search?q=x", "de")
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the market does not sell in de"
    );
    let (status, body, _) = published
        .storefront("/storefront/v1/search?f.price.cz=1", "cs")
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_filter"))
    );
    let (status, body, _) = published
        .storefront("/storefront/v1/search?nope=1", "cs")
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );

    // Meilisearch is down in this harness: search degrades to 503, nothing else breaks.
    let (status, body, _) = published
        .storefront("/storefront/v1/search?q=tricko", "cs")
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "service_unavailable");
    let (status, _, _) = published
        .storefront("/storefront/v1/search/suggest?q=tri", "cs")
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // Another tenant's market id with this tenant id is not a valid context either.
    let other = testkit::tenant(&published.runtime, "other").await.1;
    let forged = Shop {
        market: other,
        ..published
    };
    let (status, _, _) = forged.storefront("/storefront/v1/search?q=x", "cs").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rebuild_needs_an_admin_and_is_queued_and_audited(db: PgPool) {
    let s = shop(&db, "shop", false).await;
    let (owner, clerk) = (sign(&claims("boss")), sign(&claims("clerk")));
    let (status, _, _) = Call::post("/admin/v1/search/rebuild", json!({}))
        .token(&clerk)
        .tenant(s.tenant)
        .send(&s.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body, _) = Call::post("/admin/v1/search/rebuild", json!({}))
        .token(&owner)
        .tenant(s.tenant)
        .send(&s.s)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job_id = body["job_id"].as_i64().unwrap();
    let (kind, tenant, versioned, due): (String, Option<Uuid>, bool, bool) = sqlx::query_as(
        "SELECT kind, tenant_id, payload ? 'version', run_at <= now()
         FROM queue.jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), tenant, versioned, due),
        ("search.rebuild", Some(s.tenant), true, true)
    );

    let (status, body, _) = Call::get("/admin/v1/search/status")
        .token(&clerk)
        .tenant(s.tenant)
        .send(&s.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "indexes": [] }));

    let mut tx = platform::db::tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'search.rebuild_requested'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(audited >= 1);
}

/// Through the real engine with the API's search-only key.
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn storefront_search_and_suggest_end_to_end(db: PgPool) {
    let mut s = shop(&db, "shop", true).await;
    s.s.meili = testkit::meili_search();
    let admin = testkit::meili();
    let mut tx = platform::db::tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let list = commerce::pricing::create_price_list(
        &mut tx,
        "t",
        &commerce::pricing::NewPriceList {
            code: "cz".into(),
            name: "cz".into(),
            currency: commerce::money::Currency::Czk,
            market_ids: vec![s.market],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let cat = testkit::catalog::category(&s.runtime, s.tenant, "tricka", None).await;
    let mut input = testkit::catalog::product_input("TS", 2);
    input.translations[0].name = "Pánské tričko".into();
    input.category_ids = vec![cat.id];
    let product = testkit::catalog::create(&s.runtime, s.tenant, &input).await;
    testkit::pricing::set_prices(
        &s.runtime,
        s.tenant,
        list.id,
        &[
            (product.variants[0].id, 29_900),
            (product.variants[1].id, 31_900),
        ],
    )
    .await;
    commerce::search::index::index_product(&s.runtime, &admin, s.tenant, product.id, None)
        .await
        .unwrap();
    admin
        .wait_idle(
            &commerce::search::index_uid(s.tenant, "cs"),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

    let (status, body, headers) = s
        .storefront(
            "/storefront/v1/search?q=panska%20tricka&f.opt.size=v2",
            "cs",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);
    let hit = &body["items"][0];
    assert_eq!(hit["product_id"], json!(product.id));
    assert_eq!(hit["variant_id"], json!(product.variants[1].id));
    assert_eq!(hit["price"]["amount_minor"], 31_900);
    assert_eq!(hit["price"]["currency"], "CZK");
    let size = body["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "opt.size")
        .unwrap();
    assert_eq!(size["label"], "Velikost");
    assert_eq!(
        size["values"],
        json!([
            { "value": "v1", "label": "V1", "selected": false, "available": true },
            { "value": "v2", "label": "V2", "selected": true, "available": true },
        ])
    );
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=60");

    let (status, body, _) = s
        .storefront("/storefront/v1/search/suggest?q=Trič", "cs")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["categories"][0]["slug"], "tricka");
    assert_eq!(body["products"][0]["product_id"], json!(product.id));

    // The search key cannot write (A27).
    let write =
        s.s.meili
            .add_documents(
                &commerce::search::index_uid(s.tenant, "cs"),
                &[json!({ "id": "x" })],
            )
            .await;
    assert!(write.is_err());
}
