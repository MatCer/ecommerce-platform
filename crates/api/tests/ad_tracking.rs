//! WP20 through the HTTP API: ad-platform settings (roles, fresh auth, write-only
//! credentials), the delivery log and the beacon/cart capture gated by `ads` consent.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
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
            .send(&self.s)
            .await
    }

    async fn admin(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (s, b, _) = call
            .token(&self.owner)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await;
        (s, b)
    }

    async fn deliveries(&self) -> Vec<(String, String)> {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let rows = sqlx::query_as("SELECT platform, event_name FROM ad_deliveries ORDER BY 1, 2")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        rows
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn settings_need_admin_fresh_auth_and_never_return_credentials(db: PgPool) {
    let c = setup(db).await;
    let meta = json!({
        "enabled": true,
        "market_ids": [c.shop.cz],
        "settings": { "pixel_id": "1234567890" },
        "credentials": { "access_token": "EAA-very-secret-token" },
    });
    let (status, _, _) = Call::get("/admin/v1/ad-platforms")
        .token(&c.employee)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "staff cannot see ad settings"
    );

    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::patch("/admin/v1/ad-platforms/meta", meta.clone())
        .token(&sign(&stale))
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    // Pausing is not sensitive.
    let (status, _, _) = Call::patch("/admin/v1/ad-platforms/meta", json!({ "paused": true }))
        .token(&sign(&stale))
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, saved) = c
        .admin(Call::patch("/admin/v1/ad-platforms/meta", meta))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["paused"], true);
    assert_eq!(saved["credentials_hint"], "oken");
    let (status, list) = c.admin(Call::get("/admin/v1/ad-platforms")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 4);
    assert!(!list.to_string().contains("very-secret"));

    let (status, body) = c
        .admin(Call::patch(
            "/admin/v1/ad-platforms/ga4",
            json!({ "enabled": true }),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("incomplete_configuration")
        )
    );
    let (status, _) = c
        .admin(Call::patch(
            "/admin/v1/ad-platforms/ga4",
            json!({ "settings": { "pixel_id": "1234567890" } }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "not a GA4 field");
    let (status, _) = c
        .admin(Call::patch("/admin/v1/ad-platforms/tiktok", json!({})))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = c
        .admin(Call::get("/admin/v1/ad-platforms/deliveries?status=bogus"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, test) = c
        .admin(Call::post("/admin/v1/ad-platforms/sklik/test", json!({})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(test["ok"], false, "not configured");
}

#[sqlx::test(migrations = "../../migrations")]
async fn beacon_and_cart_steps_are_forwarded_only_with_ads_consent(db: PgPool) {
    let c = setup(db).await;
    let (status, body) = c
        .admin(Call::patch(
            "/admin/v1/ad-platforms/ga4",
            json!({
                "enabled": true,
                "market_ids": [c.shop.cz],
                "settings": { "measurement_id": "G-TEST123" },
                "credentials": { "api_secret": "secret" },
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let beacon = json!({
        "events": [{ "type": "view_item", "template": "product", "product_id": c.shop.product }],
        "path": "/p/tee",
        "consent": ["ads"],
    });
    let (analytics_only, ads) = (new_anon_id(), new_anon_id());
    for (who, purposes) in [
        (&analytics_only, json!({ "analytics": true, "ads": false })),
        (&ads, json!({ "analytics": false, "ads": true })),
    ] {
        let (status, _, _) = c
            .sf(Call::post(
                "/storefront/v1/consent",
                json!({ "purposes": purposes, "text_version": "v1" }),
            )
            .header("x-consent-subject", who.clone()))
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = c
            .sf(Call::post("/storefront/v1/events", beacon.clone())
                .header("x-consent-subject", who.clone())
                .header("x-client-user-agent", "UA"))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    assert_eq!(c.deliveries().await, [("ga4".into(), "view_item".into())]);

    // The API's own cart step for the consented visitor.
    let (_, _, created) = c.sf(Call::post("/storefront/v1/cart", json!({}))).await;
    let cart = created.headers()["x-cart-token"]
        .to_str()
        .unwrap()
        .to_owned();
    let (status, _, _) = c
        .sf(Call::post(
            "/storefront/v1/cart/lines",
            json!({ "variant_id": c.shop.variants[0], "quantity": 2 }),
        )
        .header("x-cart-token", cart)
        .header("x-consent-subject", ads.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(c.deliveries().await.len(), 2);

    // Withdrawal through the consent endpoint cancels what is still queued.
    let (status, _, _) = c
        .sf(Call::post(
            "/storefront/v1/consent",
            json!({ "purposes": { "ads": false }, "text_version": "v1", "source": "preferences" }),
        )
        .header("x-consent-subject", ads.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, log) = c
        .admin(Call::get("/admin/v1/ad-platforms/deliveries?platform=ga4"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let items = log["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|d| d["status"] == "cancelled"), "{log}");
    assert!(
        items[0].get("subject").is_none(),
        "the log has no identifiers"
    );
}
