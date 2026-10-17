use super::pii_rotation::{addresses, cart_id, contact, holds_a_capability, place_body};
use super::*;

struct Placed {
    shop_token: String,
    rotated_away: [String; 2],
    presented: String,
    current: String,
    body: Value,
    number: Value,
    cart: Uuid,
}

impl Ctx {
    pub(super) async fn receipt(&self, cart: &str) -> (StatusCode, String) {
        let (status, text, res) = self
            .edge
            .call(Call::get("/storefront/v1/checkout/order"))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        assert_eq!(replacement(&res), None, "a receipt carries no capability");
        (status, text)
    }

    async fn receipt_number(&self, cart: &str, what: &str) -> Value {
        let (status, text) = self.receipt(cart).await;
        assert_eq!(status, StatusCode::OK, "{what}: {text}");
        serde_json::from_str::<Value>(&text).unwrap()["number"].clone()
    }

    async fn assert_no_receipt(&self, cart: &str, what: &str) {
        let (status, text) = self.receipt(cart).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{what}: {text}");
        assert_eq!(code(&text), "not_found", "{what}");
    }

    async fn assert_receipts(&self, p: &Placed) {
        for (what, k) in [("K", &p.presented), ("K'", &p.current)] {
            assert_eq!(self.receipt_number(k, what).await, p.number, "{what}");
        }
    }

    async fn assert_no_receipts(&self, p: &Placed, what: &str) {
        for k in [&p.presented, &p.current] {
            self.assert_no_receipt(k, what).await;
        }
    }

    async fn placed(&self, method: Uuid, note: &str) -> Placed {
        let shop_token = self.shop_cart().await;
        let k0 = self.started(&shop_token, None).await;
        let k1 = self.rotated(&k0, contact("jana@example.cz")).await;
        let k2 = self.rotated(&k1, addresses("Jana Nováková")).await;
        let view = self.ready(&k2, method).await;
        let body = place_body(&view, note);
        let (status, text, res) = self.place(&k2, "p-1", &body).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let placed: Value = serde_json::from_str(&text).unwrap();
        Placed {
            shop_token,
            rotated_away: [k0, k1],
            current: replacement(&res).unwrap_or_else(|| k2.clone()),
            presented: k2,
            body,
            number: placed["number"].clone(),
            cart: cart_id(&view),
        }
    }

    async fn effects(&self, cart: Uuid) -> [i64; 4] {
        let outbox: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM queue.outbox x JOIN orders o ON x.payload->>'order_id' = o.id::text
             WHERE x.type = 'order.created' AND o.cart_id = $1",
        )
        .bind(cart)
        .fetch_one(&self.db)
        .await
        .unwrap();
        [
            self.read("SELECT count(*) FROM orders WHERE cart_id = $1", cart)
                .await,
            self.read(
                "SELECT count(*) FROM payment_attempts a JOIN orders o ON o.id = a.order_id
                 WHERE o.cart_id = $1",
                cart,
            )
            .await,
            outbox,
            self.read(
                "SELECT count(*) FROM stock_movements m
                 JOIN orders o ON m.ref_type = 'order' AND m.ref_id = o.id::text
                 WHERE o.cart_id = $1",
                cart,
            )
            .await,
        ]
    }
}

const ONE_ORDER: [i64; 4] = [1, 1, 1, 1];

#[sqlx::test(migrations = "../../migrations")]
async fn lost_response_after_commit_returns_existing_order(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let k0 = c.started(&shop_token, None).await;
    let k1 = c.rotated(&k0, contact("jana@example.cz")).await;
    let k2 = c.rotated(&k1, addresses("Jana Nováková")).await;
    let view = c.ready(&k2, method).await;

    let body = place_body(&view, "Zvonit dvakrát");
    let (status, text, res) = c.place(&k2, "p-1", &body).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let k3 =
        replacement(&res).expect("the note replaced K2, and this answer never reaches the browser");
    let placed: Value = serde_json::from_str(&text).unwrap();

    let (status, text) = c.receipt(&k2).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the replaced capability finds its order: {text}"
    );
    let receipt: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(receipt["number"], placed["number"]);
    assert_eq!(receipt["email"], "jana@example.cz");
    assert!(
        !text.contains("Zvonit") && !text.contains("Nováková"),
        "the receipt holds no note or address: {text}"
    );
    assert!(
        !holds_a_capability(&text),
        "nor a capability or an order link: {text}"
    );

    let (status, text) = c.receipt(&k3).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        receipt,
        "K3 reads the same receipt"
    );
    c.assert_restart(&k2, "Nováková", "K2 opens no checkout")
        .await;
    let (status, text, res) = c.place(&k2, "p-1", &body).await;
    assert_eq!(
        (status, replacement(&res)),
        (StatusCode::NOT_FOUND, None),
        "K2 replays no placement: {text}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn placement_recovery_does_not_repeat_order(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;

    let p = c.placed(method, "Zvonit dvakrát").await;
    assert_ne!(p.presented, p.current, "the note replaced K2");
    for key in ["p-1", "p-2"] {
        let (status, text, _) = c.place(&p.presented, key, &p.body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "K2 with {key}: {text}");
    }
    let (status, text, _) = c.place(&p.current, "p-2", &p.body).await;
    assert_eq!(
        (status, code(&text)),
        (StatusCode::CONFLICT, json!("order_already_placed")),
        "K3 with a fresh key"
    );
    for _ in 0..2 {
        c.assert_receipts(&p).await;
    }
    assert_eq!(c.effects(p.cart).await, ONE_ORDER, "after a note placement");

    let p = c.placed(method, "").await;
    assert_eq!(p.presented, p.current, "no note, no replacement");
    let (status, text, _) = c.place(&p.presented, "p-2", &p.body).await;
    assert_eq!(
        (status, code(&text)),
        (StatusCode::CONFLICT, json!("order_already_placed")),
        "K with a fresh key"
    );
    for _ in 0..2 {
        c.assert_receipts(&p).await;
    }
    assert_eq!(
        c.effects(p.cart).await,
        ONE_ORDER,
        "after a placement without a note"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn placement_note_bypass_denied(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let p = c.placed(method, "Note A").await;
    let mut note_b = p.body.clone();
    note_b["notes"] = json!("Note B");

    for key in ["p-1", "p-2"] {
        let (status, text, res) = c.place(&p.presented, key, &note_b).await;
        assert_eq!(
            (status, replacement(&res)),
            (StatusCode::NOT_FOUND, None),
            "K2 with note B and {key}: {text}"
        );
    }
    for (key, expected) in [
        ("p-1", "idempotency_conflict"),
        ("p-2", "order_already_placed"),
    ] {
        let (status, text, res) = c.place(&p.current, key, &note_b).await;
        assert_eq!(
            (status, code(&text), replacement(&res)),
            (StatusCode::CONFLICT, json!(expected), None),
            "K3 with note B and {key}"
        );
    }
    let (status, text, replaced) = c.details(&p.current, contact("other@example.cz")).await;
    assert_eq!(
        (status, replaced),
        (StatusCode::NOT_FOUND, None),
        "K3 writes no details: {text}"
    );

    let shop_token = c.shop_cart().await;
    let s0 = c.started(&shop_token, None).await;
    let s1 = c.rotated(&s0, contact("jana@example.cz")).await;
    let s2 = c.rotated(&s1, addresses("Jana Nováková")).await;
    let (_, stale) = c.checkout(&s2).await;
    let view = c.ready(&s2, method).await;
    let (status, text, _) = c.place(&s0, "s-1", &place_body(&view, "Note C")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a capability the contact write rotated away places nothing: {text}"
    );
    let (status, text, res) = c.place(&s2, "s-2", &place_body(&stale, "Note C")).await;
    assert_eq!(
        (status, code(&text), replacement(&res)),
        (StatusCode::CONFLICT, json!("cart_changed"), None),
        "a stale version"
    );
    let (status, open) = c.checkout(&s2).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the refused checkout stays open: {open}"
    );

    let notes: Vec<Option<String>> = c
        .read(
            "SELECT array_agg(notes ORDER BY number) FROM orders WHERE tenant_id = $1",
            c.shop.tenant,
        )
        .await;
    assert_eq!(notes, [Some("Note A".to_owned())], "one order, with note A");
    for (what, k) in [("K2", &p.presented), ("K3", &p.current)] {
        let (status, text) = c.receipt(k).await;
        assert_eq!(status, StatusCode::OK, "{what}: {text}");
        assert!(!text.contains("Note A"), "{what}: {text}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_wrong_generation_denied(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let host = "shop-alias.localhost";
    sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, verified_at)
         VALUES ($1, $2, $3, now())",
    )
    .bind(host)
    .bind(c.shop.tenant)
    .bind(c.shop.cz)
    .execute(&c.runtime)
    .await
    .unwrap();
    let alias = commerce::checkout_access::ShopOrigin::parse(&format!("https://{host}")).unwrap();
    sqlx::query(
        "UPDATE platform.checkout_origins SET history = 'merchant_attested_never_served',
             attested_by = 'user-owner', attested_at = now(), attestation_version = 1
         WHERE origin = $1",
    )
    .bind(alias.as_str())
    .execute(&c.runtime)
    .await
    .unwrap();
    assert!(
        checkout_access::switch_mode(&c.runtime, c.shop.tenant, &alias, CheckoutMode::SameOrigin)
            .await
            .unwrap()
    );
    let elsewhere = Edge {
        origin: alias.as_str().to_owned(),
        ..c.edge.clone()
    };
    let method = c.placeable().await;

    let p = c.placed(method, "Zvonit").await;
    c.assert_receipts(&p).await;
    for k in [&p.presented, &p.current] {
        let (status, text, _) = elsewhere
            .call(Call::get("/storefront/v1/checkout/order"))
            .header("x-cart-token", k.clone())
            .send_text(&c.s)
            .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "another same-origin origin: {text}"
        );
    }
    let unknown = "0".repeat(64);
    for (what, k) in [
        ("K0", &p.rotated_away[0]),
        ("K1", &p.rotated_away[1]),
        ("an unknown value", &unknown),
    ] {
        c.assert_no_receipt(k, what).await;
    }
    c.assert_receipts(&p).await;

    c.switch(CheckoutMode::Isolated).await;
    c.assert_no_receipts(&p, "on an isolated origin").await;
    c.switch(CheckoutMode::SameOrigin).await;
    c.assert_no_receipts(&p, "after a mode switch").await;

    let p = c.placed(method, "Zvonit").await;
    c.assert_receipts(&p).await;
    c.add(&p.shop_token, 0).await;
    c.started(&p.shop_token, None).await;
    c.assert_no_receipts(&p, "after a later start of the same shop cart")
        .await;

    let p = c.placed(method, "Zvonit").await;
    c.assert_receipts(&p).await;
    let other = c.shop_cart().await;
    c.started(&other, Some(&p.presented)).await;
    c.assert_no_receipts(&p, "after a start from another shop cart presenting K")
        .await;

    let p = c.placed(method, "Zvonit").await;
    c.assert_receipts(&p).await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE carts SET last_activity_at = now() - interval '24 hours 1 minute' WHERE id = $1",
    )
    .bind(p.cart)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    c.assert_no_receipts(&p, "after 24 h idle").await;

    let p = c.placed(method, "Zvonit").await;
    c.assert_receipts(&p).await;
    c.add(&p.shop_token, 0).await;
    let mut held = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    let mut ctx = storefront::context(
        &mut held,
        &PublicUrls::default(),
        c.shop.cz,
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    ctx.origin = Some(c.shop.origin.clone());
    checkout_access::start(&mut held, &ctx, &p.shop_token, None)
        .await
        .unwrap();
    let held_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let queued = tokio::spawn({
        let (s, edge, k) = (c.s.clone(), c.edge.clone(), p.presented.clone());
        async move {
            edge.call(Call::get("/storefront/v1/checkout/order"))
                .header("x-cart-token", k)
                .send_text(&s)
                .await
                .0
        }
    });
    waiting_on(&c.db, held_pid).await;
    held.commit().await.unwrap();
    assert_eq!(
        queued.await.unwrap(),
        StatusCode::NOT_FOUND,
        "a receipt read queued behind a revoking start re-checks the committed row"
    );
}
