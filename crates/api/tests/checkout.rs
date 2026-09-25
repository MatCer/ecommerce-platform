//! Checkout, orders, the fake gateway and the checkout admin through the HTTP API (as the
//! edge calls it): place-order idempotency, the order capability, payment failure → retry →
//! success, signed fake events, customer orders, admin roles and fresh-auth rules.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use commerce::payments::{FakeEvent, FakeGateway, MethodKind, Outcome, PaymentMethodInput};
use commerce::shipping::{Carrier, ShippingMethodInput};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    other: Shop,
    runtime: PgPool,
    pickup: Uuid,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let mut tx = platform::db::tenant_tx(&runtime, shop.tenant)
        .await
        .unwrap();
    let pickup = commerce::shipping::create(
        &mut tx,
        "t",
        &ShippingMethodInput {
            market_id: shop.cz,
            carrier: Carrier::PacketaPickup,
            name_i18n: [("cs".to_owned(), "Zásilkovna".to_owned())].into(),
            description_i18n: Default::default(),
            price_minor: 7900,
            free_over_minor: Some(150_000),
            weight_tiers: vec![],
            cod_allowed: true,
            cod_fee_minor: 3900,
            active: true,
            position: 0,
        },
    )
    .await
    .unwrap()
    .id;
    commerce::payments::configure(
        &mut tx,
        "t",
        &s.checkout.payments,
        shop.cz,
        MethodKind::Fake,
        &PaymentMethodInput {
            enabled: true,
            name_i18n: Default::default(),
            timeout_minutes: None,
            position: 0,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    testkit::staff(&runtime, shop.tenant, "owner", "owner").await;
    testkit::staff(&runtime, shop.tenant, "employee", "staff").await;
    Ctx {
        s,
        shop,
        other,
        runtime,
        pickup,
        _jwks: jwks,
    }
}

fn sf<'a>(call: Call<'a>, shop: &Shop) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
        .header("x-client-ip", "203.0.113.7")
}

impl Ctx {
    async fn call(&self, call: Call<'_>) -> (StatusCode, Value, axum::response::Response) {
        sf(call, &self.shop).send(&self.s).await
    }

    /// A cart with one unit, handed off to the checkout origin; returns the checkout capability.
    async fn checkout_cart(&self) -> String {
        let (status, _, res) = self
            .call(Call::post("/storefront/v1/cart", json!({})))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let shop_token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
        let (status, body, _) = self
            .call(
                Call::post(
                    "/storefront/v1/cart/lines",
                    json!({"variant_id": self.shop.variants[0], "quantity": 1}),
                )
                .header("x-cart-token", shop_token.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body, _) = self
            .call(
                Call::post("/storefront/v1/cart/handoff", json!({}))
                    .header("x-cart-token", shop_token),
            )
            .await;
        let (_, body, _) = self
            .call(Call::post(
                "/storefront/v1/checkout/handoff",
                json!({"token": body["token"]}),
            ))
            .await;
        body["cart_token"].as_str().unwrap().to_owned()
    }

    async fn checkout(
        &self,
        cart: &str,
        call: Call<'_>,
    ) -> (StatusCode, Value, axum::response::Response) {
        self.call(call.header("x-cart-token", cart.to_owned()))
            .await
    }

    /// Fills every checkout step; returns the view.
    async fn fill(&self, cart: &str) -> Value {
        let steps = [
            Call::put(
                "/storefront/v1/checkout/contact",
                json!({"email": "jana@example.test", "phone": "+420 777 123 456"}),
            ),
            Call::put(
                "/storefront/v1/checkout/addresses",
                json!({"billing": {"name": "Jana Nováková", "street": "Dlouhá 12", "city": "Praha",
                                   "postal_code": "110 00", "country": "CZ"}}),
            ),
            Call::put(
                "/storefront/v1/checkout/shipping",
                json!({"method_id": self.pickup, "pickup_point": {"id": "4321", "name": "Z-BOX",
                       "street": "Dlouhá 1", "city": "Praha", "zip": "110 00", "country": "cz"}}),
            ),
            Call::put("/storefront/v1/checkout/payment", json!({"method": "fake"})),
        ];
        let mut view = Value::Null;
        for step in steps {
            let (status, body, _) = self.checkout(cart, step).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            view = body;
        }
        view
    }

    fn place_body(view: &Value) -> Value {
        json!({
            "version": view["cart"]["version"],
            "total_minor": view["totals"]["total"]["amount_minor"],
            "accept_terms": true,
            "accept_withdrawal": true,
        })
    }

    async fn order(&self, token: &str) -> Value {
        let (status, body, _) = self
            .call(Call::get(&format!("/storefront/v1/orders/{token}")))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn fake_pay(&self, attempt: &str, outcome: &str) -> (StatusCode, Value) {
        let (s, b, _) = self
            .call(Call::post(
                &format!("/storefront/v1/checkout/fake-pay/{attempt}"),
                json!({"outcome": outcome}),
            ))
            .await;
        (s, b)
    }
}

fn token_of(url: &Value) -> String {
    url.as_str().unwrap().trim_start_matches("/o/").to_owned()
}

#[sqlx::test(migrations = "../../migrations")]
async fn guest_checkout_pays_after_a_failed_attempt(db: PgPool) {
    let c = setup(db).await;
    let cart = c.checkout_cart().await;

    let (status, view, res) = c
        .checkout(&cart, Call::get("/storefront/v1/checkout"))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(res.headers()["cache-control"], "no-store");
    assert_eq!(
        view["missing"],
        json!([
            "email",
            "billing_address",
            "shipping_method",
            "payment_method"
        ])
    );
    assert_eq!(view["ship_to_countries"], json!(["CZ"]));
    assert_eq!(view["shipping_methods"][0]["price"]["amount_minor"], 7900);
    assert_eq!(
        view["payment_methods"],
        json!([{"kind": "fake", "name": "Testovací platba",
                "fee": view["payment_methods"][0]["fee"], "selectable": true}])
    );
    // The shop capability is not a checkout capability.
    let (status, _, _) = c.call(Call::get("/storefront/v1/checkout")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let view = c.fill(&cart).await;
    assert_eq!(view["missing"], json!([]));
    assert_eq!(view["pickup_point"]["country"], "CZ");
    assert_eq!(view["totals"]["total"]["amount_minor"], 12_900 + 7900);
    let body = Ctx::place_body(&view);

    let (status, problem, _) = c
        .checkout(
            &cart,
            Call::post("/storefront/v1/checkout/place-order", body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["code"], "idempotency_key_required");

    let (status, placed, res) = c
        .checkout(
            &cart,
            Call::post("/storefront/v1/checkout/place-order", body.clone()).key("k-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{placed}");
    assert!(res.headers().get("idempotent-replayed").is_none());
    let attempt = placed["payment"]["attempt_id"].as_str().unwrap().to_owned();
    let token = token_of(&placed["confirmation_url"]);
    assert_eq!(
        placed["payment"]["action"],
        json!({"type": "redirect", "url": format!("/_p/fake-pay/{attempt}?return=/o/{token}")})
    );

    // A lost response: the retry answers the same order with a fresh token.
    let (status, again, res) = c
        .checkout(
            &cart,
            Call::post("/storefront/v1/checkout/place-order", body.clone()).key("k-1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(res.headers()["idempotent-replayed"], "true");
    assert_eq!(again["order_id"], placed["order_id"]);
    assert_ne!(again["confirmation_url"], placed["confirmation_url"]);
    // Another tab: one order per cart.
    let (status, problem, _) = c
        .checkout(
            &cart,
            Call::post("/storefront/v1/checkout/place-order", body).key("k-2"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["code"], "order_already_placed");

    let order = c.order(&token).await;
    assert_eq!(order["status"], "pending");
    assert_eq!(order["payment"]["status"], "unpaid");
    assert_eq!(order["shipping"]["pickup_point"]["id"], "4321");

    // The fake provider page: fail, then retry with a new attempt and succeed.
    let (status, page) = c.fake_pay(&attempt, "failed").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["status"], "failed");
    let (status, payment, _) = c
        .call(Call::get(&format!("/storefront/v1/orders/{token}/payment")))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(payment["status"], "failed");
    assert_eq!(payment["can_retry"], true);
    let (status, retry, _) = c
        .call(Call::post(
            &format!("/storefront/v1/orders/{token}/payment-attempts"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{retry}");
    let second = retry["attempt_id"].as_str().unwrap().to_owned();
    assert_ne!(second, attempt);
    // Init is idempotent and bound to the order.
    let (status, init, _) = c
        .call(Call::post(
            &format!("/storefront/v1/orders/{token}/payment-attempts/{second}/init"),
            json!({}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(init["action"], retry["action"]);
    let (status, _, _) = c
        .call(Call::post(
            &format!("/storefront/v1/orders/{token}/payment-attempts/{attempt}/init"),
            json!({}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the first attempt is finished"
    );

    let (status, page) = c.fake_pay(&second, "succeeded").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["status"], "succeeded");
    let order = c.order(&token).await;
    assert_eq!(order["status"], "confirmed");
    assert_eq!(order["payment"]["status"], "paid");
    assert_eq!(order["payment"]["can_retry"], false);

    // The order capability works only in its own shop.
    let (status, _, _) = sf(
        Call::get(&format!("/storefront/v1/orders/{token}")),
        &c.other,
    )
    .header("x-market", c.other.cz.to_string())
    .send(&c.s)
    .await;
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN);
    let (status, _, _) = c
        .call(Call::get(&format!(
            "/storefront/v1/orders/{}",
            "0".repeat(64)
        )))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn fake_events_must_be_signed_and_match_the_attempt(db: PgPool) {
    let c = setup(db).await;
    let cart = c.checkout_cart().await;
    let view = c.fill(&cart).await;
    let (_, placed, _) = c
        .checkout(
            &cart,
            Call::post(
                "/storefront/v1/checkout/place-order",
                Ctx::place_body(&view),
            )
            .key("k"),
        )
        .await;
    let attempt: Uuid = placed["payment"]["attempt_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let event = |amount: i64| FakeEvent {
        id: Uuid::now_v7(),
        tenant_id: c.shop.tenant,
        attempt_id: attempt,
        outcome: Outcome::Succeeded,
        amount_minor: amount,
        currency: "CZK".into(),
    };
    let gateway = FakeGateway::new(FAKE_SECRET.as_bytes().to_vec());
    let post = async |e: &FakeEvent, signature: String| {
        Call::post("/webhooks/fake", serde_json::to_value(e).unwrap())
            .header("x-fake-signature", signature)
            .send(&c.s)
            .await
    };
    let good = event(12_900 + 7900);
    // `Call` sends the JSON value re-serialized; sign exactly those bytes.
    let bytes = |e: &FakeEvent| serde_json::to_value(e).unwrap().to_string().into_bytes();
    let raw = bytes(&good);
    let wrong_key = FakeGateway::new(b"not-the-secret".to_vec())
        .sign(&raw, Utc::now())
        .unwrap();
    let (status, body, _) = post(&good, wrong_key).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "invalid_signature");
    let stale = gateway
        .sign(&raw, Utc::now() - chrono::Duration::minutes(10))
        .unwrap();
    assert_eq!(post(&good, stale).await.0, StatusCode::UNAUTHORIZED);
    let cheap = event(1);
    let raw_cheap = bytes(&cheap);
    let (status, body, _) = post(&cheap, gateway.sign(&raw_cheap, Utc::now()).unwrap()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "event_mismatch");
    let signature = gateway.sign(&raw, Utc::now()).unwrap();
    assert_eq!(
        post(&good, signature.clone()).await.0,
        StatusCode::NO_CONTENT
    );
    // A repeated delivery is a no-op.
    assert_eq!(post(&good, signature).await.0, StatusCode::NO_CONTENT);
    let order = c.order(&token_of(&placed["confirmation_url"])).await;
    assert_eq!(order["payment"]["status"], "paid");

    // Without PAYMENTS_FAKE the gateway does not exist.
    let off = api::AppState {
        checkout: Default::default(),
        ..c.s.clone()
    };
    let (status, _, _) = sf(
        Call::get(&format!("/storefront/v1/checkout/fake-pay/{attempt}")),
        &c.shop,
    )
    .send(&off)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_in_customers_see_their_orders_only(db: PgPool) {
    let c = setup(db).await;
    let session = |email: &'static str| {
        let c = &c;
        async move {
            let (status, _, _) = c
                .call(Call::post(
                    "/storefront/v1/customer/magic-link",
                    json!({"email": email}),
                ))
                .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
                .await
                .unwrap();
            let text: String = sqlx::query_scalar(
                "SELECT body_text FROM email_messages WHERE template = 'magic_link' AND to_email = $1
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(email)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            let start = text.find("token=").unwrap() + "token=".len();
            let (_, _, res) = c
                .call(Call::post(
                    "/storefront/v1/customer/magic-link/consume",
                    json!({"token": &text[start..start + 64]}),
                ))
                .await;
            res.headers()["x-session-token"]
                .to_str()
                .unwrap()
                .to_owned()
        }
    };
    let jana = session("jana@example.test").await;
    let petr = session("petr@example.test").await;
    let cart = c.checkout_cart().await;
    let view = c.fill(&cart).await;
    let (status, placed, _) = c
        .checkout(
            &cart,
            Call::post(
                "/storefront/v1/checkout/place-order",
                Ctx::place_body(&view),
            )
            .key("k")
            .header("x-customer-session", jana.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{placed}");
    let id = placed["order_id"].as_str().unwrap();

    let (status, list, _) = c
        .call(
            Call::get("/storefront/v1/customer/orders").header("x-customer-session", jana.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"][0]["id"], id);
    let (status, _, _) = c
        .call(
            Call::get(&format!("/storefront/v1/customer/orders/{id}"))
                .header("x-customer-session", jana),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, list, _) = c
        .call(
            Call::get("/storefront/v1/customer/orders").header("x-customer-session", petr.clone()),
        )
        .await;
    assert_eq!(
        (status, list["items"].as_array().unwrap().len()),
        (StatusCode::OK, 0)
    );
    let (status, _, _) = c
        .call(
            Call::get(&format!("/storefront/v1/customer/orders/{id}"))
                .header("x-customer-session", petr),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = c.call(Call::get("/storefront/v1/customer/orders")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_manages_methods_and_reads_orders(db: PgPool) {
    let c = setup(db).await;
    let owner = sign(&claims("owner"));
    let employee = sign(&claims("employee"));
    let tenant = c.shop.tenant;
    let input = json!({
        "market_id": c.shop.cz, "carrier": "ppl", "name_i18n": {"cs": "PPL"},
        "price_minor": 9900, "weight_tiers": [{"up_to_g": 5000, "price_minor": 9900},
                                              {"up_to_g": 30000, "price_minor": 14900}]
    });
    let (status, _, _) = Call::post("/admin/v1/shipping-methods", input.clone())
        .tenant(tenant)
        .token(&employee)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created, _) = Call::post("/admin/v1/shipping-methods", input.clone())
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, list, _) = Call::get(&format!(
        "/admin/v1/shipping-methods?market_id={}",
        c.shop.cz
    ))
    .tenant(tenant)
    .token(&employee)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let mut update = input.clone();
    update["cod_allowed"] = json!(true);
    update["cod_fee_minor"] = json!(4900);
    let (status, updated, _) = Call::put(&format!("/admin/v1/shipping-methods/{id}"), update)
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["cod_fee_minor"], 4900);
    let mut bad = input.clone();
    bad["weight_tiers"] = json!([{"up_to_g": 0, "price_minor": 1}]);
    let (status, _, _) = Call::post("/admin/v1/shipping-methods", bad)
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = Call::delete(&format!("/admin/v1/shipping-methods/{id}"))
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Payment settings: admin role and a fresh login (A9).
    let path = format!("/admin/v1/markets/{}/payment-methods", c.shop.cz);
    let (status, methods, _) = Call::get(&path)
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<(&str, bool, bool)> = methods["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["kind"].as_str().unwrap(),
                m["enabled"].as_bool().unwrap(),
                m["available"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(kinds.contains(&("fake", true, true)));
    assert!(kinds.contains(&("stripe", false, false)));
    let cod = json!({"enabled": true});
    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body, _) = Call::put(&format!("{path}/cod"), cod.clone())
        .tenant(tenant)
        .token(&sign(&stale))
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "reauth_required");
    let (status, _, _) = Call::put(&format!("{path}/cod"), cod.clone())
        .tenant(tenant)
        .token(&employee)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body, _) = Call::put(&format!("{path}/cod"), cod)
        .tenant(tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);

    // Orders: list and detail with the timeline.
    let cart = c.checkout_cart().await;
    let view = c.fill(&cart).await;
    let (_, placed, _) = c
        .checkout(
            &cart,
            Call::post(
                "/storefront/v1/checkout/place-order",
                Ctx::place_body(&view),
            )
            .key("k"),
        )
        .await;
    let (status, list, _) = Call::get("/admin/v1/orders?status=pending")
        .tenant(tenant)
        .token(&employee)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"][0]["id"], placed["order_id"]);
    let (status, detail, _) = Call::get(&format!(
        "/admin/v1/orders/{}",
        placed["order_id"].as_str().unwrap()
    ))
    .tenant(tenant)
    .token(&employee)
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["order"]["number"], placed["number"]);
    assert_eq!(detail["attempts"].as_array().unwrap().len(), 1);
    let kinds: Vec<&str> = detail["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["placed", "legal_accepted"]);
    let (status, list, _) = Call::get("/admin/v1/orders")
        .tenant(c.other.tenant)
        .token(&owner)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{list}");
}
