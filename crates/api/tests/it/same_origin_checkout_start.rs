use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use commerce::checkout_access;
use commerce::storefront::{self, PublicUrls};
use commerce::tenancy::CheckoutMode;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

use crate::common::*;

mod pii_rotation;
mod placement_recovery;

#[derive(Clone)]
struct Edge {
    token: String,
    market: Uuid,
    origin: String,
}

impl Edge {
    fn call<'a>(&self, call: Call<'a>) -> Call<'a> {
        call.header("x-storefront-token", self.token.clone())
            .header("x-market", self.market.to_string())
            .header("x-shop-origin", self.origin.clone())
    }
}

struct Ctx {
    s: api::AppState,
    shop: Shop,
    edge: Edge,
    db: PgPool,
    runtime: PgPool,
    _jwks: Jwks,
}

async fn setup(db: PgPool, mode: CheckoutMode) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let c = Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        edge: Edge {
            token: shop.token.clone(),
            market: shop.cz,
            origin: shop.origin.as_str().to_owned(),
        },
        shop,
        db,
        runtime,
        _jwks: jwks,
    };
    if mode == CheckoutMode::SameOrigin {
        c.switch(mode).await;
    }
    c
}

async fn start_call(
    s: &api::AppState,
    edge: &Edge,
    shop_token: &str,
    previous: Option<&str>,
) -> (StatusCode, Value) {
    let body = previous.map_or(json!({}), |p| json!({ "previous": p }));
    let (status, body, _) = edge
        .call(Call::post("/storefront/v1/cart/checkout", body))
        .header("x-cart-token", shop_token)
        .send(s)
        .await;
    (status, body)
}

async fn waiting_on(db: &PgPool, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))",
            )
            .bind(blocker)
            .fetch_one(db)
            .await
            .unwrap();
            if waiting {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("nothing waited on backend {blocker}"));
}

impl Ctx {
    async fn switch(&self, mode: CheckoutMode) {
        sqlx::query(
            "UPDATE platform.checkout_origins SET history = 'merchant_attested_never_served',
                 attested_by = 'user-owner', attested_at = now(), attestation_version = 1
             WHERE origin = $1",
        )
        .bind(self.shop.origin.as_str())
        .execute(&self.runtime)
        .await
        .unwrap();
        let changed =
            checkout_access::switch_mode(&self.runtime, self.shop.tenant, &self.shop.origin, mode)
                .await
                .unwrap();
        assert!(changed, "switched to {mode:?}");
    }

    async fn call(&self, call: Call<'_>, cart: &str) -> (StatusCode, Value) {
        let (status, body, _) = self
            .edge
            .call(call)
            .header("x-cart-token", cart.to_owned())
            .send(&self.s)
            .await;
        (status, body)
    }

    async fn shop_cart(&self) -> String {
        let (status, _, res) = self
            .edge
            .call(Call::post("/storefront/v1/cart", json!({})))
            .send(&self.s)
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
        self.add(&token, 0).await;
        token
    }

    async fn add(&self, shop_token: &str, variant: usize) -> Value {
        let (status, body) = self
            .call(
                Call::post(
                    "/storefront/v1/cart/lines",
                    json!({ "variant_id": self.shop.variants[variant] }),
                ),
                shop_token,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn start(&self, shop_token: &str, previous: Option<&str>) -> (StatusCode, Value) {
        start_call(&self.s, &self.edge, shop_token, previous).await
    }

    async fn started(&self, shop_token: &str, previous: Option<&str>) -> String {
        let (status, body) = self.start(shop_token, previous).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["cart_token"].as_str().unwrap().to_owned()
    }

    async fn checkout(&self, cart: &str) -> (StatusCode, Value) {
        self.call(Call::get("/storefront/v1/checkout"), cart).await
    }

    async fn assert_restart(&self, cart: &str, typed: &str, what: &str) {
        let (status, text, _) = self
            .edge
            .call(Call::get("/storefront/v1/checkout"))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{what}: {text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["code"], "not_found", "{what}");
        for key in ["cart", "email", "totals", "missing"] {
            assert!(body.get(key).is_none(), "{what}: {key} in {text}");
        }
        assert!(!text.contains(typed), "{what}: {text}");
    }

    async fn details(&self, cart: &str, body: Value) -> (StatusCode, String, Option<String>) {
        let (status, text, res) = self
            .edge
            .call(Call::post("/storefront/v1/checkout/details", body))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        (status, text, replacement(&res))
    }

    async fn rotated(&self, cart: &str, body: Value) -> String {
        let (status, text, k) = self.details(cart, body).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{text}");
        assert_eq!(text, "", "the replacing write answers no body");
        let k = k.expect("the write replaced the capability");
        assert!(
            commerce::capability::well_formed(&k) && k != cart,
            "a new capability"
        );
        k
    }
}

fn replacement(res: &axum::response::Response) -> Option<String> {
    res.headers()
        .get("x-cart-token")
        .map(|v| v.to_str().unwrap().to_owned())
}

fn code(text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap_or_default()["code"].clone()
}

fn line_count(checkout: &Value) -> usize {
    checkout["cart"]["lines"].as_array().unwrap().len()
}

#[sqlx::test(migrations = "../../migrations")]
async fn start_last_commit_wins(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let shop_token = c.shop_cart().await;

    let mut first = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let mut ctx = storefront::context(
        &mut first,
        &PublicUrls::default(),
        c.shop.cz,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    ctx.origin = Some(c.shop.origin.clone());
    let k1 = checkout_access::start(&mut first, &ctx, &shop_token, None)
        .await
        .unwrap();
    let k1 = commerce::checkout::set_same_origin_details(
        &mut first,
        &ctx,
        &k1,
        &commerce::checkout::Details::Contact(commerce::checkout::ContactInput {
            email: "first@example.cz".into(),
            phone: None,
        }),
    )
    .await
    .unwrap();
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .unwrap();
    let last = tokio::spawn({
        let (s, edge, shop_token) = (c.s.clone(), c.edge.clone(), shop_token.clone());
        async move { start_call(&s, &edge, &shop_token, None).await }
    });
    waiting_on(&c.db, first_pid).await;
    first.commit().await.unwrap();
    let (status, body) = last.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let k2 = body["cart_token"].as_str().unwrap().to_owned();

    let (status, checkout) = c.checkout(&k2).await;
    assert_eq!(status, StatusCode::OK, "{checkout}");
    assert_eq!(line_count(&checkout), 1);
    assert_eq!(checkout["email"], Value::Null, "K2 is a fresh snapshot");
    c.assert_restart(&k1, "first@example.cz", "the earlier commit lost")
        .await;

    let k3 = c.started(&shop_token, Some(&k1)).await;
    let (status, checkout) = c.checkout(&k3).await;
    assert_eq!(status, StatusCode::OK, "{checkout}");
    assert_eq!(line_count(&checkout), 1);
    c.assert_restart(&k2, "first@example.cz", "superseded by the delayed start")
        .await;
    c.assert_restart(
        &k1,
        "first@example.cz",
        "a revoked capability is never revived",
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn old_shop_cap_cannot_read_snapshot(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let shop_token = c.shop_cart().await;
    let k = c.started(&shop_token, None).await;
    let k = c
        .rotated(&k, json!({ "contact": { "email": "typed@example.cz" } }))
        .await;
    let (status, before) = c.checkout(&k).await;
    assert_eq!(status, StatusCode::OK, "{before}");

    c.assert_restart(
        &shop_token,
        "typed@example.cz",
        "GET /checkout with the shop capability",
    )
    .await;
    let (status, text, replaced) = c
        .details(
            &shop_token,
            json!({ "contact": { "email": "attacker@example.cz" } }),
        )
        .await;
    assert_eq!(
        (status, replaced),
        (StatusCode::NOT_FOUND, None),
        "contact write: {text}"
    );
    let (status, _) = c
        .call(
            Call::post(
                "/storefront/v1/checkout/place-order",
                json!({ "version": before["cart"]["version"],
                        "total_minor": before["totals"]["total"]["amount_minor"],
                        "accept_terms": true, "accept_withdrawal": true }),
            )
            .key("place-1"),
            &shop_token,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "place-order");

    let (status, cart) = c.call(Call::get("/storefront/v1/cart"), &shop_token).await;
    assert_eq!(status, StatusCode::OK, "{cart}");
    assert_ne!(
        cart["id"], before["cart"]["id"],
        "the shop cart, not the snapshot"
    );
    assert!(!cart.to_string().contains("typed@example.cz"));

    let added = c.add(&shop_token, 1).await;
    assert_eq!(added["lines"].as_array().unwrap().len(), 2);
    let (status, after) = c.checkout(&k).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["cart"]["lines"], before["cart"]["lines"]);
    assert_eq!(after["cart"]["version"], before["cart"]["version"]);
    assert_eq!(after["email"], "typed@example.cz");
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_origin_handoff_rejected(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let handed = c.shop_cart().await;
    let (status, body) = c
        .call(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            &handed,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let handoff = body["token"].as_str().unwrap().to_owned();
    let waiting = c.shop_cart().await;
    c.switch(CheckoutMode::SameOrigin).await;

    let (status, body, _) = c
        .edge
        .call(Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": handoff }),
        ))
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_handoff"))
    );
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let unspent: bool =
        sqlx::query_scalar("SELECT used_at IS NULL FROM checkout_handoffs WHERE token_hash = $1")
            .bind(commerce::capability::hash(&handoff))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert!(unspent, "the refused redemption spent nothing");

    let (status, body) = c
        .call(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            &waiting,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_mode_changed"))
    );
    let (status, cart) = c.call(Call::get("/storefront/v1/cart"), &waiting).await;
    assert_eq!(
        (status, cart["item_count"].as_i64()),
        (StatusCode::OK, Some(1)),
        "the shop cart is intact"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn isolated_start_still_handoffs(db: PgPool) {
    let c = setup(db, CheckoutMode::Isolated).await;
    let shop_token = c.shop_cart().await;
    let (status, before) = c.call(Call::get("/storefront/v1/cart"), &shop_token).await;
    assert_eq!(status, StatusCode::OK, "{before}");

    let (status, body) = c.start(&shop_token, None).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("checkout_mode_changed"))
    );
    let (status, cart) = c.call(Call::get("/storefront/v1/cart"), &shop_token).await;
    assert_eq!(status, StatusCode::OK, "{cart}");
    assert_eq!(cart, before, "the refused start changed nothing");

    let (status, body) = c
        .call(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            &shop_token,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body, _) = c
        .edge
        .call(Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": body["token"] }),
        ))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let k = body["cart_token"].as_str().unwrap().to_owned();
    let (status, text, replaced) = c
        .details(&k, json!({ "contact": { "email": "jana@example.cz" } }))
        .await;
    assert_eq!(
        (status, code(&text)),
        (StatusCode::CONFLICT, json!("checkout_mode_changed"))
    );
    assert_eq!(replaced, None, "an isolated capability is never replaced");
    let (status, checkout) = c.checkout(&k).await;
    assert_eq!(status, StatusCode::OK, "{checkout}");
    assert_eq!(checkout["cart"]["id"], before["id"], "checked out in place");
    assert_eq!(line_count(&checkout), 1);
    assert_eq!(
        checkout["email"],
        Value::Null,
        "the refused write wrote nothing"
    );
}
