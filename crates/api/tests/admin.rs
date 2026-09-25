//! Admin and internal API through the router, with real Postgres (runtime role) and a local
//! JWKS server signing like the auth service (spec A8, A9, A12).
#![allow(clippy::unwrap_used)]

use std::sync::atomic::Ordering;
use std::time::Duration;

use api::app;
use api::auth::StaffAuth;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

mod common;
use common::*;

fn sk_market() -> Value {
    json!({
        "code": "sk", "name": "Slovensko", "country_codes": ["SK"], "currency": "EUR",
        "default_locale": "sk", "locales": ["sk"]
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn staff_tokens_are_validated_strictly(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let s = state(
        testkit::runtime_pool(&db, 4).await,
        &jwks,
        Duration::from_secs(30),
    );

    let (status, body, res) = Call::get("/admin/v1/me").send(&s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("missing_token"))
    );
    assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");

    let mut expired = claims("u1");
    expired["exp"] = json!(now() - 120);
    let mut wrong_aud = claims("u1");
    wrong_aud["aud"] = json!("storefront");
    let mut wrong_iss = claims("u1");
    wrong_iss["iss"] = json!("http://evil.localhost");
    let mut no_sub = claims("u1");
    no_sub.as_object_mut().unwrap().remove("sub");
    let hs256 = jsonwebtoken::encode(
        &Header {
            kid: Some("a".into()),
            ..Header::new(Algorithm::HS256)
        },
        &claims("u1"),
        &EncodingKey::from_secret(X_A.as_bytes()),
    )
    .unwrap();
    let forged = sign_with("a", KEY_B, &claims("u1"));
    for (what, token) in [
        ("garbage", "not.a.jwt".to_owned()),
        ("expired", sign(&expired)),
        ("audience", sign(&wrong_aud)),
        ("issuer", sign(&wrong_iss)),
        ("no sub", sign(&no_sub)),
        ("hs256 with the public key as secret", hs256),
        ("signed by another key", forged),
    ] {
        let (status, body, _) = Call::get("/admin/v1/me").token(&token).send(&s).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{what}");
        assert_eq!(body["code"], "invalid_token", "{what}");
    }

    let mut unverified = claims("u1");
    unverified["email_verified"] = json!(false);
    let (status, body, _) = Call::get("/admin/v1/me")
        .token(&sign(&unverified))
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("email_not_verified"))
    );

    let (status, body, _) = Call::get("/admin/v1/me")
        .token(&sign(&claims("u1")))
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "user_id": "u1", "email": "u1@example.test", "memberships": [], "is_superadmin": false })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenant_access_requires_membership_and_role(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let (b, _) = testkit::tenant(&runtime, "beta").await;
    testkit::staff(&runtime, a, "owner-a", "owner").await;
    testkit::staff(&runtime, a, "clerk-a", "staff").await;
    testkit::staff(&runtime, b, "owner-b", "owner").await;
    let s = state(runtime, &jwks, Duration::from_secs(30));
    let owner_a = sign(&claims("owner-a"));
    let clerk_a = sign(&claims("clerk-a"));

    let (status, body, _) = Call::get("/admin/v1/me").token(&owner_a).send(&s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memberships"][0]["slug"], "alpha");
    assert_eq!(body["memberships"][0]["role"], "owner");

    let (status, body, _) = Call::get("/admin/v1/markets")
        .token(&owner_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["code"], "cz");

    // Tenant B: not a member.
    for call in [
        Call::get("/admin/v1/markets"),
        Call::post("/admin/v1/markets", sk_market()),
        Call::get("/admin/v1/audit-log"),
    ] {
        let (status, body, _) = call.token(&owner_a).tenant(b).send(&s).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("not_a_member"))
        );
    }
    let (status, body, _) = Call::get("/admin/v1/markets")
        .token(&owner_a)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("tenant_required"))
    );

    // Staff role: may read markets, may not change settings or read the audit log.
    let (status, _, _) = Call::get("/admin/v1/markets")
        .token(&clerk_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = Call::post("/admin/v1/markets", sk_market())
        .token(&clerk_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("insufficient_role"))
    );

    let (status, market, _) = Call::post("/admin/v1/markets", sk_market())
        .token(&owner_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(market["currency"], "EUR");

    // Tax settings need a recent login (A9): a token from a 16-minute-old session is refused.
    let mut stale = claims("owner-a");
    stale["auth_time"] = json!(now() - 16 * 60);
    let mut hu = sk_market();
    hu["code"] = json!("hu");
    let (status, body, res) = Call::post("/admin/v1/markets", hu)
        .token(&sign(&stale))
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");

    let (status, log, _) = Call::get("/admin/v1/audit-log?limit=10")
        .token(&owner_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(log["items"][0]["action"], "market.created");
    assert_eq!(log["items"][0]["actor"], "owner-a");
    assert_eq!(log["items"][0]["entity_id"], market["id"]);

    // Tenant B never sees tenant A's market.
    let (_, body, _) = Call::get("/admin/v1/markets")
        .token(&sign(&claims("owner-b")))
        .tenant(b)
        .send(&s)
        .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    let (status, body, _) = Call::post("/admin/v1/markets", json!({ "code": "x" }))
        .token(&owner_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_body"))
    );
    let (status, body, _) = Call::get("/admin/v1/audit-log?limit=abc")
        .token(&owner_a)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_query"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn idempotency_key_replays_and_rejects_reuse(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    testkit::staff(&runtime, a, "owner-a", "owner").await;
    let s = state(runtime, &jwks, Duration::from_secs(30));
    let token = sign(&claims("owner-a"));

    let call = || {
        Call::post("/admin/v1/markets", sk_market())
            .token(&token)
            .tenant(a)
            .key("create-sk-1")
    };
    let (status, first, res) = call().send(&s).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(res.headers().get("idempotent-replayed").is_none());
    let (status, second, res) = call().send(&s).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(res.headers()["idempotent-replayed"], "true");
    assert_eq!(first, second);

    let mut other = sk_market();
    other["name"] = json!("Slovakia");
    let (status, body, _) = Call::post("/admin/v1/markets", other)
        .token(&token)
        .tenant(a)
        .key("create-sk-1")
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_conflict"))
    );

    let (status, body, _) = Call::post("/admin/v1/markets", sk_market())
        .token(&token)
        .tenant(a)
        .key("bad key")
        .send(&s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_idempotency_key"))
    );

    // Only one market and one audit entry were written.
    let (_, log, _) = Call::get("/admin/v1/audit-log")
        .token(&token)
        .tenant(a)
        .send(&s)
        .await;
    assert_eq!(log["items"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn jwks_refreshes_on_unknown_kid_at_most_once_per_interval(db: PgPool) {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let s = state(
        testkit::runtime_pool(&db, 2).await,
        &jwks,
        // Wide enough that five DB-backed requests fit inside it on a slow CI runner.
        Duration::from_millis(2000),
    );

    let ok = |status: StatusCode| assert_eq!(status, StatusCode::OK);
    ok(Call::get("/admin/v1/me")
        .token(&sign(&claims("u")))
        .send(&s)
        .await
        .0);
    ok(Call::get("/admin/v1/me")
        .token(&sign(&claims("u")))
        .send(&s)
        .await
        .0);
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 1, "cached");

    // Key rotation: a token with a new kid forces one refresh (once the minimum interval
    // since the last fetch has passed).
    tokio::time::sleep(Duration::from_millis(2100)).await;
    *jwks.keys.write().await = json!([jwk("a", X_A), jwk("b", X_B)]);
    let rotated = sign_with("b", KEY_B, &claims("u"));
    ok(Call::get("/admin/v1/me").token(&rotated).send(&s).await.0);
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 2);

    // Unknown kids: refused from cache while the last forced refresh is recent, then one
    // refresh per interval.
    let unknown = sign_with("zzz", KEY_B, &claims("u"));
    let hammer = || async {
        for _ in 0..5 {
            let (status, _, _) = Call::get("/admin/v1/me").token(&unknown).send(&s).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    };
    hammer().await;
    assert_eq!(
        jwks.hits.load(Ordering::SeqCst),
        2,
        "rotation refresh was just now"
    );
    tokio::time::sleep(Duration::from_millis(2100)).await;
    hammer().await;
    assert_eq!(
        jwks.hits.load(Ordering::SeqCst),
        3,
        "one refresh per interval"
    );
}

#[tokio::test]
async fn jwks_outage_is_throttled_and_known_keys_keep_working() {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let interval = Duration::from_millis(200);
    let auth = StaffAuth::with_timing(
        reqwest::Client::new(),
        jwks.url.parse().unwrap(),
        ISSUER,
        interval,
        interval,
    );
    let good = sign(&claims("u"));
    let unknown = sign_with("zzz", KEY_B, &claims("u"));
    auth.verify(&good).await.unwrap();
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 1);

    // Outage: the stale key set is refreshed once (failing) and keeps verifying known keys;
    // unknown kids get 503 without further fetches until the interval has passed.
    *jwks.keys.write().await = Value::Null;
    tokio::time::sleep(Duration::from_millis(250)).await;
    auth.verify(&good).await.unwrap();
    for _ in 0..5 {
        assert_eq!(
            auth.verify(&unknown).await.unwrap_err().code(),
            "service_unavailable"
        );
        auth.verify(&good).await.unwrap();
    }
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 2);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(auth.verify(&unknown).await.is_err());
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 3);

    // Starting during an outage: one attempt, then throttled.
    let cold = StaffAuth::with_timing(
        reqwest::Client::new(),
        jwks.url.parse().unwrap(),
        ISSUER,
        interval,
        interval,
    );
    for _ in 0..3 {
        assert_eq!(
            cold.verify(&good).await.unwrap_err().code(),
            "service_unavailable"
        );
    }
    assert_eq!(jwks.hits.load(Ordering::SeqCst), 4);
}

#[sqlx::test(migrations = "../../migrations")]
async fn cors_allows_only_the_admin_origin(db: PgPool) {
    let jwks = jwks_server(json!([])).await;
    let s = state(
        testkit::runtime_pool(&db, 1).await,
        &jwks,
        Duration::from_secs(30),
    );
    let preflight = |origin: &'static str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/admin/v1/markets")
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,x-tenant-id,idempotency-key",
            )
            .body(Body::empty())
            .unwrap()
    };
    let res = app(s.clone(), false)
        .oneshot(preflight(ADMIN_ORIGIN))
        .await
        .unwrap();
    assert_eq!(
        res.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        ADMIN_ORIGIN
    );
    let allowed = res.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        allowed.contains("x-tenant-id") && allowed.contains("idempotency-key"),
        "{allowed}"
    );
    let res = app(s, false)
        .oneshot(preflight("http://evil.localhost"))
        .await
        .unwrap();
    assert!(
        res.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn internal_resolve_needs_the_service_token(db: PgPool) {
    let jwks = jwks_server(json!([])).await;
    let runtime = testkit::runtime_pool(&db, 2).await;
    let created = commerce::tenancy::create_tenant(
        &runtime,
        &commerce::tenancy::NewTenant {
            slug: "demo",
            name: "Demo",
            owner_user_id: "u",
            owner_email: "u@example.test",
        },
    )
    .await
    .unwrap();
    let s = state(runtime, &jwks, Duration::from_secs(30));

    let uri = "/internal/v1/resolve?host=demo.localhost:8180";
    let (status, _, _) = Call::get(uri).send(&s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = Call::get(uri).token("wrong-token").send(&s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A staff JWT is not a service token.
    let (status, _, _) = Call::get(uri).token(&sign(&claims("u"))).send(&s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body, _) = Call::get(uri).token(SERVICE_TOKEN).send(&s).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tenant_id"], created.tenant_id.to_string());
    assert_eq!(body["market_code"], "cz");
    assert_eq!(body["hostname"], "demo.localhost");

    let (status, _, _) = Call::get("/internal/v1/resolve?host=unknown.localhost")
        .token(SERVICE_TOKEN)
        .send(&s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
