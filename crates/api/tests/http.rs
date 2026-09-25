// Integration test file: panicking on unexpected errors is the desired failure mode.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use api::{AppState, BODY_LIMIT_BYTES, app};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

/// Nothing listens on port 1, so these dependencies are reliably down.
const DEAD_DB: &str = "postgres://nobody:nothing@127.0.0.1:1/none";
const DEAD_MEILI: &str = "http://127.0.0.1:1";

fn state(db: PgPool) -> AppState {
    AppState {
        db,
        auth_service: None,
        http: reqwest::Client::new(),
        meili: commerce::search::Meili::new(
            reqwest::Client::new(),
            DEAD_MEILI.parse().unwrap(),
            "unused".into(),
            Duration::from_secs(1),
        ),
        storage: testkit::memory_storage(),
        staff_auth: std::sync::Arc::new(api::auth::StaffAuth::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1/jwks".parse().unwrap(),
            "http://auth.localhost",
        )),
        internal_token: api::auth::ServiceToken::new("unused-in-these-tests"),
        admin_origin: axum::http::HeaderValue::from_static("http://admin.localhost:8080"),
        public_urls: commerce::storefront::PublicUrls::default(),
        edge: api::edge::EdgePurge::disabled(),
        checkout: Default::default(),
        ai: commerce::ai::Ai::fake(),
        webhooks: None,
        ads: None,
        rate_limit: std::sync::Arc::new(api::rate_limit::StorefrontLimiter::new(1, 1)),
    }
}

#[tokio::test]
async fn storefront_calls_are_rate_limited_per_token_and_ip() {
    let app = api::app(state(dead_db()), false);
    let call = |ip: &str| {
        Request::get("/storefront/v1/shop")
            .header("x-storefront-token", "sf_test")
            .header("x-client-ip", ip)
            .body(Body::empty())
            .unwrap()
    };
    let first = send(app.clone(), call("203.0.113.7")).await;
    assert_ne!(first.status(), StatusCode::TOO_MANY_REQUESTS);
    let second = send(app.clone(), call("203.0.113.7")).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second.headers()["content-type"], "application/problem+json");
    let retry: u64 = second.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&retry));
    assert_eq!(json(second).await["code"], "rate_limited");
    // Another client IP has its own bucket; other routes are not limited.
    let other = send(app.clone(), call("198.51.100.9")).await;
    assert_ne!(other.status(), StatusCode::TOO_MANY_REQUESTS);
    for _ in 0..3 {
        assert_eq!(get(app.clone(), "/healthz").await.status(), StatusCode::OK);
    }
}

fn dead_db() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy(DEAD_DB)
        .unwrap()
}

async fn send(app: axum::Router, req: Request<Body>) -> Response {
    app.oneshot(req).await.unwrap()
}

async fn get(app: axum::Router, uri: &str) -> Response {
    send(app, Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn json(res: Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn healthz_is_ok_without_dependencies() {
    let res = get(app(state(dead_db()), false), "/healthz").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json(res).await, serde_json::json!({ "status": "ok" }));
}

#[tokio::test]
async fn openapi_lists_health_routes() {
    let res = get(app(state(dead_db()), false), "/openapi.json").await;
    assert_eq!(res.status(), StatusCode::OK);
    let spec = json(res).await;
    assert!(spec["openapi"].as_str().unwrap().starts_with("3.1"));
    assert!(spec["paths"]["/healthz"]["get"].is_object());
    assert!(spec["paths"]["/readyz"]["get"].is_object());
    assert!(spec["components"]["schemas"]["Problem"].is_object());
}

#[tokio::test]
async fn unknown_route_is_problem_json() {
    let res = get(app(state(dead_db()), false), "/nope").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(json(res).await["code"], "not_found");
}

#[tokio::test]
async fn request_id_is_generated_and_propagated() {
    let res = get(app(state(dead_db()), false), "/healthz").await;
    let id = res.headers()["x-request-id"].to_str().unwrap();
    assert_eq!(id.len(), 36, "uuid expected, got {id}");

    let req = Request::get("/healthz")
        .header("x-request-id", "abc_123-XYZ")
        .body(Body::empty())
        .unwrap();
    let res = send(app(state(dead_db()), false), req).await;
    assert_eq!(res.headers()["x-request-id"], "abc_123-XYZ");
}

#[tokio::test]
async fn invalid_request_id_is_replaced() {
    for bad in ["has space", "semi;colon", &"a".repeat(65)] {
        let req = Request::get("/healthz")
            .header("x-request-id", bad)
            .body(Body::empty())
            .unwrap();
        let res = send(app(state(dead_db()), false), req).await;
        let id = res.headers()["x-request-id"].to_str().unwrap();
        assert_ne!(id, bad);
        assert_eq!(id.len(), 36, "uuid expected for {bad:?}, got {id}");
    }
    let ok64 = "a".repeat(64);
    let req = Request::get("/healthz")
        .header("x-request-id", &ok64)
        .body(Body::empty())
        .unwrap();
    let res = send(app(state(dead_db()), false), req).await;
    assert_eq!(res.headers()["x-request-id"], ok64.as_str());
}

async fn assert_problem(res: Response, status: StatusCode, code: &str) {
    assert_eq!(res.status(), status);
    assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let body = json(res).await;
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["code"], code);
}

#[tokio::test]
async fn oversized_body_is_problem_json() {
    let req = Request::post("/healthz")
        .header(header::CONTENT_LENGTH, BODY_LIMIT_BYTES + 1)
        .body(Body::from(vec![0u8; BODY_LIMIT_BYTES + 1]))
        .unwrap();
    let res = send(app(state(dead_db()), false), req).await;
    assert!(res.headers().contains_key("x-request-id"));
    assert_problem(res, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;
}

#[tokio::test]
async fn wrong_method_is_problem_json() {
    let req = Request::delete("/healthz").body(Body::empty()).unwrap();
    let res = send(app(state(dead_db()), false), req).await;
    assert_eq!(res.headers()[header::ALLOW], "GET,HEAD");
    assert_problem(res, StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed").await;
}

#[tokio::test]
async fn swagger_ui_only_in_dev() {
    let res = get(app(state(dead_db()), true), "/docs/").await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = get(app(state(dead_db()), true), "/openapi.json").await;
    assert_eq!(res.status(), StatusCode::OK);

    let res = get(app(state(dead_db()), false), "/docs/").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn readyz_fails_when_database_is_down() {
    let res = get(app(state(dead_db()), false), "/readyz").await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = json(res).await;
    assert_eq!(body["status"], "fail");
    assert_eq!(body["checks"]["database"], "fail");
    assert_eq!(body["checks"]["storage"], "ok");
}

/// Real Postgres (`DATABASE_URL`). Search down degrades but does not fail readiness (A27).
#[sqlx::test(migrations = "../../migrations")]
async fn readyz_degraded_with_database_if_search_is_down(db: PgPool) {
    let res = get(app(state(db), false), "/readyz").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = json(res).await;
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["checks"]["database"], "ok");
    assert_eq!(body["checks"]["meilisearch"], "fail");
}

#[sqlx::test(migrations = "../../migrations")]
async fn migrations_create_platform_schema(db: PgPool) {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.schemata WHERE schema_name = 'platform')",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(exists);
}
