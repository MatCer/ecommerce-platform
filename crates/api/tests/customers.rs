//! Customer accounts and consent through the Storefront API (as the edge calls it): magic
//! links, sessions, passwords (A5), addresses, cart merge (A4), consent (A20), rate limits and
//! tenant isolation. Real Postgres as the runtime role.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
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
    owner: PgPool,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        shop,
        other,
        runtime,
        owner: db,
        _jwks: jwks,
    }
}

fn sf<'a>(call: Call<'a>, shop: &Shop, market: Uuid) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", market.to_string())
        .header("x-client-ip", "203.0.113.7")
}

impl Ctx {
    async fn call(&self, call: Call<'_>) -> (StatusCode, Value, axum::response::Response) {
        sf(call, &self.shop, self.shop.cz).send(&self.s).await
    }

    /// Requests a link and returns its token, read from the queued email.
    async fn magic_token(&self, email: &str) -> String {
        let (status, _, _) = self
            .call(Call::post(
                "/storefront/v1/customer/magic-link",
                json!({"email": email}),
            ))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let text: String = sqlx::query_scalar(
            "SELECT body_text FROM email_messages WHERE template = 'magic_link' AND to_email = $1
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(email.to_lowercase())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let start = text.find("token=").unwrap() + "token=".len();
        text[start..start + 64].to_owned()
    }

    /// Signs in by magic link; returns the session token.
    async fn sign_in(&self, email: &str, extra: &[(&'static str, String)]) -> (String, Value) {
        let token = self.magic_token(email).await;
        let mut call = Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        );
        for (k, v) in extra {
            call = call.header(k, v.clone());
        }
        let (status, body, res) = self.call(call).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let session = res.headers()["x-session-token"]
            .to_str()
            .unwrap()
            .to_owned();
        (session, body)
    }

    async fn me(&self, session: &str) -> (StatusCode, Value) {
        let (s, b, _) = self
            .call(Call::get("/storefront/v1/customer/me").header("x-customer-session", session))
            .await;
        (s, b)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn magic_link_signs_in_once_and_verifies_the_address(db: PgPool) {
    let c = setup(db).await;
    let token = c.magic_token("Jana@Example.test").await;
    let (status, body, res) = c
        .call(Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["customer"]["email"], "jana@example.test");
    assert_eq!(body["customer"]["email_verified"], true);
    assert_eq!(body["customer"]["recently_verified"], true);
    assert_eq!(body["customer"]["has_password"], false);
    assert_eq!(body["redirect"], "/account");
    assert!(
        body.get("session_token").is_none(),
        "credentials never in the body"
    );
    assert_eq!(res.headers()["cache-control"], "no-store");
    let session = res.headers()["x-session-token"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(session.len(), 64);
    assert_eq!(c.me(&session).await.0, StatusCode::OK);

    // Single use.
    let (status, body, _) = c
        .call(Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_magic_link");

    // The WP10 hook: guest orders may be linked now.
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE type = 'customer.email_verified' AND payload->>'customer_id' = $1",
    )
    .bind(body_id(&c, &session).await)
    .fetch_one(&c.owner)
    .await
    .unwrap();
    assert_eq!(events, 1);

    // Signing in again with a new link does not re-announce the verification.
    c.sign_in("jana@example.test", &[]).await;
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE type = 'customer.email_verified'",
    )
    .fetch_one(&c.owner)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

async fn body_id(c: &Ctx, session: &str) -> String {
    c.me(session).await.1["id"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrations = "../../migrations")]
async fn magic_links_are_bound_to_their_market_and_expire(db: PgPool) {
    let c = setup(db).await;
    let token = c.magic_token("a@example.test").await;
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        ),
        &c.shop,
        c.shop.sk,
    )
    .send(&c.s)
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "another market's checkout host"
    );
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query("UPDATE customer_magic_links SET expires_at = now() - interval '1 second'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, _, _) = c
        .call(Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired");
}

#[sqlx::test(migrations = "../../migrations")]
async fn magic_link_requests_do_not_reveal_accounts_and_are_rate_limited(db: PgPool) {
    let c = setup(db).await;
    for _ in 0..3 {
        let (status, _, _) = c
            .call(Call::post(
                "/storefront/v1/customer/magic-link",
                json!({"email": "nobody@example.test", "redirect": "//evil.example"}),
            ))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, body, _) = c
        .call(Call::post(
            "/storefront/v1/customer/magic-link",
            json!({"email": "NOBODY@example.test"}),
        ))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "too_many_attempts");
    // No customer exists until a link is used.
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let customers: i64 = sqlx::query_scalar("SELECT count(*) FROM customers")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(customers, 0);
    // The stored redirect was sanitized; the email carries the checkout-origin link.
    let (redirect, text): (String, String) = sqlx::query_as(
        "SELECT l.redirect, m.body_text FROM customer_magic_links l, email_messages m LIMIT 1",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(redirect, "/account");
    assert!(
        text.contains("http://checkout.shop.localhost:8080/account/verify?token="),
        "{text}"
    );
    let (status, _, _) = c
        .call(Call::post(
            "/storefront/v1/customer/magic-link",
            json!({"email": "not-an-email"}),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../../migrations")]
async fn passwords_follow_a5(db: PgPool) {
    let c = setup(db).await;
    let (first, _) = c.sign_in("pat@example.test", &[]).await;
    let set = |session: String, body: Value| {
        Call::post("/storefront/v1/customer/password", body).header("x-customer-session", session)
    };
    // Weak passwords are refused; right after an email-link sign-in no current password is needed.
    let (status, _, _) = c
        .call(set(first.clone(), json!({"new_password": "short"})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body, _) = c
        .call(set(
            first.clone(),
            json!({"new_password": "correct horse battery"}),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Password sign-in.
    let (status, body, res) = c
        .call(Call::post(
            "/storefront/v1/customer/login",
            json!({"email": "PAT@example.test", "password": "correct horse battery", "redirect": "/account/security"}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["redirect"], "/account/security");
    assert_eq!(body["customer"]["recently_verified"], false);
    assert_eq!(body["customer"]["has_password"], true);
    let second = res.headers()["x-session-token"]
        .to_str()
        .unwrap()
        .to_owned();

    // Without a recent email link, the current password is required and checked.
    let (status, body, _) = c
        .call(set(
            second.clone(),
            json!({"new_password": "another good one"}),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("reauth_required"))
    );
    let (status, body, _) = c
        .call(set(
            second.clone(),
            json!({"current_password": "wrong password!", "new_password": "another good one"}),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("invalid_current_password"))
    );
    let (status, _, _) = c
        .call(set(
            second.clone(),
            json!({"current_password": "correct horse battery", "new_password": "another good one"}),
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The change signed out every other session, not this one.
    assert_eq!(c.me(&first).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(c.me(&second).await.0, StatusCode::OK);
    let (status, _, _) = c
        .call(Call::post(
            "/storefront/v1/customer/login",
            json!({"email": "pat@example.test", "password": "correct horse battery"}),
        ))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old password is gone");

    // Each change mailed a notice.
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let notices: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_messages WHERE template = 'password_changed'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(notices, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_logins_look_alike_and_are_rate_limited(db: PgPool) {
    let c = setup(db).await;
    let (session, _) = c.sign_in("lee@example.test", &[]).await;
    c.call(
        Call::post(
            "/storefront/v1/customer/password",
            json!({"new_password": "correct horse battery"}),
        )
        .header("x-customer-session", session),
    )
    .await;
    for (email, password) in [
        ("lee@example.test", "wrong password"),
        ("ghost@example.test", "whatever it is"),
        ("not-an-email", "x"),
    ] {
        let (status, body, _) = c
            .call(Call::post(
                "/storefront/v1/customer/login",
                json!({"email": email, "password": password}),
            ))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "invalid_credentials");
    }
    for _ in 0..9 {
        c.call(Call::post(
            "/storefront/v1/customer/login",
            json!({"email": "lee@example.test", "password": "wrong password"}),
        ))
        .await;
    }
    let (status, _, _) = c
        .call(Call::post(
            "/storefront/v1/customer/login",
            json!({"email": "lee@example.test", "password": "correct horse battery"}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "locked for the window even with the right password"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn logout_ends_the_session(db: PgPool) {
    let c = setup(db).await;
    let (session, _) = c.sign_in("x@example.test", &[]).await;
    let (status, _, res) = c
        .call(
            Call::post("/storefront/v1/customer/logout", json!({}))
                .header("x-customer-session", session.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(res.headers()["x-session-clear"], "1");
    assert_eq!(c.me(&session).await.0, StatusCode::UNAUTHORIZED);
    let (status, body) = c.me("garbage").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("not_signed_in"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_session_is_useless_for_another_tenant(db: PgPool) {
    let c = setup(db).await;
    let (session, _) = c.sign_in("x@example.test", &[]).await;
    let (status, _, _) = sf(
        Call::get("/storefront/v1/customer/me").header("x-customer-session", session.clone()),
        &c.other,
        c.other.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Nor can a magic link of one shop be consumed at another.
    let token = c.magic_token("y@example.test").await;
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/customer/magic-link/consume",
            json!({"token": token}),
        ),
        &c.other,
        c.other.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // RLS: the other tenant's transaction sees none of it.
    let mut tx = platform::db::tenant_tx(&c.runtime, c.other.tenant)
        .await
        .unwrap();
    for table in [
        "customers",
        "customer_sessions",
        "customer_magic_links",
        "customer_auth_attempts",
        "email_messages",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let write = sqlx::query(
        "INSERT INTO customers (tenant_id, email, locale) VALUES ($1, 'z@example.test', 'cs')",
    )
    .bind(c.shop.tenant)
    .execute(&mut *tx)
    .await;
    assert!(write.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn addresses_belong_to_their_customer(db: PgPool) {
    let c = setup(db).await;
    let (a, _) = c.sign_in("a@example.test", &[]).await;
    let (b, _) = c.sign_in("b@example.test", &[]).await;
    let address = json!({"name": "Jana Nováková", "street": "Dlouhá 1", "city": "Praha", "postal_code": "110 00", "country": "cz"});
    let (status, first, _) = c
        .call(
            Call::post("/storefront/v1/customer/addresses", address.clone())
                .header("x-customer-session", a.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(
        (first["country"].as_str(), first["is_default"].as_bool()),
        (Some("CZ"), Some(true))
    );
    let (_, second, _) = c
        .call(
            Call::post("/storefront/v1/customer/addresses", json!({"name": "Work", "street": "Krátká 2", "city": "Brno", "postal_code": "602 00", "country": "CZ", "is_default": true}))
                .header("x-customer-session", a.clone()),
        )
        .await;
    assert_eq!(second["is_default"], true);
    let (_, list, _) = c
        .call(
            Call::get("/storefront/v1/customer/addresses").header("x-customer-session", a.clone()),
        )
        .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], second["id"], "the default comes first");
    assert_eq!(items[1]["is_default"], false);

    let uri = format!(
        "/storefront/v1/customer/addresses/{}",
        first["id"].as_str().unwrap()
    );
    // Another customer cannot see, change or delete it.
    let (_, list, _) = c
        .call(
            Call::get("/storefront/v1/customer/addresses").header("x-customer-session", b.clone()),
        )
        .await;
    assert_eq!(list["items"], json!([]));
    let (status, _, _) = c
        .call(Call::put(&uri, address.clone()).header("x-customer-session", b.clone()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = c
        .call(Call::delete(&uri).header("x-customer-session", b.clone()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = c.call(Call::get("/storefront/v1/customer/addresses")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, updated, _) = c
        .call(Call::put(&uri, json!({"name": "Jana N.", "street": "Dlouhá 1", "city": "Praha", "postal_code": "110 00", "country": "CZ", "phone": "+420 777 123 456"})).header("x-customer-session", a.clone()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["phone"], "+420 777 123 456");
    let (status, _, _) = c
        .call(Call::delete(&uri).header("x-customer-session", a.clone()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = c
        .call(Call::post("/storefront/v1/customer/addresses", json!({"name": "x", "street": "y", "city": "z", "postal_code": "1", "country": "CZE"})).header("x-customer-session", a))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_address"))
    );
}

/// A shop cart handed off to the checkout origin; returns the checkout capability.
async fn checkout_cart(c: &Ctx, variant: Uuid, quantity: i32) -> String {
    let (status, _, res) = c.call(Call::post("/storefront/v1/cart", json!({}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let shop_token = res.headers()["x-cart-token"].to_str().unwrap().to_owned();
    let (status, body, _) = c
        .call(
            Call::post(
                "/storefront/v1/cart/lines",
                json!({"variant_id": variant, "quantity": quantity}),
            )
            .header("x-cart-token", shop_token.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body, _) = c
        .call(
            Call::post("/storefront/v1/cart/handoff", json!({})).header("x-cart-token", shop_token),
        )
        .await;
    let handoff = body["token"].as_str().unwrap().to_owned();
    let (_, body, _) = c
        .call(Call::post(
            "/storefront/v1/checkout/handoff",
            json!({"token": handoff}),
        ))
        .await;
    body["cart_token"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrations = "../../migrations")]
async fn sign_in_attaches_the_checkout_cart_and_merges_older_ones(db: PgPool) {
    let c = setup(db).await;
    let [v1, v2] = c.shop.variants;
    let first = checkout_cart(&c, v1, 2).await;
    c.sign_in("m@example.test", &[("x-cart-token", first.clone())])
        .await;

    // Later, on another device: a new anonymous cart, then sign-in again.
    let second = checkout_cart(&c, v1, 1).await;
    let (status, _, _) = c
        .call(
            Call::post("/storefront/v1/cart/lines", json!({"variant_id": v2}))
                .header("x-cart-token", second.clone()),
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "checkout capability is read-only until WP10"
    );
    c.sign_in("m@example.test", &[("x-cart-token", second.clone())])
        .await;

    let (status, cart, _) = c
        .call(Call::get("/storefront/v1/cart").header("x-cart-token", second))
        .await;
    assert_eq!(status, StatusCode::OK);
    let lines = cart["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["variant_id"], json!(v1));
    assert_eq!(lines[0]["quantity"], 3, "lines merged by variant");
    // The older cart is closed; its capability is dead.
    let (status, _, _) = c
        .call(Call::get("/storefront/v1/cart").header("x-cart-token", first))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let statuses: Vec<(String, bool)> =
        sqlx::query_as("SELECT status, customer_id IS NOT NULL FROM carts ORDER BY created_at")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![("merged".into(), true), ("open".into(), true)]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn consent_is_recorded_resolved_and_linked_at_sign_in(db: PgPool) {
    let c = setup(db).await;
    // First choice on the shop origin: a subject is minted and returned.
    let (status, state, res) = c
        .call(Call::post(
            "/storefront/v1/consent",
            json!({"purposes": {"analytics": true, "ads": false, "personalization": true}, "text_version": "2026-09-25"}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{state}");
    let subject = res.headers()["x-consent-subject"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(subject.len(), 32);
    assert_eq!(res.headers()["x-consent-summary"], "2026-09-25.101--");
    assert_eq!(state["purposes"]["analytics"], true);

    // Later change: only analytics withdrawn.
    let (_, state, res) = c
        .call(
            Call::post("/storefront/v1/consent", json!({"purposes": {"analytics": false}, "text_version": "2026-09-25", "source": "preferences"}))
                .header("x-consent-subject", subject.clone()),
        )
        .await;
    assert_eq!(res.headers()["x-consent-subject"], subject.as_str());
    assert_eq!(
        state["purposes"],
        json!({"analytics": false, "ads": false, "personalization": true, "email_marketing": null, "review_invites": null})
    );
    let (_, got, _) = c
        .call(Call::get("/storefront/v1/consent").header("x-consent-subject", subject.clone()))
        .await;
    assert_eq!(got, state);
    let (_, empty, _) = c.call(Call::get("/storefront/v1/consent")).await;
    assert_eq!(empty["text_version"], Value::Null);

    // Clients cannot forge the source or send unknown purposes.
    for bad in [
        json!({"purposes": {"analytics": true}, "text_version": "v", "source": "linked"}),
        json!({"purposes": {"tracking": true}, "text_version": "v"}),
        json!({"purposes": {}, "text_version": "v"}),
        json!({"purposes": {"ads": true}, "text_version": "v 1"}),
    ] {
        let (status, _, _) = c.call(Call::post("/storefront/v1/consent", bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Sign-in copies the anonymous choices to the customer; the server resolves from records.
    let (session, body) = c
        .sign_in("k@example.test", &[("x-consent-subject", subject.clone())])
        .await;
    let customer = Uuid::parse_str(body["customer"]["id"].as_str().unwrap()).unwrap();
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let who = commerce::consent::Subject::Customer(customer);
    use commerce::consent::{ConsentPurpose as P, current};
    assert!(!current(&mut tx, &who, P::Analytics).await.unwrap());
    assert!(current(&mut tx, &who, P::Personalization).await.unwrap());
    assert!(
        !current(&mut tx, &who, P::EmailMarketing).await.unwrap(),
        "no record = no consent"
    );
    tx.commit().await.unwrap();

    // Signed in on the checkout origin, the choice is recorded for the customer too.
    let (_, _, _) = c
        .call(
            Call::post("/storefront/v1/consent", json!({"purposes": {"email_marketing": true}, "text_version": "2026-09-25", "source": "preferences"}))
                .header("x-consent-subject", subject)
                .header("x-customer-session", session.clone()),
        )
        .await;
    let (_, mine, _) = c
        .call(Call::get("/storefront/v1/consent").header("x-customer-session", session))
        .await;
    assert_eq!(mine["purposes"]["email_marketing"], true);
    assert_eq!(mine["purposes"]["personalization"], true);
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let (ip_hashes, sources): (i64, Vec<String>) = (
        sqlx::query_scalar("SELECT count(*) FROM consent_records WHERE ip_hash IS NOT NULL")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT DISTINCT source FROM consent_records ORDER BY 1")
            .fetch_all(&mut *tx)
            .await
            .unwrap(),
    );
    assert!(ip_hashes > 0);
    assert_eq!(sources, ["banner", "linked", "preferences"]);
    // Append-only for the application.
    assert!(
        sqlx::query("UPDATE consent_records SET granted = true")
            .execute(&mut *tx)
            .await
            .is_err()
    );
}
