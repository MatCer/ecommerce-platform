use chrono::DateTime;
use commerce::payments::{MethodKind, PaymentMethodInput};
use commerce::shipping::{Carrier, ShippingMethodInput};

use super::*;

/// Fails the replacement only once `OLD` already holds the case's detail write, and records
/// that in a sequence, which survives the abort.
const FAIL_REPLACEMENT_AFTER_DETAIL_WRITE: &str = "
CREATE SEQUENCE pii_rotation_witness;
GRANT USAGE ON SEQUENCE pii_rotation_witness TO app_runtime;
CREATE FUNCTION pii_rotation_fail() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.email = 'injected@example.cz'
       OR OLD.billing_address->>'name' = 'Injected'
       OR EXISTS (SELECT 1 FROM orders WHERE cart_id = OLD.id AND notes = 'injected') THEN
        PERFORM nextval('pii_rotation_witness');
        RAISE EXCEPTION 'injected failure after the detail write';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER pii_rotation_fail BEFORE UPDATE OF checkout_token_hash ON carts
    FOR EACH ROW EXECUTE FUNCTION pii_rotation_fail();
";

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct Committed {
    checkout_token_hash: Option<Vec<u8>>,
    email: Option<String>,
    billing_name: Option<String>,
    version: i32,
    status: String,
    last_activity_at: DateTime<Utc>,
}

enum Write {
    Details(Value),
    Place(Value),
}

fn contact(email: &str) -> Value {
    json!({ "contact": { "email": email } })
}

fn billing(name: &str) -> Value {
    json!({ "name": name, "street": "Dlouhá 12", "city": "Praha", "postal_code": "110 00",
            "country": "CZ" })
}

fn addresses(name: &str) -> Value {
    json!({ "addresses": { "billing": billing(name) } })
}

fn place_body(view: &Value, notes: &str) -> Value {
    json!({ "version": view["cart"]["version"],
            "total_minor": view["totals"]["total"]["amount_minor"],
            "accept_terms": true, "accept_withdrawal": true, "notes": notes })
}

fn cart_id(view: &Value) -> Uuid {
    view["cart"]["id"].as_str().unwrap().parse().unwrap()
}

fn holds_a_capability(text: &str) -> bool {
    text.split(|ch: char| !matches!(ch, '0'..='9' | 'a'..='f'))
        .any(|run| run.len() >= 64)
}

impl Ctx {
    async fn placeable(&self) -> Uuid {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let method = commerce::shipping::create(
            &mut tx,
            "t",
            &ShippingMethodInput {
                market_id: self.shop.cz,
                carrier: Carrier::Ppl,
                name_i18n: [("cs".to_owned(), "PPL".to_owned())].into(),
                description_i18n: Default::default(),
                price_minor: 9900,
                free_over_minor: None,
                weight_tiers: vec![],
                cod_allowed: false,
                cod_fee_minor: 0,
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
            &self.s.checkout.payments,
            self.shop.cz,
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
        method
    }

    async fn ready(&self, cart: &str, method: Uuid) -> Value {
        let mut view = Value::Null;
        for step in [
            Call::put(
                "/storefront/v1/checkout/shipping",
                json!({ "method_id": method }),
            ),
            Call::put(
                "/storefront/v1/checkout/payment",
                json!({ "method": "fake" }),
            ),
        ] {
            let (status, body, res) = self
                .edge
                .call(step)
                .header("x-cart-token", cart.to_owned())
                .send(&self.s)
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(
                replacement(&res),
                None,
                "a non-PII write keeps the capability"
            );
            view = body;
        }
        view
    }

    async fn place(
        &self,
        cart: &str,
        key: &str,
        body: &Value,
    ) -> (StatusCode, String, axum::response::Response) {
        self.edge
            .call(Call::post("/storefront/v1/checkout/place-order", body.clone()).key(key))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await
    }

    async fn read<T>(&self, sql: &'static str, id: Uuid) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
    {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let value = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        value
    }

    async fn committed(&self, cart: Uuid) -> Committed {
        let mut tx = platform::db::tenant_tx(&self.runtime, self.shop.tenant)
            .await
            .unwrap();
        let row = sqlx::query_as(
            "SELECT checkout_token_hash, email, billing_address->>'name' AS billing_name, version,
                    status, last_activity_at
             FROM carts WHERE id = $1",
        )
        .bind(cart)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        row
    }

    async fn witness(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM pii_rotation_witness",
        )
        .fetch_one(&self.db)
        .await
        .unwrap()
    }

    async fn write(&self, cart: &str, write: &Write) -> (StatusCode, String, Option<String>) {
        match write {
            Write::Details(body) => self.details(cart, body.clone()).await,
            Write::Place(body) => {
                let (status, text, res) = self.place(cart, "p-1", body).await;
                (status, text, replacement(&res))
            }
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_pii_write_rotates(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let k0 = c.started(&shop_token, None).await;

    let k1 = c.rotated(&k0, contact("jana@example.cz")).await;
    c.assert_restart(&k0, "jana@example.cz", "the contact write revoked K0")
        .await;
    let (status, view) = c.checkout(&k1).await;
    assert_eq!(
        (status, view["email"].as_str()),
        (StatusCode::OK, Some("jana@example.cz"))
    );

    let k2 = c.rotated(&k1, addresses("Jana Nováková")).await;
    c.assert_restart(&k1, "Nováková", "the address write revoked K1")
        .await;
    let (status, view) = c.checkout(&k2).await;
    assert_eq!(
        (status, view["billing_address"]["name"].as_str()),
        (StatusCode::OK, Some("Jana Nováková"))
    );

    let view = c.ready(&k2, method).await;
    let body = place_body(&view, "Zvonit dvakrát");
    let (status, text, res) = c.place(&k2, "p-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let k3 = replacement(&res).expect("the note replaced the capability");
    assert!(
        commerce::capability::well_formed(&k3) && k3 != k2,
        "a new capability"
    );
    assert!(
        !text.contains(&k3) && !text.contains(&k2),
        "no capability in the body"
    );
    let placed: Value = serde_json::from_str(&text).unwrap();
    let order: Uuid = placed["order_id"].as_str().unwrap().parse().unwrap();
    let notes: Option<String> = c
        .read("SELECT notes FROM orders WHERE id = $1", order)
        .await;
    assert_eq!(notes.as_deref(), Some("Zvonit dvakrát"));

    let (status, text, res) = c.place(&k2, "p-1", &body).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the replaced capability replays nothing: {text}"
    );
    assert_eq!(replacement(&res), None);
    let (status, text, res) = c.place(&k3, "p-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(res.headers()["idempotent-replayed"], "true");
    let again: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(again["order_id"], placed["order_id"]);
    assert_eq!(replacement(&res), None, "a replay replaces nothing");
}

#[sqlx::test(migrations = "../../migrations")]
async fn prefilled_attacker_checkout_rotates_again(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let ka0 = c.started(&shop_token, None).await;
    let ka1 = c.rotated(&ka0, contact("attacker@evil.cz")).await;
    let ka2 = c.rotated(&ka1, addresses("Attacker")).await;
    let (status, planted) = c.checkout(&ka2).await;
    assert_eq!(status, StatusCode::OK, "{planted}");

    let kv1 = c.rotated(&ka2, contact("victim@example.cz")).await;
    let kv2 = c.rotated(&kv1, addresses("Victim")).await;

    c.assert_restart(&ka2, "victim@example.cz", "the planted capability")
        .await;
    c.assert_restart(&kv1, "victim@example.cz", "the victim's first replacement")
        .await;
    let (status, text, replaced) = c.details(&ka2, contact("attacker@evil.cz")).await;
    assert_eq!(
        (status, replaced),
        (StatusCode::NOT_FOUND, None),
        "the planter cannot write again: {text}"
    );
    let (status, _) = c
        .call(
            Call::put(
                "/storefront/v1/checkout/shipping",
                json!({ "method_id": method }),
            ),
            &ka2,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nor choose shipping");
    let (status, text, _) = c.place(&ka2, "p-1", &place_body(&planted, "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nor place: {text}");

    let (status, view) = c.checkout(&kv2).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        (
            view["email"].as_str(),
            view["billing_address"]["name"].as_str()
        ),
        (Some("victim@example.cz"), Some("Victim"))
    );
    assert_eq!(
        view["cart"]["id"], planted["cart"]["id"],
        "the planted checkout, under a capability its planter never saw"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn non_pii_stale_write_denied(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let k0 = c.started(&shop_token, None).await;
    let k1 = c.rotated(&k0, contact("jana@example.cz")).await;
    let (_, before) = c.checkout(&k1).await;

    for (what, call) in [
        (
            "PUT shipping",
            Call::put(
                "/storefront/v1/checkout/shipping",
                json!({ "method_id": method }),
            ),
        ),
        (
            "PUT payment",
            Call::put(
                "/storefront/v1/checkout/payment",
                json!({ "method": "fake" }),
            ),
        ),
        ("GET cart", Call::get("/storefront/v1/cart")),
    ] {
        let (status, text, res) = c
            .edge
            .call(call)
            .header("x-cart-token", k0.clone())
            .send_text(&c.s)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{what} with K0: {text}");
        assert!(!text.contains("jana@example.cz"), "{what}: {text}");
        assert_eq!(replacement(&res), None, "{what}");
    }
    c.assert_restart(&k0, "jana@example.cz", "GET checkout with K0")
        .await;
    let (status, after) = c.checkout(&k1).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        (&after["shipping_method_id"], &after["payment_method"]),
        (&Value::Null, &Value::Null),
        "no stale selection landed"
    );
    assert_eq!(after["cart"]["version"], before["cart"]["version"]);

    let (status, view, res) = c
        .edge
        .call(Call::put(
            "/storefront/v1/checkout/shipping",
            json!({ "method_id": method }),
        ))
        .header("x-cart-token", k1.clone())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(
        replacement(&res),
        None,
        "a non-PII write keeps the capability"
    );
    assert_eq!(view["shipping_method_id"], json!(method));
    let (status, view) = c.checkout(&k1).await;
    assert_eq!(status, StatusCode::OK, "K1 still opens: {view}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_write_keeps_previous_generation(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let k0 = c.started(&shop_token, None).await;
    let k1 = c.rotated(&k0, contact("kept@example.cz")).await;
    let k1 = c.rotated(&k1, addresses("Kept")).await;
    let view = c.ready(&k1, method).await;
    let id = cart_id(&view);

    let abroad = json!({ "addresses": { "billing": billing("Kept"), "shipping": {
        "name": "Kept", "street": "Unter den Linden 1", "city": "Berlin",
        "postal_code": "10117", "country": "DE" } } });
    for (what, body, expected) in [
        ("an invalid email", contact("not-an-email"), "invalid_email"),
        ("a delivery abroad", abroad, "ship_to_not_allowed"),
    ] {
        let before = c.committed(id).await;
        let (status, text, replaced) = c.details(&k1, body).await;
        assert_eq!(
            (status, code(&text)),
            (StatusCode::UNPROCESSABLE_ENTITY, json!(expected)),
            "{what}"
        );
        assert_eq!(replaced, None, "{what}");
        assert_eq!(c.committed(id).await, before, "{what} changed nothing");
    }

    sqlx::raw_sql(FAIL_REPLACEMENT_AFTER_DETAIL_WRITE)
        .execute(&c.db)
        .await
        .unwrap();
    let cases = [
        ("contact", Write::Details(contact("injected@example.cz"))),
        ("addresses", Write::Details(addresses("Injected"))),
        ("note", Write::Place(place_body(&view, "injected"))),
    ];
    for (witnessed, (what, write)) in (1..).zip(&cases) {
        let before = c.committed(id).await;
        let (status, text, replaced) = c.write(&k1, write).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{what}: {text}");
        assert_eq!(replaced, None, "{what}: no replacement left the API");
        assert_eq!(
            c.witness().await,
            witnessed,
            "{what}: the replacement ran after the detail write, once"
        );
        assert_eq!(
            c.committed(id).await,
            before,
            "{what}: the write rolled back with the replacement"
        );
        let orders: i64 = c
            .read("SELECT count(*) FROM orders WHERE cart_id = $1", id)
            .await;
        assert_eq!(orders, 0, "{what}: no order");
        let (status, view) = c.checkout(&k1).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{what}: the previous capability still opens: {view}"
        );
    }

    sqlx::query("DROP TRIGGER pii_rotation_fail ON carts")
        .execute(&c.db)
        .await
        .unwrap();
    let k2 = c.rotated(&k1, contact("injected@example.cz")).await;
    let k3 = c.rotated(&k2, addresses("Injected")).await;
    let (_, view) = c.checkout(&k3).await;
    let (status, text, res) = c.place(&k3, "p-1", &place_body(&view, "injected")).await;
    assert_eq!(status, StatusCode::CREATED, "control: {text}");
    let k4 = replacement(&res).expect("control: the note replaced the capability");
    assert!(
        commerce::capability::well_formed(&k4) && k4 != k3,
        "control: a new capability"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn theme_json_pii_write_never_returns_token(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let shop_token = c.shop_cart().await;
    let k = c.started(&shop_token, None).await;
    let (status, view) = c.checkout(&k).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let id = cart_id(&view);
    let before = c.committed(id).await;

    let unknown = "0".repeat(64);
    for (what, path, body) in [
        (
            "PUT contact",
            "/storefront/v1/checkout/contact",
            json!({ "email": "jana@example.cz" }),
        ),
        (
            "PUT addresses",
            "/storefront/v1/checkout/addresses",
            json!({ "billing": billing("Jana Nováková") }),
        ),
    ] {
        for cart in [&k, &unknown] {
            let (status, text, res) = c
                .edge
                .call(Call::put(path, body.clone()))
                .header("x-cart-token", cart.clone())
                .send_text(&c.s)
                .await;
            assert_eq!(
                (status, code(&text)),
                (StatusCode::CONFLICT, json!("checkout_mode_changed")),
                "{what}"
            );
            assert_eq!(replacement(&res), None, "{what}");
            assert!(!holds_a_capability(&text), "{what}: {text}");
        }
    }
    assert_eq!(
        c.committed(id).await,
        before,
        "the refusals locked, refreshed and wrote nothing"
    );
    let (status, view) = c.checkout(&k).await;
    assert_eq!(
        (status, &view["email"]),
        (StatusCode::OK, &Value::Null),
        "K still opens"
    );

    let rotated = c.rotated(&k, contact("jana@example.cz")).await;
    let (status, text, res) = c
        .edge
        .call(Call::get("/storefront/v1/checkout"))
        .header("x-cart-token", rotated)
        .send_text(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!holds_a_capability(&text), "the view holds no capability");
    assert_eq!(replacement(&res), None, "nor its headers");
}
