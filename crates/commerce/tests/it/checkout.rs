//! Checkout and order placement against a real Postgres as the runtime role (A10, A12, A13,
//! A15, A16, A20): the placement transaction, idempotency, the four races, timeouts, late
//! payments, guest linking and tenant isolation.
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use commerce::cart::{self, CartRef, NewLine, Scope};
use commerce::checkout::{
    self, AddressesInput, CheckoutAddress, ContactInput, PaymentInput, PlaceOrderInput, Placement,
    Placer, Settings, ShippingInput,
};
use commerce::inventory::{self, Adjustment};
use commerce::orders::{self, PickupPoint};
use commerce::payments::{self, FakeGateway, MethodKind, Outcome, PaymentMethodInput, Payments};
use commerce::promotions::coupons::{self, CouponInput};
use commerce::shipping::{self, Carrier, ShippingMethodInput};
use commerce::storefront::{self, Context, PublicUrls};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const ACTOR: &str = "test";

fn settings() -> Settings {
    Settings {
        payments: Payments {
            fake: Some(FakeGateway::new(b"test-secret".to_vec())),
            ..Payments::default()
        },
        packeta: None,
    }
}

async fn ctx(tx: &mut TenantTx, market: Uuid) -> Context {
    storefront::context(tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap()
}

/// Shipping and payment methods of the CZ market: Packeta pickup (79 Kč, free over 1 500 Kč,
/// COD +39 Kč), PPL home (99 Kč, no COD); fake and COD payments.
struct Methods {
    pickup: Uuid,
    home: Uuid,
}

async fn methods(runtime: &PgPool, shop: &Shop) -> Methods {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let mut make = async |carrier, price, cod| {
        shipping::create(
            &mut tx,
            ACTOR,
            &ShippingMethodInput {
                market_id: shop.cz,
                carrier,
                name_i18n: [("cs".to_owned(), format!("{carrier:?}"))].into(),
                description_i18n: Default::default(),
                price_minor: price,
                free_over_minor: Some(150_000),
                weight_tiers: vec![],
                cod_allowed: cod,
                cod_fee_minor: if cod { 3900 } else { 0 },
                active: true,
                position: 0,
            },
        )
        .await
        .unwrap()
        .id
    };
    let pickup = make(Carrier::PacketaPickup, 7900, true).await;
    let home = make(Carrier::Ppl, 9900, false).await;
    for kind in [MethodKind::Fake, MethodKind::Cod] {
        payments::configure(
            &mut tx,
            ACTOR,
            &settings().payments,
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
    Methods { pickup, home }
}

fn address(country: &str) -> CheckoutAddress {
    CheckoutAddress {
        name: "Jana Nováková".into(),
        company: None,
        street: "Dlouhá 12".into(),
        city: "Praha".into(),
        postal_code: "110 00".into(),
        country: country.into(),
        phone: None,
    }
}

fn point() -> PickupPoint {
    PickupPoint {
        id: "1234".into(),
        name: "Z-BOX Praha 1".into(),
        street: "Dlouhá 1".into(),
        city: "Praha".into(),
        zip: "110 00".into(),
        country: "CZ".into(),
    }
}

/// A handed-off cart with `qty` of variant 0, ready to place (pickup point, fake payment).
/// Returns the checkout capability.
async fn ready_cart(runtime: &PgPool, shop: &Shop, m: &Methods, qty: i32, email: &str) -> String {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let (_, token) = cart::create(&mut tx, &ctx).await.unwrap();
    let c = cart::find(&mut tx, &ctx, &token, None).await.unwrap();
    cart::add_line(
        &mut tx,
        &ctx,
        &c,
        &NewLine {
            variant_id: shop.variants[0],
            quantity: qty,
        },
    )
    .await
    .unwrap();
    let h = cart::start_handoff(&mut tx, &c).await.unwrap();
    let checkout = cart::redeem_handoff(&mut tx, shop.cz, &h)
        .await
        .unwrap()
        .unwrap();
    let c = cart::find(&mut tx, &ctx, &checkout, Some(Scope::Checkout))
        .await
        .unwrap();
    checkout::set_contact(
        &mut tx,
        &c,
        &ContactInput {
            email: email.into(),
            phone: Some("+420 777 123 456".into()),
        },
    )
    .await
    .unwrap();
    checkout::set_addresses(
        &mut tx,
        &ctx,
        &c,
        &AddressesInput {
            billing: address("CZ"),
            shipping: None,
        },
    )
    .await
    .unwrap();
    checkout::set_shipping(
        &mut tx,
        &ctx,
        &c,
        &ShippingInput {
            method_id: m.pickup,
            pickup_point: Some(point()),
        },
    )
    .await
    .unwrap();
    checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput {
            method: MethodKind::Fake,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    checkout
}

async fn cart_ref(tx: &mut TenantTx, ctx: &Context, token: &str) -> CartRef {
    cart::find(tx, ctx, token, Some(Scope::Checkout))
        .await
        .unwrap()
}

/// What the checkout page would send: the current version and total.
async fn summary(runtime: &PgPool, shop: &Shop, token: &str) -> PlaceOrderInput {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let c = cart_ref(&mut tx, &ctx, token).await;
    let v = checkout::view(&mut tx, &ctx, &settings(), &c)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(v.missing.is_empty(), "{:?}", v.missing);
    PlaceOrderInput {
        version: v.cart.version,
        total_minor: v.totals.total.amount_minor,
        accept_terms: true,
        accept_withdrawal: true,
        email_marketing: false,
        review_invites: false,
        notes: None,
    }
}

async fn place(
    runtime: &PgPool,
    shop: &Shop,
    token: &str,
    key: &str,
    input: &PlaceOrderInput,
) -> Result<Placement, Error> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let hash = serde_json::to_string(input).unwrap();
    let placed = checkout::place_order(
        &mut tx,
        &ctx,
        &settings(),
        token,
        key,
        &hash,
        input,
        &Placer::default(),
    )
    .await?;
    tx.commit().await.unwrap();
    Ok(placed)
}

/// A count inside the tenant's RLS scope (`$1` is the tenant id).
async fn count(runtime: &PgPool, sql: &'static str, tenant: Uuid) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar(sql)
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

/// Outbox events of a type (the queue schema is not reachable by the runtime role, A8).
async fn outbox(owner: &PgPool, kind: &str, tenant: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = $2")
        .bind(tenant)
        .bind(kind)
        .fetch_one(owner)
        .await
        .unwrap()
}

async fn level(runtime: &PgPool, shop: &Shop, variant: Uuid) -> inventory::Level {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    inventory::get(&mut tx, variant).await.unwrap()
}

fn code(e: &Error) -> &'static str {
    e.code()
}

#[sqlx::test(migrations = "../../migrations")]
async fn placement_persists_the_priced_order(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "place").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 2, "Jana@Example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    // 2 × 129 Kč + Packeta 79 Kč.
    assert_eq!(input.total_minor, 2 * 12_900 + 7900);

    let placed = place(&runtime, &shop, &token, "k1", &input).await.unwrap();
    assert!(!placed.replayed);
    assert_eq!(placed.number, "100001");

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let o = checkout::order_by_token(&mut tx, &placed.token)
        .await
        .unwrap();
    assert_eq!(o.id, placed.order_id);
    assert_eq!(o.email, "jana@example.test");
    assert_eq!(o.status, orders::status::OrderStatus::Pending);
    assert_eq!(o.total.amount_minor, input.total_minor);
    assert_eq!(o.shipping.pickup_point, Some(point()));
    assert_eq!(o.lines.len(), 1);
    assert_eq!(o.payment.attempt.as_ref().unwrap().id, placed.attempt_id);
    assert!(!o.payment.can_retry, "an attempt is open");

    // A15: the persisted allocations add up to the order totals.
    let (lines_total, lines_tax): (i64, i64) = sqlx::query_as(
        "SELECT sum(total_minor)::bigint, sum(tax_minor)::bigint FROM order_lines WHERE order_id = $1",
    )
    .bind(o.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let (charges_total, charges_tax): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(sum(total_minor), 0)::bigint, coalesce(sum(tax_minor), 0)::bigint
         FROM order_charges WHERE order_id = $1",
    )
    .bind(o.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(lines_total + charges_total, o.total.amount_minor);
    assert_eq!(lines_tax + charges_tax, o.vat_total.amount_minor);
    assert_eq!(
        o.subtotal.amount_minor - o.discount.amount_minor
            + o.shipping_total.amount_minor
            + o.payment_fee.amount_minor
            + o.rounding.amount_minor,
        o.total.amount_minor
    );
    tx.commit().await.unwrap();

    // A13: reserved; the cart is converted; outbox + confirmation email in the same commit.
    let l = level(&runtime, &shop, shop.variants[0]).await;
    assert_eq!((l.on_hand, l.reserved), (10, 2));
    assert_eq!(
        count(
            &runtime,
            "SELECT count(*) FROM carts WHERE tenant_id = $1 AND status = 'converted'",
            shop.tenant
        )
        .await,
        1
    );
    assert_eq!(outbox(&db, "order.created", shop.tenant).await, 1);
    assert_eq!(
        count(&runtime, "SELECT count(*) FROM email_messages WHERE tenant_id = $1 AND template = 'order_confirmation' AND sensitive", shop.tenant).await,
        1
    );
    // The token is stored only hashed.
    assert_eq!(
        count(
            &runtime,
            "SELECT count(*) FROM order_tokens WHERE tenant_id = $1 AND length(token_hash) = 32",
            shop.tenant
        )
        .await,
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_retry_with_the_same_key_replays_with_a_fresh_token(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "replay").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let first = place(&runtime, &shop, &token, "same", &input)
        .await
        .unwrap();
    let again = place(&runtime, &shop, &token, "same", &input)
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.order_id, first.order_id);
    assert_eq!(again.attempt_id, first.attempt_id);
    assert_ne!(again.token, first.token);
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    for t in [&first.token, &again.token] {
        assert_eq!(
            checkout::order_by_token(&mut tx, t).await.unwrap().id,
            first.order_id
        );
    }
    tx.commit().await.unwrap();

    // Same key, other request: 409; another key: one order per cart.
    let other = PlaceOrderInput {
        email_marketing: true,
        ..input.clone()
    };
    let e = place(&runtime, &shop, &token, "same", &other)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "idempotency_conflict");
    let e = place(&runtime, &shop, &token, "second-tab", &input)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "order_already_placed");
    assert_eq!(
        count(
            &runtime,
            "SELECT count(*) FROM orders WHERE tenant_id = $1",
            shop.tenant
        )
        .await,
        1
    );
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn double_submit_and_two_tabs_race_to_one_order(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "tabs").await;
    let m = methods(&runtime, &shop).await;

    // Double submit: the same key twice at once.
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let (a, b) = tokio::join!(
        place(&runtime, &shop, &token, "dbl", &input),
        place(&runtime, &shop, &token, "dbl", &input)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.order_id, b.order_id);
    assert!(a.replayed != b.replayed);

    // Two tabs: two keys at once.
    let token = ready_cart(&runtime, &shop, &m, 1, "b@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let (a, b) = tokio::join!(
        place(&runtime, &shop, &token, "tab-1", &input),
        place(&runtime, &shop, &token, "tab-2", &input)
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let err = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(code(err), "order_already_placed");
    assert_eq!(
        count(
            &runtime,
            "SELECT count(*) FROM orders WHERE tenant_id = $1",
            shop.tenant
        )
        .await,
        2
    );
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_last_unit_goes_to_one_order(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "last").await;
    let m = methods(&runtime, &shop).await;
    let t1 = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let t2 = ready_cart(&runtime, &shop, &m, 1, "b@example.test").await;
    let (i1, i2) = (
        summary(&runtime, &shop, &t1).await,
        summary(&runtime, &shop, &t2).await,
    );
    // Now only one unit is left (both carts were filled while there were ten).
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    inventory::adjust(
        &mut tx,
        ACTOR,
        shop.variants[0],
        "to-one",
        &Adjustment {
            delta: None,
            on_hand: Some(1),
            note: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (a, b) = tokio::join!(
        place(&runtime, &shop, &t1, "k", &i1),
        place(&runtime, &shop, &t2, "k", &i2)
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let err = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert!(
        ["insufficient_stock", "cart_unavailable"].contains(&code(err)),
        "{err:?}"
    );
    let l = level(&runtime, &shop, shop.variants[0]).await;
    assert_eq!((l.on_hand, l.reserved), (1, 1));
    assert_eq!(
        count(
            &runtime,
            "SELECT count(*) FROM orders WHERE tenant_id = $1",
            shop.tenant
        )
        .await,
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_single_use_coupon_is_redeemed_once(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "coupon").await;
    let m = methods(&runtime, &shop).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    coupons::create(
        &mut tx,
        ACTOR,
        &CouponInput {
            code: "JEDNOU".into(),
            discount: commerce::pricing::cart::CouponDiscount::Percent { basis_points: 1000 },
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: Some(1),
            per_customer_limit: None,
            published: false,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tokens = Vec::new();
    for email in ["a@example.test", "b@example.test"] {
        let t = ready_cart(&runtime, &shop, &m, 1, email).await;
        let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
        let ctx = ctx(&mut tx, shop.cz).await;
        // Coupons are applied on the shop capability; attach directly for the checkout cart.
        let c = cart_ref(&mut tx, &ctx, &t).await;
        sqlx::query(
            "INSERT INTO cart_coupons (tenant_id, cart_id, coupon_id)
             SELECT $1, $2, id FROM coupons WHERE code = 'JEDNOU'",
        )
        .bind(shop.tenant)
        .bind(c.id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        tokens.push(t);
    }
    let i1 = summary(&runtime, &shop, &tokens[0]).await;
    let i2 = summary(&runtime, &shop, &tokens[1]).await;
    // 10 % off 129 Kč = 116.10 Kč + 79 Kč shipping.
    assert_eq!(i1.total_minor, 11_610 + 7900);
    let (a, b) = tokio::join!(
        place(&runtime, &shop, &tokens[0], "k", &i1),
        place(&runtime, &shop, &tokens[1], "k", &i2)
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let err = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert_eq!(code(err), "coupon_exhausted");
    // The loser rolled back completely: its stock is not reserved.
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 1);
    assert_eq!(
        count(
            &runtime,
            "SELECT used_count::bigint FROM coupons WHERE tenant_id = $1",
            shop.tenant
        )
        .await,
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_summaries_and_missing_consent_are_refused(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "stale").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;

    let e = place(
        &runtime,
        &shop,
        &token,
        "a",
        &PlaceOrderInput {
            accept_withdrawal: false,
            ..input.clone()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "legal_consent_required");
    let e = place(
        &runtime,
        &shop,
        &token,
        "b",
        &PlaceOrderInput {
            total_minor: input.total_minor - 1,
            ..input.clone()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "price_changed");

    // Another tab switched to home delivery: the version moved on.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let c = cart_ref(&mut tx, &ctx, &token).await;
    checkout::set_shipping(
        &mut tx,
        &ctx,
        &c,
        &ShippingInput {
            method_id: m.home,
            pickup_point: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let e = place(&runtime, &shop, &token, "c", &input)
        .await
        .unwrap_err();
    assert_eq!(code(&e), "cart_changed");
    let fresh = summary(&runtime, &shop, &token).await;
    assert_eq!(fresh.total_minor, 12_900 + 9900);
    assert!(place(&runtime, &shop, &token, "d", &fresh).await.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn ship_to_pickup_and_cod_rules(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "rules").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let c = cart_ref(&mut tx, &ctx, &token).await;
    // A3: the CZ market ships to CZ only.
    let e = checkout::set_addresses(
        &mut tx,
        &ctx,
        &c,
        &AddressesInput {
            billing: address("CZ"),
            shipping: Some(address("DE")),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "ship_to_not_allowed");
    let e = checkout::set_shipping(
        &mut tx,
        &ctx,
        &c,
        &ShippingInput {
            method_id: m.pickup,
            pickup_point: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "pickup_point_required");
    let e = checkout::set_shipping(
        &mut tx,
        &ctx,
        &c,
        &ShippingInput {
            method_id: m.pickup,
            pickup_point: Some(PickupPoint {
                country: "SK".into(),
                ..point()
            }),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "ship_to_not_allowed");
    // COD is not possible with PPL here; switching to PPL drops a COD selection.
    checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput {
            method: MethodKind::Cod,
        },
    )
    .await
    .unwrap();
    checkout::set_shipping(
        &mut tx,
        &ctx,
        &c,
        &ShippingInput {
            method_id: m.home,
            pickup_point: None,
        },
    )
    .await
    .unwrap();
    let v = checkout::view(&mut tx, &ctx, &settings(), &c)
        .await
        .unwrap();
    assert_eq!(v.payment_method, None);
    assert_eq!(v.missing, vec!["payment_method".to_owned()]);
    let e = checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput {
            method: MethodKind::Cod,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "cod_not_allowed");
    // Stripe is configured nowhere and has no adapter yet (WP11).
    let e = checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput {
            method: MethodKind::Stripe,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "unknown_payment_method");
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn cash_on_delivery_is_confirmed_on_placement(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "cod").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let c = cart_ref(&mut tx, &ctx, &token).await;
    checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput {
            method: MethodKind::Cod,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let input = summary(&runtime, &shop, &token).await;
    assert_eq!(input.total_minor, 12_900 + 7900 + 3900, "COD fee");
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.status, orders::status::OrderStatus::Confirmed);
    assert_eq!(o.payment_fee.amount_minor, 3900);
    assert_eq!(o.payment.expires_at, None);
    assert!(!o.payment.can_retry);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_payment_retry_then_success(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "retry").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    let action = payments::init(
        &runtime,
        shop.tenant,
        &settings().payments,
        placed.attempt_id,
        &format!("/o/{}", placed.token),
    )
    .await
    .unwrap();
    assert!(matches!(action, payments::NextAction::Redirect { .. }));

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Failed, "fake")
        .await
        .unwrap();
    // Repeating an outcome is a no-op.
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Failed, "fake")
        .await
        .unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Failed);
    assert!(o.payment.can_retry);
    let second = payments::retry(&mut tx, &settings().payments, placed.order_id)
        .await
        .unwrap();
    assert_eq!(
        code(
            &payments::retry(&mut tx, &settings().payments, placed.order_id)
                .await
                .unwrap_err()
        ),
        "retry_not_allowed"
    );
    payments::apply_outcome(&mut tx, second, Outcome::Succeeded, "fake")
        .await
        .unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(o.status, orders::status::OrderStatus::Confirmed);
    assert_eq!(o.exception, None);
    tx.commit().await.unwrap();
    assert_eq!(outbox(&db, "order.paid", shop.tenant).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_payment_deadline_holds_before_the_expiry_job_runs(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "deadline").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 2, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET payment_expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(placed.order_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let e = payments::init(
        &runtime,
        shop.tenant,
        &settings().payments,
        placed.attempt_id,
        "/o/x",
    )
    .await
    .unwrap_err();
    assert_eq!(code(&e), "payment_window_closed");
    // A failure reported after the deadline: the order expires (committed), nothing else.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let a = payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Failed, "fake")
        .await
        .unwrap();
    assert_eq!(a.status, payments::AttemptStatus::Expired);
    tx.commit().await.unwrap();
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 0);
    // The provider confirms anyway: a late payment.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Succeeded, "fake")
        .await
        .unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(o.status, orders::status::OrderStatus::Cancelled);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(o.exception.as_deref(), Some("late_payment"));
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 0);
    let page = {
        let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
        orders::list(
            &mut tx,
            &orders::OrderFilter {
                exception: true,
                ..Default::default()
            },
            None,
            10,
        )
        .await
        .unwrap()
    };
    assert_eq!(page.items.len(), 1, "the refund work list");
}

#[sqlx::test(migrations = "../../migrations")]
async fn money_from_a_second_attempt_is_flagged_not_refused(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "dup").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &shop, &token).await;
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Failed, "fake")
        .await
        .unwrap();
    let second = payments::retry(&mut tx, &settings().payments, placed.order_id)
        .await
        .unwrap();
    // The first attempt's money arrives after all: paid, and the open second attempt closes.
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Succeeded, "fake")
        .await
        .unwrap();
    assert_eq!(
        payments::attempt(&mut tx, second).await.unwrap().status,
        payments::AttemptStatus::Expired
    );
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(o.exception, None);
    // A provider still reporting the second attempt paid: recorded and flagged for a refund.
    payments::apply_outcome(&mut tx, second, Outcome::Succeeded, "fake")
        .await
        .unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.exception.as_deref(), Some("duplicate_payment"));
    assert_eq!(o.status, orders::status::OrderStatus::Confirmed);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn expiry_releases_stock_and_a_late_success_is_an_exception(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "expire").await;
    let m = methods(&runtime, &shop).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    coupons::create(
        &mut tx,
        ACTOR,
        &CouponInput {
            code: "PODZIM".into(),
            discount: commerce::pricing::cart::CouponDiscount::Percent { basis_points: 500 },
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: Some(5),
            per_customer_limit: None,
            published: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = ready_cart(&runtime, &shop, &m, 3, "a@example.test").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, shop.cz).await;
    let c = cart_ref(&mut tx, &ctx, &token).await;
    sqlx::query(
        "INSERT INTO cart_coupons (tenant_id, cart_id, coupon_id)
         SELECT $1, $2, id FROM coupons WHERE code = 'PODZIM'",
    )
    .bind(shop.tenant)
    .bind(c.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let input = summary(&runtime, &shop, &token).await;
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 3);

    // Nothing is due yet.
    assert_eq!(
        checkout::expire_due(&runtime, &PublicUrls::default(), 100)
            .await
            .unwrap(),
        0
    );
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET payment_expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(placed.order_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        checkout::expire_due(&runtime, &PublicUrls::default(), 100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        checkout::expire_due(&runtime, &PublicUrls::default(), 100)
            .await
            .unwrap(),
        0,
        "once"
    );
    let l = level(&runtime, &shop, shop.variants[0]).await;
    assert_eq!((l.on_hand, l.reserved), (10, 0));
    assert_eq!(
        count(
            &runtime,
            "SELECT used_count::bigint FROM coupons WHERE tenant_id = $1",
            shop.tenant
        )
        .await,
        0
    );
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.status, orders::status::OrderStatus::Cancelled);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Expired);
    assert!(!o.payment.can_retry);

    // The money arrives anyway: recorded, flagged, stock untouched (A10).
    payments::apply_outcome(&mut tx, placed.attempt_id, Outcome::Succeeded, "fake")
        .await
        .unwrap();
    let o = orders::view(&mut tx, placed.order_id).await.unwrap();
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(o.status, orders::status::OrderStatus::Cancelled);
    assert_eq!(o.exception.as_deref(), Some("late_payment"));
    tx.commit().await.unwrap();
    assert_eq!(level(&runtime, &shop, shop.variants[0]).await.reserved, 0);
    assert_eq!(outbox(&db, "order.exception", shop.tenant).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn consents_customers_and_guest_linking(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "link").await;
    let m = methods(&runtime, &shop).await;
    let token = ready_cart(&runtime, &shop, &m, 1, "guest@example.test").await;
    let input = PlaceOrderInput {
        email_marketing: true,
        ..summary(&runtime, &shop, &token).await
    };
    let placed = place(&runtime, &shop, &token, "k", &input).await.unwrap();
    // A20: only the ticked purpose is recorded, for the email address.
    let rows: Vec<(String, String, bool, String)> = sqlx::query_as(
        "SELECT subject_type, purpose, granted, source FROM consent_records WHERE tenant_id = $1",
    )
    .bind(shop.tenant)
    .fetch_all(&mut *tenant_tx(&runtime, shop.tenant).await.unwrap())
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(
            "email".into(),
            "email_marketing".into(),
            true,
            "checkout".into()
        )]
    );

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let unverified: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale) VALUES ($1, 'guest@example.test', 'cs')
         RETURNING id",
    )
    .bind(shop.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    // A5: nothing is linked before the address is verified.
    assert_eq!(
        orders::link_guest_orders(&mut tx, unverified)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE customers SET email_verified_at = now() WHERE id = $1")
        .bind(unverified)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        orders::link_guest_orders(&mut tx, unverified)
            .await
            .unwrap(),
        1
    );
    let page = orders::list(
        &mut tx,
        &orders::OrderFilter {
            customer_id: Some(unverified),
            ..Default::default()
        },
        None,
        10,
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, placed.order_id);
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn orders_and_methods_are_tenant_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let a = testkit::storefront::shop(&runtime, "iso-a").await;
    let b = testkit::storefront::shop(&runtime, "iso-b").await;
    let m = methods(&runtime, &a).await;
    let token = ready_cart(&runtime, &a, &m, 1, "a@example.test").await;
    let input = summary(&runtime, &a, &token).await;
    let placed = place(&runtime, &a, &token, "k", &input).await.unwrap();

    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    for table in [
        "shipping_methods",
        "payment_methods",
        "orders",
        "order_tokens",
        "order_lines",
        "order_charges",
        "order_addresses",
        "order_events",
        "payment_attempts",
        "order_numbers",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
    }
    assert!(matches!(
        checkout::order_by_token(&mut tx, &placed.token).await,
        Err(Error::NotFound)
    ));
    // Writing into another tenant is refused by the policy.
    let e = sqlx::query(
        "INSERT INTO order_events (tenant_id, order_id, kind, actor) VALUES ($1, $2, 'x', 'y')",
    )
    .bind(a.tenant)
    .bind(placed.order_id)
    .execute(&mut *tx)
    .await;
    assert!(e.is_err());
}
