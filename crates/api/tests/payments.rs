//! Payment adapters through the HTTP API: the signed Stripe webhook (storage before 200,
//! redelivery), Stripe onboarding + checkout + the test simulator against stripe-mock
//! (`STRIPE_MOCK_URL`), bank accounts with fresh-auth rules, statement upload, the
//! exceptions queue and cash-on-delivery actions with roles.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use commerce::payments::{MethodKind, PaymentMethodInput};
use commerce::shipping::{Carrier, ShippingMethodInput};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

mod common;
use common::*;

const CZ_IBAN: &str = "CZ6508000000192000145399";

struct Ctx {
    s: api::AppState,
    shop: Shop,
    runtime: PgPool,
    home: Uuid,
    owner: String,
    employee: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "pay").await;
    let s = state(runtime.clone(), &jwks, Duration::from_secs(30));
    let mut tx = platform::db::tenant_tx(&runtime, shop.tenant)
        .await
        .unwrap();
    let home = commerce::shipping::create(
        &mut tx,
        "t",
        &ShippingMethodInput {
            market_id: shop.cz,
            carrier: Carrier::PacketaHome,
            name_i18n: [("cs".to_owned(), "Domů".to_owned())].into(),
            description_i18n: Default::default(),
            price_minor: 7900,
            free_over_minor: None,
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
    for kind in [
        MethodKind::Stripe,
        MethodKind::BankTransfer,
        MethodKind::Cod,
    ] {
        commerce::payments::configure(
            &mut tx,
            "t",
            &s.checkout.payments,
            shop.cz,
            kind,
            &PaymentMethodInput {
                enabled: true,
                name_i18n: Default::default(),
                timeout_minutes: None,
                position: 0,
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    testkit::staff(&runtime, shop.tenant, "owner", "owner").await;
    testkit::staff(&runtime, shop.tenant, "employee", "staff").await;
    Ctx {
        s,
        shop,
        runtime,
        home,
        owner: sign(&claims("owner")),
        employee: sign(&claims("employee")),
        _jwks: jwks,
    }
}

fn sf<'a>(call: Call<'a>, shop: &Shop) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", shop.cz.to_string())
        .header("x-client-ip", "203.0.113.7")
}

impl Ctx {
    async fn admin(&self, call: Call<'_>, token: &str) -> (StatusCode, Value) {
        let (status, body, _) = call
            .token(token)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await;
        (status, body)
    }

    async fn sf(&self, call: Call<'_>) -> (StatusCode, Value, axum::response::Response) {
        sf(call, &self.shop).send(&self.s).await
    }

    /// Places a one-unit order paid with `method` through the storefront API; returns the
    /// placement response and the checkout capability.
    async fn place(&self, method: &str) -> (Value, String) {
        let (_, _, res) = self.sf(Call::post("/storefront/v1/cart", json!({}))).await;
        let shop_token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
        self.sf(Call::post(
            "/storefront/v1/cart/lines",
            json!({"variant_id": self.shop.variants[0], "quantity": 1}),
        )
        .header("x-cart-token", shop_token.clone()))
            .await;
        let (_, body, _) = self
            .sf(Call::post("/storefront/v1/cart/handoff", json!({}))
                .header("x-cart-token", shop_token))
            .await;
        let (_, body, _) = self
            .sf(Call::post(
                "/storefront/v1/checkout/handoff",
                json!({"token": body["token"]}),
            ))
            .await;
        let cart = body["cart_token"].as_str().unwrap().to_owned();
        let mut view = Value::Null;
        for step in [
            Call::put(
                "/storefront/v1/checkout/contact",
                json!({"email": "jana@example.test"}),
            ),
            Call::put(
                "/storefront/v1/checkout/addresses",
                json!({"billing": {"name": "Jana", "street": "Dlouhá 12", "city": "Praha",
                                   "postal_code": "110 00", "country": "CZ"}}),
            ),
            Call::put(
                "/storefront/v1/checkout/shipping",
                json!({"method_id": self.home}),
            ),
            Call::put("/storefront/v1/checkout/payment", json!({"method": method})),
        ] {
            let (status, body, _) = self.sf(step.header("x-cart-token", cart.clone())).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            view = body;
        }
        let (status, placed, _) = self
            .sf(Call::post(
                "/storefront/v1/checkout/place-order",
                json!({
                    "version": view["cart"]["version"],
                    "total_minor": view["totals"]["total"]["amount_minor"],
                    "accept_terms": true, "accept_withdrawal": true,
                }),
            )
            .header("x-cart-token", cart.clone())
            .key(&Uuid::now_v7().to_string()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{placed}");
        (placed, cart)
    }

    /// What the worker does: process every stored, unprocessed provider event.
    async fn process_events(&self) {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM platform.provider_events WHERE processed_at IS NULL ORDER BY received_at",
        )
        .fetch_all(&self.runtime)
        .await
        .unwrap();
        for id in ids {
            commerce::payments::stripe::process_event(&self.runtime, id)
                .await
                .unwrap();
        }
    }
}

fn signed(c: &Ctx, body: &str) -> String {
    let stripe = c.s.checkout.payments.stripe.as_ref().unwrap();
    stripe.sign(body.as_bytes(), Utc::now()).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn stripe_webhook_verifies_stores_and_dedupes(db: PgPool) {
    let c = setup(db).await;
    let body = json!({"id": "evt_http_1", "type": "payment_intent.succeeded", "livemode": false,
                      "account": "acct_unknown", "data": {"object": {}}})
    .to_string();
    let post = |sig: Option<String>| {
        let call = Call::post_raw("/webhooks/stripe", body.clone(), "application/json");
        match sig {
            Some(s) => call.header("stripe-signature", s),
            None => call,
        }
    };
    let (status, problem, _) = post(None).send(&c.s).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_signature"))
    );
    let forged = signed(&c, &body).replace("v1=", "v1=00");
    let (status, _, _) = post(Some(forged)).send(&c.s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, ok, _) = post(Some(signed(&c, &body))).send(&c.s).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok, json!({"received": true, "new": true}));
    let (status, again, _) = post(Some(signed(&c, &body))).send(&c.s).await;
    assert_eq!(
        (status, again["new"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    c.process_events().await;
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM platform.provider_events WHERE event_id = 'evt_http_1'",
    )
    .fetch_one(&c.runtime)
    .await
    .unwrap();
    assert_eq!(outcome, "rejected", "an unknown account is never applied");
}

/// stripe-mock: onboarding (simulated), then a Stripe checkout paid with the simulator.
#[sqlx::test(migrations = "../../migrations")]
async fn stripe_onboarding_checkout_and_simulator(db: PgPool) {
    let c = setup(db).await;
    let (status, s) = c
        .admin(Call::get("/admin/v1/payments/stripe"), &c.employee)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s, json!({"mode": "simulator", "account": null}));
    let (status, _) = c
        .admin(
            Call::post("/admin/v1/payments/stripe/onboarding", json!({})),
            &c.employee,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, link) = c
        .admin(
            Call::post("/admin/v1/payments/stripe/onboarding", json!({})),
            &c.owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    assert!(
        link["url"]
            .as_str()
            .unwrap()
            .ends_with("/settings/payments?stripe=return")
    );
    c.process_events().await;
    let (_, s) = c
        .admin(Call::get("/admin/v1/payments/stripe"), &c.owner)
        .await;
    assert_eq!(s["account"]["ready"], true, "{s}");

    let (placed, cart) = c.place("stripe").await;
    assert_eq!(
        placed["payment"]["action"],
        json!({"type": "stripe_simulator"})
    );
    let token = placed["confirmation_url"]
        .as_str()
        .unwrap()
        .trim_start_matches("/o/")
        .to_owned();
    let attempt = placed["payment"]["attempt_id"].as_str().unwrap().to_owned();
    let sim_url = format!("/storefront/v1/orders/{token}/payment-attempts/{attempt}/simulate");
    let simulate = |outcome: &str, cart: Option<&str>| {
        let call = Call::post(&sim_url, json!({"outcome": outcome}));
        match cart {
            Some(k) => call.header("x-cart-token", k.to_owned()),
            None => call,
        }
    };
    // The order token alone is read-only (A4).
    let (status, _, _) = c.sf(simulate("succeeded", None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, payment, _) = c.sf(simulate("succeeded", Some(&cart))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{payment}");
    c.process_events().await;
    let (_, order, _) = c
        .sf(Call::get(&format!("/storefront/v1/orders/{token}")))
        .await;
    assert_eq!(order["status"], "confirmed", "{order}");
    assert_eq!(order["payment"]["status"], "paid");

    // Capability loss (simulated account.updated) hides Stripe at checkout.
    let (status, _) = c
        .admin(
            Call::post(
                "/admin/v1/payments/stripe/simulate",
                json!({"enabled": false}),
            ),
            &c.owner,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    c.process_events().await;
    let (_, s) = c
        .admin(Call::get("/admin/v1/payments/stripe"), &c.owner)
        .await;
    assert_eq!(s["account"]["ready"], false);
    let (_, methods) = c
        .admin(
            Call::get(&format!("/admin/v1/markets/{}/payment-methods", c.shop.cz)),
            &c.owner,
        )
        .await;
    let stripe = methods["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "stripe")
        .unwrap();
    assert_eq!(stripe["unavailable_reason"], "stripe_onboarding");
}

#[sqlx::test(migrations = "../../migrations")]
async fn bank_accounts_statements_and_the_exceptions_queue(db: PgPool) {
    let c = setup(db).await;
    let url = format!("/admin/v1/markets/{}/bank-account", c.shop.cz);
    let account = json!({"iban": "CZ65 0800 0000 1920 0014 5399", "bic": "gibaczpx",
                         "account_name": "Demo s.r.o."});
    let (status, _) = c.admin(Call::put(&url, account.clone()), &c.employee).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut stale = claims("owner");
    stale["auth_time"] = json!(now() - 3600);
    let (status, body) = c
        .admin(Call::put(&url, account.clone()), &sign(&stale))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("reauth_required"))
    );
    let (status, bad) = c
        .admin(
            Call::put(
                &url,
                json!({"iban": "CZ6508000000192000145398", "account_name": "x"}),
            ),
            &c.owner,
        )
        .await;
    assert_eq!(
        (status, bad["code"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("invalid_bank_account")
        )
    );
    let (status, saved) = c.admin(Call::put(&url, account), &c.owner).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        (saved["iban"].as_str(), saved["bic"].as_str()),
        (Some(CZ_IBAN), Some("GIBACZPX"))
    );
    assert_eq!(saved["fio_connected"], false);
    let (status, _) = c
        .admin(
            Call::put(
                &url,
                json!({"iban": CZ_IBAN, "account_name": "Demo", "fio_token": "abcdefgh12"}),
            ),
            &c.owner,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no SECRETS_KEY in tests"
    );

    let statement = std::fs::read(format!(
        "{}/../../fixtures/bank/camt053.xml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let upload = |format: &str| {
        Call::post_raw(
            // The URI must outlive the call: leak one small string per upload in this test.
            Box::leak(
                format!(
                    "/admin/v1/bank-accounts/{}/statements?format={format}",
                    saved["id"].as_str().unwrap()
                )
                .into_boxed_str(),
            ),
            statement.clone(),
            "application/octet-stream",
        )
    };
    let (status, _) = c.admin(upload("camt053"), &c.employee).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, report) = c.admin(upload("camt053"), &c.owner).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(
        report,
        json!({"imported": 4, "duplicates": 0, "debits": 1, "matched": 0, "exceptions": 4})
    );
    let (_, again) = c.admin(upload("camt053"), &c.owner).await;
    assert_eq!(again["duplicates"], 4);
    let (status, problem) = c.admin(upload("gpc"), &c.owner).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_statement"))
    );
    let (status, _) = c.admin(upload("pdf"), &c.owner).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, list) = c
        .admin(
            Call::get("/admin/v1/bank-transactions?status=unmatched"),
            &c.employee,
        )
        .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 4);
    let (_, queue) = c
        .admin(Call::get("/admin/v1/payment-exceptions"), &c.employee)
        .await;
    assert_eq!(queue["bank_transactions"].as_array().unwrap().len(), 4);
    let first = queue["bank_transactions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, resolved) = c
        .admin(
            Call::post(
                Box::leak(format!("/admin/v1/bank-transactions/{first}/resolve").into_boxed_str()),
                json!({"action": "dismiss", "note": "not ours"}),
            ),
            &c.owner,
        )
        .await;
    assert_eq!(
        (status, resolved["status"].as_str()),
        (StatusCode::OK, Some("dismissed"))
    );
    let (_, queue) = c
        .admin(Call::get("/admin/v1/payment-exceptions"), &c.employee)
        .await;
    assert_eq!(queue["bank_transactions"].as_array().unwrap().len(), 3);

    // A bank-transfer order shows its instructions and QR on the order page.
    let (placed, _) = c.place("bank_transfer").await;
    assert_eq!(placed["payment"]["action"], json!({"type": "none"}));
    let token = placed["confirmation_url"]
        .as_str()
        .unwrap()
        .trim_start_matches("/o/")
        .to_owned();
    let (_, order, _) = c
        .sf(Call::get(&format!("/storefront/v1/orders/{token}")))
        .await;
    let bank = &order["payment"]["bank_transfer"];
    assert_eq!(bank["variable_symbol"], placed["number"]);
    assert_eq!(bank["qr_kind"], "spayd");
    assert!(bank["qr_svg"].as_str().unwrap().starts_with("<svg"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn cod_actions_need_an_admin(db: PgPool) {
    let c = setup(db).await;
    let (placed, _) = c.place("cod").await;
    let id = placed["order_id"].as_str().unwrap().to_owned();
    let at = |action: &str| {
        Box::leak(format!("/admin/v1/orders/{id}/cod/{action}").into_boxed_str()) as &str
    };
    let (status, _) = c
        .admin(Call::post(at("deliver"), json!({})), &c.employee)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, a) = c
        .admin(Call::post(at("deliver"), json!({})), &c.owner)
        .await;
    assert_eq!(
        (status, a["cod_status"].as_str()),
        (StatusCode::OK, Some("delivered")),
        "{a}"
    );
    let (status, a) = c
        .admin(
            Call::post(
                at("collect"),
                json!({"tender": "card", "collector": "carrier"}),
            ),
            &c.owner,
        )
        .await;
    assert_eq!(
        (status, a["cod_status"].as_str(), a["status"].as_str()),
        (StatusCode::OK, Some("collected"), Some("succeeded"))
    );
    let (status, a) = c
        .admin(
            Call::post(at("remit"), json!({"note": "payout 42"})),
            &c.owner,
        )
        .await;
    assert_eq!(
        (status, a["cod_status"].as_str()),
        (StatusCode::OK, Some("remitted"))
    );
    let (status, p) = c.admin(Call::post(at("remit"), json!({})), &c.owner).await;
    assert_eq!(
        (status, p["code"].as_str()),
        (StatusCode::CONFLICT, Some("invalid_transition"))
    );
    let (status, report) = c
        .admin(
            Call::post_raw(
                "/admin/v1/cod-reports",
                format!(
                    "order_number;amount;tender;event\n{};1;cash;remitted\n",
                    placed["number"].as_str().unwrap()
                ),
                "text/csv",
            ),
            &c.owner,
        )
        .await;
    assert_eq!(
        (status, report["skipped"].as_u64()),
        (StatusCode::OK, Some(1)),
        "{report}"
    );
}
