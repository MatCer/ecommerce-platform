//! WP14 through the HTTP API: the events beacon (consent-gated), edge counters (service
//! token), the dashboard, webhook administration and the superadmin job view.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use commerce::consent::new_anon_id;
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
    employee: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    testkit::staff(&runtime, shop.tenant, "owner", "owner").await;
    testkit::staff(&runtime, shop.tenant, "employee", "staff").await;
    Ctx {
        s,
        shop,
        runtime,
        owner: sign(&claims("owner")),
        employee: sign(&claims("employee")),
        _jwks: jwks,
    }
}

impl Ctx {
    async fn sf(&self, call: Call<'_>) -> (StatusCode, Value, axum::response::Response) {
        call.header("x-storefront-token", self.shop.token.clone())
            .header("x-market", self.shop.cz.to_string())
            .header("x-client-ip", "203.0.113.7")
            .send(&self.s)
            .await
    }

    async fn events(&self) -> i64 {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let n = sqlx::query_scalar("SELECT count(*) FROM events")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        n
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn beacon_events_follow_server_side_consent(db: PgPool) {
    let c = setup(db).await;
    let beacon = json!({ "events": [{ "type": "page_view", "template": "home" }] });
    let subject = new_anon_id();

    // No consent recorded yet: accepted, nothing stored.
    let (status, _, _) = c
        .sf(Call::post("/storefront/v1/events", beacon.clone())
            .header("x-consent-subject", subject.clone()))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(c.events().await, 0);

    // Grant analytics through the consent endpoint, then the same beacon is stored.
    let (status, _, _) = c
        .sf(Call::post(
            "/storefront/v1/consent",
            json!({ "purposes": { "analytics": true, "ads": false }, "text_version": "v1" }),
        )
        .header("x-consent-subject", subject.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = c
        .sf(Call::post("/storefront/v1/events", beacon.clone())
            .header("x-consent-subject", subject.clone()))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(c.events().await, 1);

    // Without the subject header nothing is stored, whatever the beacon claims.
    let claimed = json!({ "events": [{ "type": "page_view" }], "consent": ["analytics"] });
    c.sf(Call::post("/storefront/v1/events", claimed)).await;
    assert_eq!(c.events().await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn counters_need_the_service_token_and_feed_the_dashboard(db: PgPool) {
    let c = setup(db).await;
    let today = Utc::now().date_naive();
    let batch = json!({
        "counters": [{ "tenant_id": c.shop.tenant, "market_id": c.shop.cz, "day": today,
                       "template": "product", "requests": 12 }],
        "searches": [{ "tenant_id": c.shop.tenant, "day": today, "locale": "cs",
                       "query": "triko", "count": 4 }],
    });
    let (status, _, _) = Call::post("/internal/v1/analytics/counters", batch.clone())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body, _) = Call::post("/internal/v1/analytics/counters", batch)
        .header("authorization", format!("Bearer {SERVICE_TOKEN}"))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "counters": 1, "searches": 1 }));

    let uri = format!("/admin/v1/analytics/dashboard?from={today}&to={today}");
    let (status, _, _) = Call::get(&uri).send(&c.s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, d, _) = Call::get(&uri)
        .token(&c.employee)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    assert_eq!(d["traffic"]["page_requests"], 12);
    assert_eq!(
        d["top_searches"][0],
        json!({ "query": "triko", "count": 4 })
    );
    assert_eq!(d["traffic"]["funnel"][0]["step"], "sessions");
    let bad = format!("/admin/v1/analytics/dashboard?from={today}&to=2020-01-01");
    let (status, _, _) = Call::get(&bad)
        .token(&c.employee)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_admin_roles_fresh_auth_and_secret_once(db: PgPool) {
    let c = setup(db).await;
    let input = json!({ "url": "https://hooks.example.com/erp", "events": ["order.paid"] });

    let (status, _, _) = Call::post("/admin/v1/webhooks", input.clone())
        .token(&c.employee)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "staff cannot configure webhooks"
    );

    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::post("/admin/v1/webhooks", input.clone())
        .token(&sign(&stale))
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );

    let (status, created, _) = Call::post("/admin/v1/webhooks", input)
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let secret = created["secret"].as_str().unwrap();
    assert!(secret.starts_with("whsec_"));
    let id = created["subscription"]["id"].as_str().unwrap().to_owned();

    let (status, list, _) = Call::get("/admin/v1/webhooks")
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !list.to_string().contains(secret),
        "the secret is never listed"
    );
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(
        list["event_types"]
            .as_array()
            .unwrap()
            .contains(&json!("customer.created"))
    );

    let uri = format!("/admin/v1/webhooks/{id}/rotate-secret");
    let (status, rotated, _) = Call::post(&uri, json!({}))
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(rotated["secret"], created["secret"]);

    let (status, body, _) = Call::post(
        "/admin/v1/webhooks",
        json!({ "url": "http://10.0.0.5/x", "events": ["order.paid"] }),
    )
    .token(&c.owner)
    .tenant(c.shop.tenant)
    .send(&c.s)
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_url"))
    );

    let (status, page, _) = Call::get("/admin/v1/webhooks/deliveries")
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"], json!([]));

    let uri = format!("/admin/v1/webhooks/{id}");
    let (status, _, _) = Call::delete(&uri)
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn superadmins_see_and_requeue_dead_jobs(db: PgPool) {
    let c = setup(db.clone()).await;
    let mut job = platform::queue::NewJob::new("media.process", json!({ "asset_id": "x" }));
    job.tenant_id = Some(c.shop.tenant);
    let id = platform::queue::enqueue(&c.runtime, &job).await.unwrap();
    sqlx::query("UPDATE queue.jobs SET status = 'dead', last_error = 'boom', finished_at = now() WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();

    let (status, me, _) = Call::get("/admin/v1/me").token(&c.owner).send(&c.s).await;
    assert_eq!(
        (status, &me["is_superadmin"]),
        (StatusCode::OK, &json!(false))
    );
    let (status, _, _) = Call::get("/admin/v1/platform/jobs")
        .token(&c.owner)
        .send(&c.s)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "tenant owners are not superadmins"
    );

    sqlx::query("INSERT INTO platform.platform_admins (user_id) VALUES ('root')")
        .execute(&c.runtime)
        .await
        .unwrap();
    let root = sign(&claims("root"));
    let (_, me, _) = Call::get("/admin/v1/me").token(&root).send(&c.s).await;
    assert_eq!(me["is_superadmin"], true);
    let (status, page, _) = Call::get("/admin/v1/platform/jobs?status=dead")
        .token(&root)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["items"][0]["id"], id);
    assert_eq!(page["items"][0]["last_error"], "boom");

    let uri = format!("/admin/v1/platform/jobs/{id}/requeue");
    let (status, _, _) = Call::post(&uri, json!({})).token(&root).send(&c.s).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = Call::post(&uri, json!({})).token(&root).send(&c.s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("not_dead"))
    );
    let (_, page, _) = Call::get("/admin/v1/platform/jobs?status=queued&kind=media.process")
        .token(&root)
        .send(&c.s)
        .await;
    assert_eq!(page["items"][0]["attempts"], 0);
}
