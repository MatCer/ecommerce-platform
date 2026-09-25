//! Payment adapters against a real Postgres as the runtime role (A10, A11, A16, A25): bank
//! transfer instructions and statement matching, reminders, the Fio API poller, Stripe
//! webhook verification and processing, refunds, cash-on-delivery collection with rounding,
//! and tenant isolation of the new tables. Tests marked "stripe-mock" call the local
//! stripe-mock (`STRIPE_MOCK_URL`, default `http://localhost:12111`).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use commerce::cart::{self, NewLine, Scope};
use commerce::checkout::{
    self, AddressesInput, CheckoutAddress, ContactInput, PaymentInput, PlaceOrderInput, Placement,
    Placer, Settings, ShippingInput,
};
use commerce::inventory;
use commerce::orders::{self, status::CodStatus};
use commerce::payments::bank::{
    self, BankAccountInput, ResolveAction, ResolveInput, TxReason, TxStatus,
};
use commerce::payments::cod::{self, CollectInput, Collector};
use commerce::payments::statements::{Statement, StatementLine};
use commerce::payments::stripe::{self, Processed, Stripe};
use commerce::payments::{self, MethodKind, NextAction, Outcome, PaymentMethodInput, Payments};
use commerce::pricing::cart::Tender;
use commerce::shipping::{self, Carrier, ShippingMethodInput};
use commerce::storefront::{self, Context, PublicUrls};
use platform::Error;
use platform::config::{StripeConfig, StripeMode};
use platform::crypto::SecretBox;
use platform::db::{TenantTx, tenant_tx};
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const ACTOR: &str = "test";
const KEY: [u8; 32] = [7; 32];
const CZ_IBAN: &str = "CZ6508000000192000145399";
const SK_IBAN: &str = "SK9611000000002918599669";

fn stripe_client() -> Stripe {
    Stripe::new(
        &StripeConfig {
            mode: StripeMode::Simulator,
            api_url: std::env::var("STRIPE_MOCK_URL")
                .unwrap_or_else(|_| "http://localhost:12111".into())
                .parse()
                .unwrap(),
            secret_key: "sk_test_x".into(),
            publishable_key: None,
            webhook_secret: "whsec_test_0123456789".into(),
        },
        reqwest::Client::new(),
    )
}

fn settings() -> Settings {
    Settings {
        payments: Payments {
            fake: None,
            stripe: Some(stripe_client()),
            secrets: Some(Arc::new(SecretBox::new(&KEY))),
        },
        packeta: None,
    }
}

#[derive(Clone, Copy)]
enum M {
    Cz,
    Sk,
}

struct Setup {
    shop: Shop,
    cz_home: Uuid,
    sk_home: Uuid,
    account: String,
}

impl Setup {
    fn market(&self, m: M) -> Uuid {
        match m {
            M::Cz => self.shop.cz,
            M::Sk => self.shop.sk,
        }
    }
}

async fn ctx(tx: &mut TenantTx, market: Uuid) -> Context {
    storefront::context(tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap()
}

/// A shop with home delivery (COD allowed) in CZ and SK, bank transfer, COD and Stripe
/// enabled everywhere, a receiving account per market and a ready Stripe account.
async fn setup(runtime: &PgPool, slug: &str) -> Setup {
    let shop = testkit::storefront::shop(runtime, slug).await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let mut home = async |market, price, fee| {
        shipping::create(
            &mut tx,
            ACTOR,
            &ShippingMethodInput {
                market_id: market,
                carrier: Carrier::PacketaHome,
                name_i18n: [("cs".to_owned(), "Domů".to_owned())].into(),
                description_i18n: Default::default(),
                price_minor: price,
                free_over_minor: None,
                weight_tiers: vec![],
                cod_allowed: true,
                cod_fee_minor: fee,
                active: true,
                position: 0,
            },
        )
        .await
        .unwrap()
        .id
    };
    let cz_home = home(shop.cz, 7900, 3900).await;
    let sk_home = home(shop.sk, 390, 100).await;
    let account = format!("acct_test{}", slug.replace('-', ""));
    sqlx::query(
        "INSERT INTO stripe_accounts (tenant_id, account_id, livemode, charges_enabled,
             details_submitted, card_payments)
         VALUES ($1, $2, false, true, true, 'active')",
    )
    .bind(shop.tenant)
    .bind(&account)
    .execute(&mut *tx)
    .await
    .unwrap();
    let s = settings();
    for (market, iban, bic) in [
        (shop.cz, CZ_IBAN, "GIBACZPX"),
        (shop.sk, SK_IBAN, "TATRSKBX"),
    ] {
        bank::configure_account(
            &mut tx,
            ACTOR,
            None,
            market,
            &BankAccountInput {
                iban: iban.into(),
                bic: Some(bic.into()),
                account_name: "Demo s.r.o.".into(),
                fio_token: None,
                clear_fio_token: false,
            },
        )
        .await
        .unwrap();
        for kind in [
            MethodKind::BankTransfer,
            MethodKind::Cod,
            MethodKind::Stripe,
        ] {
            payments::configure(
                &mut tx,
                ACTOR,
                &s.payments,
                market,
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
    }
    tx.commit().await.unwrap();
    Setup {
        shop,
        cz_home,
        sk_home,
        account,
    }
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

/// Places an order of one unit of variant 0 in `m`, paid with `kind`.
async fn place(runtime: &PgPool, s: &Setup, m: M, kind: MethodKind) -> Placement {
    let market = s.market(m);
    let mut tx = tenant_tx(runtime, s.shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, market).await;
    let (_, token) = cart::create(&mut tx, &ctx).await.unwrap();
    let c = cart::find(&mut tx, &ctx, &token, None).await.unwrap();
    cart::add_line(
        &mut tx,
        &ctx,
        &c,
        &NewLine {
            variant_id: s.shop.variants[0],
            quantity: 1,
        },
    )
    .await
    .unwrap();
    let h = cart::start_handoff(&mut tx, &c).await.unwrap();
    let checkout_token = cart::redeem_handoff(&mut tx, market, &h)
        .await
        .unwrap()
        .unwrap();
    let c = cart::find(&mut tx, &ctx, &checkout_token, Some(Scope::Checkout))
        .await
        .unwrap();
    checkout::set_contact(
        &mut tx,
        &c,
        &ContactInput {
            email: "jana@example.test".into(),
            phone: None,
        },
    )
    .await
    .unwrap();
    let country = match m {
        M::Cz => "CZ",
        M::Sk => "SK",
    };
    checkout::set_addresses(
        &mut tx,
        &ctx,
        &c,
        &AddressesInput {
            billing: address(country),
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
            method_id: match m {
                M::Cz => s.cz_home,
                M::Sk => s.sk_home,
            },
            pickup_point: None,
        },
    )
    .await
    .unwrap();
    checkout::set_payment(
        &mut tx,
        &ctx,
        &settings(),
        &c,
        &PaymentInput { method: kind },
    )
    .await
    .unwrap();
    let c = cart::find(&mut tx, &ctx, &checkout_token, Some(Scope::Checkout))
        .await
        .unwrap();
    let v = checkout::view(&mut tx, &ctx, &settings(), &c)
        .await
        .unwrap();
    assert!(v.missing.is_empty(), "{:?}", v.missing);
    let input = PlaceOrderInput {
        version: v.cart.version,
        total_minor: v.totals.total.amount_minor,
        accept_terms: true,
        accept_withdrawal: true,
        email_marketing: false,
        review_invites: false,
        notes: None,
    };
    let placed = checkout::place_order(
        &mut tx,
        &ctx,
        &settings(),
        &checkout_token,
        &Uuid::now_v7().to_string(),
        "hash",
        &input,
        &Placer::default(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    placed
}

async fn view(runtime: &PgPool, tenant: Uuid, order: Uuid) -> orders::OrderView {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    orders::view(&mut tx, order).await.unwrap()
}

async fn total(runtime: &PgPool, tenant: Uuid, order: Uuid) -> i64 {
    view(runtime, tenant, order).await.total.amount_minor
}

async fn account_id(runtime: &PgPool, tenant: Uuid, market: Uuid) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    bank::account(&mut tx, market).await.unwrap().unwrap().id
}

fn line(id: &str, amount: i64, currency: &str, vs: Option<&str>) -> StatementLine {
    StatementLine {
        bank_tx_id: id.into(),
        booked_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        amount_minor: amount,
        currency: currency.into(),
        variable_symbol: vs.map(str::to_owned),
        counterparty: Some("123/0100".into()),
        counterparty_name: Some("Jan Novák".into()),
        message: None,
        raw: json!({}),
    }
}

async fn import(
    runtime: &PgPool,
    tenant: Uuid,
    account: Uuid,
    lines: Vec<StatementLine>,
) -> Result<bank::StatementImport, Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let r = bank::import(
        &mut tx,
        ACTOR,
        account,
        "camt053",
        &Statement {
            iban: None,
            domestic: None,
            lines,
        },
    )
    .await?;
    tx.commit().await.unwrap();
    Ok(r)
}

async fn count(runtime: &PgPool, tenant: Uuid, sql: &'static str) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap()
}

// ---------------------------------------------------------------------------------------
// Bank transfer

#[sqlx::test(migrations = "../../migrations")]
async fn bank_transfer_orders_get_a_variable_symbol_and_qr(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "bank-qr").await;

    let cz = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let o = view(&runtime, s.shop.tenant, cz.order_id).await;
    assert_eq!(o.status, orders::status::OrderStatus::Pending);
    let b = o.payment.bank_transfer.expect("instructions");
    assert_eq!(b.variable_symbol, cz.number, "VS = order number (A25)");
    assert_eq!(b.iban, CZ_IBAN);
    assert_eq!(b.amount.amount_minor, o.total.amount_minor);
    assert_eq!(b.qr_kind, Some(payments::qr::QrKind::Spayd));
    let svg = b.qr_svg.unwrap();
    assert!(svg.starts_with("<svg role=\"img\""));

    let sk = place(&runtime, &s, M::Sk, MethodKind::BankTransfer).await;
    let b = view(&runtime, s.shop.tenant, sk.order_id)
        .await
        .payment
        .bank_transfer
        .unwrap();
    assert_eq!(b.qr_kind, Some(payments::qr::QrKind::PayBySquare));
    assert_eq!(b.iban, SK_IBAN);

    // The stored payload is what the QR carries.
    let mut tx = tenant_tx(&runtime, s.shop.tenant).await.unwrap();
    let payload: String = sqlx::query_scalar(
        "SELECT instructions->>'qr_payload' FROM payment_attempts WHERE id = $1",
    )
    .bind(cz.attempt_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        payload.starts_with(&format!("SPD*1.0*ACC:{CZ_IBAN}+GIBACZPX*AM:")),
        "{payload}"
    );
    assert!(payload.contains(&format!("*X-VS:{}", cz.number)));

    // A25: the VS is unique per tenant and receiving account; another account may reuse it.
    let cz2 = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let reuse = async |attempt: Uuid| {
        let mut tx = tenant_tx(&runtime, s.shop.tenant).await.unwrap();
        let r = sqlx::query("UPDATE payment_attempts SET variable_symbol = $2 WHERE id = $1")
            .bind(attempt)
            .bind(&cz.number)
            .execute(&mut *tx)
            .await;
        tx.rollback().await.unwrap();
        r.is_ok()
    };
    assert!(!reuse(cz2.attempt_id).await, "same account");
    assert!(reuse(sk.attempt_id).await, "another account");

    // Init answers "nothing to pay online" (the page shows the instructions).
    let action = payments::init(
        &runtime,
        s.shop.tenant,
        &settings().payments,
        cz.attempt_id,
        "/o/x",
    )
    .await
    .unwrap();
    assert_eq!(action, NextAction::None);

    // No receiving account → bank transfer is not offered.
    let other = setup_without_bank(&runtime).await;
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    let m = payments::methods(&mut tx, &settings().payments, other.cz)
        .await
        .unwrap();
    let bt = m
        .iter()
        .find(|m| m.kind == MethodKind::BankTransfer)
        .unwrap();
    assert!(!bt.available);
    assert_eq!(bt.unavailable_reason.as_deref(), Some("no_bank_account"));
}

async fn setup_without_bank(runtime: &PgPool) -> Shop {
    testkit::storefront::shop(runtime, "no-bank").await
}

#[sqlx::test(migrations = "../../migrations")]
async fn statements_match_by_vs_amount_and_currency_once(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "match").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let b = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let (ta, tb) = (
        total(&runtime, t, a.order_id).await,
        total(&runtime, t, b.order_id).await,
    );
    let cz = account_id(&runtime, t, s.shop.cz).await;

    let lines = vec![
        line("T1", ta, "CZK", Some(&a.number)),
        line("T2", tb - 100, "CZK", Some(&b.number)),
        line("T3", 5000, "CZK", Some("999999")),
        line("T4", 5000, "CZK", None),
        line("T5", ta, "CZK", Some(&a.number)),
        line("T6", tb, "EUR", Some(&b.number)),
        line("T7", -20_000, "CZK", None),
    ];
    let r = import(&runtime, t, cz, lines.clone()).await.unwrap();
    assert_eq!(
        (r.imported, r.duplicates, r.debits, r.matched, r.exceptions),
        (6, 0, 1, 1, 5)
    );
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(o.status, orders::status::OrderStatus::Confirmed);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(
        view(&runtime, t, b.order_id).await.payment.status,
        orders::status::PaymentStatus::Unpaid,
        "a short payment does not pay"
    );

    // A25: a second import of the same statement changes nothing.
    let again = import(&runtime, t, cz, lines).await.unwrap();
    assert_eq!((again.imported, again.duplicates, again.matched), (0, 6, 0));

    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let open = bank::transactions(
        &mut tx,
        &bank::TxFilter {
            open: true,
            ..Default::default()
        },
        None,
        50,
    )
    .await
    .unwrap()
    .items;
    tx.commit().await.unwrap();
    let by = |id: &str| open.iter().find(|x| x.bank_tx_id == id).unwrap();
    assert_eq!(
        (by("T2").status, by("T2").reason),
        (TxStatus::Partial, Some(TxReason::AmountShort))
    );
    assert_eq!(by("T2").expected_minor, Some(tb));
    assert_eq!(by("T2").order_number.as_deref(), Some(b.number.as_str()));
    assert_eq!(by("T3").reason, Some(TxReason::UnknownVariableSymbol));
    assert_eq!(by("T4").reason, Some(TxReason::NoVariableSymbol));
    assert_eq!(by("T5").reason, Some(TxReason::AlreadyPaid));
    assert_eq!(by("T6").reason, Some(TxReason::CurrencyMismatch));

    // Resolution: accept the short payment, dismiss (a note is required), assign.
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let accepted = bank::resolve(
        &mut tx,
        ACTOR,
        by("T2").id,
        &ResolveInput {
            action: ResolveAction::Accept,
            order_number: None,
            note: Some("100 Kč short, customer pays the rest in cash".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(accepted.status, TxStatus::Matched);
    let no_note = bank::resolve(
        &mut tx,
        ACTOR,
        by("T3").id,
        &ResolveInput {
            action: ResolveAction::Dismiss,
            order_number: None,
            note: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(no_note.code(), "invalid_resolution");
    bank::resolve(
        &mut tx,
        ACTOR,
        by("T3").id,
        &ResolveInput {
            action: ResolveAction::Dismiss,
            order_number: None,
            note: Some("returned to the sender".into()),
        },
    )
    .await
    .unwrap();
    let again = bank::resolve(
        &mut tx,
        ACTOR,
        by("T3").id,
        &ResolveInput {
            action: ResolveAction::Dismiss,
            order_number: None,
            note: Some("x".into()),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(again.code(), "transaction_resolved");
    tx.commit().await.unwrap();
    assert_eq!(
        view(&runtime, t, b.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
    assert_eq!(
        count(
            &runtime,
            t,
            "SELECT count(*) FROM audit_log WHERE action = 'bank_transaction.resolved'"
        )
        .await,
        2
    );

    // Assign: a transfer without VS for a third order (50 Kč: a partial payment of it, which
    // is then accepted explicitly).
    let c = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let assigned = bank::resolve(
        &mut tx,
        ACTOR,
        by("T4").id,
        &ResolveInput {
            action: ResolveAction::Assign,
            order_number: Some(c.number.clone()),
            note: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(assigned.status, TxStatus::Partial);
    bank::resolve(
        &mut tx,
        ACTOR,
        by("T4").id,
        &ResolveInput {
            action: ResolveAction::Accept,
            order_number: None,
            note: Some("the rest arrives in cash".into()),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        view(&runtime, t, c.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn matching_is_scoped_to_the_tenant_and_the_receiving_account(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "scope-a").await;
    let other = setup(&runtime, "scope-b").await;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, s.shop.tenant, a.order_id).await;

    // The same VS arriving on the SK account of the same tenant pays nothing.
    let sk = account_id(&runtime, s.shop.tenant, s.shop.sk).await;
    let r = import(
        &runtime,
        s.shop.tenant,
        sk,
        vec![line("X1", amount, "CZK", Some(&a.number))],
    )
    .await
    .unwrap();
    assert_eq!((r.matched, r.exceptions), (0, 1));

    // Nor on another tenant's account.
    let other_cz = account_id(&runtime, other.shop.tenant, other.shop.cz).await;
    let r = import(
        &runtime,
        other.shop.tenant,
        other_cz,
        vec![line("X1", amount, "CZK", Some(&a.number))],
    )
    .await
    .unwrap();
    assert_eq!(r.matched, 0);
    assert_eq!(
        view(&runtime, s.shop.tenant, a.order_id)
            .await
            .payment
            .status,
        orders::status::PaymentStatus::Unpaid
    );

    // Another tenant cannot import into this tenant's account.
    let cz = account_id(&runtime, s.shop.tenant, s.shop.cz).await;
    let err = import(&runtime, other.shop.tenant, cz, vec![])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");

    // A statement for another IBAN is refused.
    let mut tx = tenant_tx(&runtime, s.shop.tenant).await.unwrap();
    let err = bank::import(
        &mut tx,
        ACTOR,
        cz,
        "camt053",
        &Statement {
            iban: Some(SK_IBAN.into()),
            domestic: None,
            lines: vec![],
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "statement_account_mismatch");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_transfer_after_expiry_is_a_late_payment_without_restock(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "late").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, t, a.order_id).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE orders SET payment_expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(a.order_id)
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
    let level = async || {
        let mut tx = tenant_tx(&runtime, t).await.unwrap();
        inventory::get(&mut tx, s.shop.variants[0]).await.unwrap()
    };
    let released = level().await;
    assert_eq!(released.reserved, 0);

    let cz = account_id(&runtime, t, s.shop.cz).await;
    let r = import(
        &runtime,
        t,
        cz,
        vec![line("L1", amount, "CZK", Some(&a.number))],
    )
    .await
    .unwrap();
    assert_eq!(r.matched, 1, "the money is recorded, never refused");
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(o.status, orders::status::OrderStatus::Cancelled);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    assert_eq!(o.exception.as_deref(), Some("late_payment"));
    assert_eq!(level().await, released, "no silent restock (A10)");

    // The exception waits in the work list until someone resolves it.
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let list = orders::list(
        &mut tx,
        &orders::OrderFilter {
            exception: true,
            ..Default::default()
        },
        None,
        10,
    )
    .await
    .unwrap();
    assert_eq!(list.items.len(), 1);
    orders::resolve_exception(&mut tx, ACTOR, a.order_id, "refunded to the sender")
        .await
        .unwrap();
    let err = orders::resolve_exception(&mut tx, ACTOR, a.order_id, "again")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "no_open_exception");
    let list = orders::list(
        &mut tx,
        &orders::OrderFilter {
            exception: true,
            ..Default::default()
        },
        None,
        10,
    )
    .await
    .unwrap();
    assert!(list.items.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn reminders_on_day_three_and_six_while_the_window_is_open(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "remind").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let urls = PublicUrls::default();
    let age = async |days: i64| {
        let mut tx = tenant_tx(&runtime, t).await.unwrap();
        sqlx::query("UPDATE payment_attempts SET created_at = now() - make_interval(days => $2::int) - interval '1 minute' WHERE id = $1")
            .bind(a.attempt_id)
            .bind(i32::try_from(days).unwrap())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    };
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        0
    );
    age(3).await;
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        0,
        "once"
    );
    age(6).await;
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        count(
            &runtime,
            t,
            "SELECT count(*) FROM email_messages WHERE template = 'payment_reminder' AND sensitive"
        )
        .await,
        2
    );
    let body: String = {
        let mut tx = tenant_tx(&runtime, t).await.unwrap();
        sqlx::query_scalar(
            "SELECT body_text FROM email_messages WHERE template = 'payment_reminder' LIMIT 1",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
    };
    assert!(body.contains(&a.number) && body.contains(CZ_IBAN), "{body}");

    // A paid or expired order gets no reminder.
    let b = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE payment_attempts SET created_at = now() - interval '4 days', expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(b.attempt_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        checkout::send_payment_reminders(&runtime, &urls, 10)
            .await
            .unwrap(),
        0
    );
}

/// A one-shot HTTP stub for the Fio API: answers `body` to one request and reports the path.
async fn fio_stub(body: String) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let res = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(res.as_bytes()).await.unwrap();
        req.lines().next().unwrap_or_default().to_owned()
    });
    (url, handle)
}

#[sqlx::test(migrations = "../../migrations")]
async fn fio_polling_imports_with_the_encrypted_token(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "fio").await;
    let t = s.shop.tenant;
    let secrets = SecretBox::new(&KEY);
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let acc = bank::configure_account(
        &mut tx,
        ACTOR,
        Some(&secrets),
        s.shop.cz,
        &BankAccountInput {
            iban: CZ_IBAN.into(),
            bic: None,
            account_name: "Demo s.r.o.".into(),
            fio_token: Some("fioToken0123456789".into()),
            clear_fio_token: false,
        },
    )
    .await
    .unwrap();
    assert!(acc.fio_connected);
    let stored: Vec<u8> = sqlx::query_scalar("SELECT fio_token FROM bank_accounts WHERE id = $1")
        .bind(acc.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains("fioToken"),
        "encrypted at rest"
    );
    tx.commit().await.unwrap();

    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, t, a.order_id).await;
    let body = json!({ "accountStatement": {
        "info": { "iban": CZ_IBAN, "currency": "CZK" },
        "transactionList": { "transaction": [{
            "column22": { "value": 777_000_001_u64, "name": "ID pohybu", "id": 22 },
            "column0": { "value": "2026-09-25+0200", "name": "Datum", "id": 0 },
            "column1": { "value": amount as f64 / 100.0, "name": "Objem", "id": 1 },
            "column14": { "value": "CZK", "name": "Měna", "id": 14 },
            "column5": { "value": a.number, "name": "VS", "id": 5 },
        }]}
    }});
    let accounts = bank::fio_accounts(&runtime).await.unwrap();
    let mine = accounts.iter().find(|x| x.tenant_id == t).unwrap();
    let (url, handle) = fio_stub(body.to_string()).await;
    let report = bank::poll_fio(
        &runtime,
        &reqwest::Client::new(),
        &secrets,
        &url,
        mine,
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(report.matched, 1);
    let request = handle.await.unwrap();
    assert!(
        request.starts_with("GET /v1/rest/periods/fioToken0123456789/"),
        "{request}"
    );
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
    // Another key cannot decrypt it; the row binding protects copies.
    let other = SecretBox::new(&[0xab; 32]);
    let err = bank::poll_fio(
        &runtime,
        &reqwest::Client::new(),
        &other,
        &url,
        mine,
        Utc::now(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "internal_error");
}

// ---------------------------------------------------------------------------------------
// Stripe webhooks

fn event(id: &str, kind: &str, account: &str, livemode: bool, object: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": id, "object": "event", "type": kind, "livemode": livemode, "account": account,
        "created": Utc::now().timestamp(), "data": { "object": object },
    }))
    .unwrap()
}

fn intent(pi: &str, attempt: Uuid, amount: i64, currency: &str) -> Value {
    json!({
        "id": pi, "object": "payment_intent", "amount": amount, "amount_received": amount,
        "currency": currency, "metadata": { "attempt_id": attempt },
    })
}

/// Receives (signed) and processes an event like the webhook route + worker job.
async fn deliver(runtime: &PgPool, raw: &[u8]) -> Option<Processed> {
    let s = stripe_client();
    let sig = s.sign(raw, Utc::now()).unwrap();
    if !stripe::receive(runtime, &s, &sig, raw).await.unwrap() {
        return None;
    }
    let event_id = serde_json::from_slice::<Value>(raw).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let id: Uuid =
        sqlx::query_scalar("SELECT id FROM platform.provider_events WHERE event_id = $1")
            .bind(event_id)
            .fetch_one(runtime)
            .await
            .unwrap();
    Some(stripe::process_event(runtime, id).await.unwrap())
}

#[sqlx::test(migrations = "../../migrations")]
async fn stripe_events_are_verified_matched_and_never_regress(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "stripe-ev").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::Stripe).await;
    let amount = total(&runtime, t, a.order_id).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE payment_attempts SET provider_ref = 'pi_test_1' WHERE id = $1")
        .bind(a.attempt_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let acct = s.account.as_str();
    let pi = |amount: i64| intent("pi_test_1", a.attempt_id, amount, "czk");

    // Signatures: a tampered body or a wrong secret is refused before anything is stored.
    let raw = event("evt_1", "payment_intent.succeeded", acct, false, pi(amount));
    let sig = stripe_client().sign(&raw, Utc::now()).unwrap();
    let mut tampered = raw.clone();
    tampered.push(b' ');
    let err = stripe::receive(&runtime, &stripe_client(), &sig, &tampered)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_signature");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM platform.provider_events")
            .fetch_one(&runtime)
            .await
            .unwrap(),
        0
    );

    // A11 matching: amount, account, livemode, object, currency.
    let rejected = |p: Option<Processed>| matches!(p, Some(Processed::Rejected(_)));
    assert!(rejected(
        deliver(
            &runtime,
            &event(
                "evt_amt",
                "payment_intent.succeeded",
                acct,
                false,
                pi(amount - 1)
            )
        )
        .await
    ));
    assert!(rejected(
        deliver(
            &runtime,
            &event(
                "evt_acc",
                "payment_intent.succeeded",
                "acct_unknown",
                false,
                pi(amount)
            )
        )
        .await
    ));
    assert!(rejected(
        deliver(
            &runtime,
            &event(
                "evt_live",
                "payment_intent.succeeded",
                acct,
                true,
                pi(amount)
            )
        )
        .await
    ));
    assert!(rejected(
        deliver(
            &runtime,
            &event(
                "evt_obj",
                "payment_intent.succeeded",
                acct,
                false,
                intent("pi_other", a.attempt_id, amount, "czk")
            )
        )
        .await
    ));
    assert!(rejected(
        deliver(
            &runtime,
            &event(
                "evt_ccy",
                "payment_intent.succeeded",
                acct,
                false,
                intent("pi_test_1", a.attempt_id, amount, "eur")
            )
        )
        .await
    ));
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Unpaid
    );

    // The real success confirms the order; its redelivery is a no-op.
    assert!(matches!(
        deliver(&runtime, &raw).await,
        Some(Processed::Applied(_))
    ));
    assert_eq!(
        deliver(&runtime, &raw).await,
        None,
        "duplicate event ignored"
    );
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(o.status, orders::status::OrderStatus::Confirmed);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);

    // Out of order: a late failure never regresses `paid` (A11).
    let failed = deliver(
        &runtime,
        &event(
            "evt_fail",
            "payment_intent.payment_failed",
            acct,
            false,
            pi(amount),
        ),
    )
    .await;
    assert!(matches!(failed, Some(Processed::Ignored(_))), "{failed:?}");
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );

    // charge.refunded: partial, then full.
    let charge = |refunded: i64| {
        json!({ "id": "ch_1", "object": "charge", "payment_intent": "pi_test_1",
                "amount": amount, "amount_refunded": refunded, "currency": "czk" })
    };
    deliver(
        &runtime,
        &event("evt_r1", "charge.refunded", acct, false, charge(100)),
    )
    .await;
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::PartiallyRefunded
    );
    deliver(
        &runtime,
        &event("evt_r2", "charge.refunded", acct, false, charge(amount)),
    )
    .await;
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Refunded
    );

    // account.updated: losing card_payments hides Stripe at checkout (A11).
    let lost = json!({ "id": acct, "object": "account", "charges_enabled": true,
                       "details_submitted": true, "capabilities": { "card_payments": "inactive" } });
    assert!(matches!(
        deliver(
            &runtime,
            &event("evt_acct", "account.updated", acct, false, lost)
        )
        .await,
        Some(Processed::Applied(_))
    ));
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let m = payments::methods(&mut tx, &settings().payments, s.shop.cz)
        .await
        .unwrap();
    let stripe_m = m.iter().find(|m| m.kind == MethodKind::Stripe).unwrap();
    assert!(!stripe_m.available);
    assert_eq!(
        stripe_m.unavailable_reason.as_deref(),
        Some("stripe_onboarding")
    );
    // The outcome of every event is kept.
    let outcomes: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT event_id, outcome FROM platform.provider_events ORDER BY received_at",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert!(outcomes.iter().all(|(_, o)| o.is_some()), "{outcomes:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn stripe_failure_then_retry_then_success(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "stripe-retry").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::Stripe).await;
    let amount = total(&runtime, t, a.order_id).await;
    let acct = s.account.as_str();

    // An event that races the intent id being stored is retried (job backoff), never lost.
    let early = event(
        "evt_early",
        "payment_intent.payment_failed",
        acct,
        false,
        intent("pi_a", a.attempt_id, amount, "czk"),
    );
    let client = stripe_client();
    let sig = client.sign(&early, Utc::now()).unwrap();
    assert!(
        stripe::receive(&runtime, &client, &sig, &early)
            .await
            .unwrap()
    );
    let early_id: Uuid =
        sqlx::query_scalar("SELECT id FROM platform.provider_events WHERE event_id = 'evt_early'")
            .fetch_one(&runtime)
            .await
            .unwrap();
    let err = stripe::process_event(&runtime, early_id).await.unwrap_err();
    assert_eq!(err.code(), "provider_ref_pending");

    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE payment_attempts SET provider_ref = 'pi_a' WHERE id = $1")
        .bind(a.attempt_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // The retry applies it.
    assert!(matches!(
        stripe::process_event(&runtime, early_id).await.unwrap(),
        Processed::Applied(_)
    ));
    deliver(
        &runtime,
        &event(
            "evt_f",
            "payment_intent.payment_failed",
            acct,
            false,
            intent("pi_a", a.attempt_id, amount, "czk"),
        ),
    )
    .await;
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Failed
    );
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let retry = payments::retry(&mut tx, &settings().payments, a.order_id)
        .await
        .unwrap();
    sqlx::query("UPDATE payment_attempts SET provider_ref = 'pi_b' WHERE id = $1")
        .bind(retry)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        deliver(
            &runtime,
            &event(
                "evt_s",
                "payment_intent.succeeded",
                acct,
                false,
                intent("pi_b", retry, amount, "czk")
            )
        )
        .await,
        Some(Processed::Applied(_))
    ));
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
}

/// stripe-mock: a direct charge on the connected account, the simulator's signed event,
/// and a refund with the application fee.
#[sqlx::test(migrations = "../../migrations")]
async fn stripe_mock_intent_simulator_and_refund(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "stripe-mock").await;
    let t = s.shop.tenant;
    sqlx::query("UPDATE platform.tenants SET application_fee_bps = 150 WHERE id = $1")
        .bind(t)
        .execute(&db)
        .await
        .unwrap();
    let a = place(&runtime, &s, M::Cz, MethodKind::Stripe).await;
    let p = settings().payments;
    let action = payments::init(&runtime, t, &p, a.attempt_id, "/o/x")
        .await
        .unwrap();
    assert_eq!(action, NextAction::StripeSimulator);
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let first = payments::attempt(&mut tx, a.attempt_id)
        .await
        .unwrap()
        .provider_ref
        .unwrap();
    tx.commit().await.unwrap();
    assert!(first.starts_with("pi_"), "{first}");
    payments::init(&runtime, t, &p, a.attempt_id, "/o/x")
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let again = payments::attempt(&mut tx, a.attempt_id)
        .await
        .unwrap()
        .provider_ref
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(first, again, "init is idempotent");

    let stripe = p.stripe.as_ref().unwrap();
    stripe::simulate_payment(&runtime, stripe, t, a.attempt_id, Outcome::Succeeded)
        .await
        .unwrap();
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM platform.provider_events WHERE event_id LIKE 'evt_sim_%'",
    )
    .fetch_one(&runtime)
    .await
    .unwrap();
    assert!(matches!(
        stripe::process_event(&runtime, id).await.unwrap(),
        Processed::Applied(_)
    ));
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );

    let amount = total(&runtime, t, a.order_id).await;
    let r = payments::refund(&runtime, &p, t, a.attempt_id, 500, Some("damaged"), ACTOR)
        .await
        .unwrap();
    assert!(r.provider_ref.unwrap().starts_with("re_"));
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::PartiallyRefunded
    );
    let err = payments::refund(&runtime, &p, t, a.attempt_id, amount, None, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "nothing_to_refund");
    payments::refund(&runtime, &p, t, a.attempt_id, amount - 500, None, ACTOR)
        .await
        .unwrap();
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Refunded
    );
}

// ---------------------------------------------------------------------------------------
// Cash on delivery

async fn set_price(runtime: &PgPool, s: &Setup, czk: i64, eur: i64) {
    testkit::pricing::set_prices(
        runtime,
        s.shop.tenant,
        s.shop.czk,
        &[(s.shop.variants[0], czk)],
    )
    .await;
    testkit::pricing::set_prices(
        runtime,
        s.shop.tenant,
        s.shop.eur,
        &[(s.shop.variants[0], eur)],
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn cod_cash_is_rounded_at_collection_then_remitted(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "cod").await;
    let t = s.shop.tenant;
    set_price(&runtime, &s, 12_345, 523).await;

    let a = place(&runtime, &s, M::Cz, MethodKind::Cod).await;
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(
        o.status,
        orders::status::OrderStatus::Confirmed,
        "COD confirmed on placement"
    );
    assert_eq!(o.total.amount_minor, 12_345 + 7900 + 3900);

    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let d = cod::deliver(&mut tx, ACTOR, a.order_id).await.unwrap();
    assert_eq!(d.cod_status, Some(CodStatus::Delivered));
    let c = cod::collect(
        &mut tx,
        ACTOR,
        a.order_id,
        &CollectInput {
            tender: Tender::Cash,
            collector: Collector::Carrier,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // 241,45 Kč in cash → 241 Kč: a −0,45 Kč rounding charge outside the VAT base (A16).
    assert_eq!(c.amount_minor, 24_100);
    assert_eq!(
        (c.tender, c.collector),
        (Some(Tender::Cash), Some(Collector::Carrier))
    );
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(o.rounding.amount_minor, -45);
    assert_eq!(o.total.amount_minor, 24_100);
    assert_eq!(o.payment.status, orders::status::PaymentStatus::Paid);
    let r = o
        .charges
        .iter()
        .find(|c| c.kind == commerce::pricing::cart::ChargeKind::Rounding)
        .unwrap();
    assert_eq!((r.total.amount_minor, r.tax.amount_minor), (-45, 0));
    let vat: i64 = o.vat.iter().map(|v| v.gross.amount_minor).sum();
    assert_eq!(
        vat,
        12_345 + 7900 + 3900,
        "rounding stays outside the recap"
    );

    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let again = cod::collect(
        &mut tx,
        ACTOR,
        a.order_id,
        &CollectInput {
            tender: Tender::Cash,
            collector: Collector::Carrier,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(again.code(), "invalid_transition");
    tx.rollback().await.unwrap();
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let r = cod::remit(&mut tx, ACTOR, a.order_id, Some("payout 2026-09"))
        .await
        .unwrap();
    assert_eq!(r.cod_status, Some(CodStatus::Remitted));
    tx.commit().await.unwrap();
    assert_eq!(
        count(
            &runtime,
            t,
            "SELECT count(*) FROM audit_log WHERE action LIKE 'order.cod_%'"
        )
        .await,
        3
    );

    // Card: no rounding. SK cash: €0.05 steps.
    let b = place(&runtime, &s, M::Cz, MethodKind::Cod).await;
    let sk = place(&runtime, &s, M::Sk, MethodKind::Cod).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let card = cod::collect(
        &mut tx,
        ACTOR,
        b.order_id,
        &CollectInput {
            tender: Tender::Card,
            collector: Collector::Merchant,
        },
    )
    .await
    .unwrap();
    assert_eq!(card.amount_minor, 24_145);
    let eur = cod::collect(
        &mut tx,
        ACTOR,
        sk.order_id,
        &CollectInput {
            tender: Tender::Cash,
            collector: Collector::Carrier,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        eur.amount_minor, 1015,
        "5,23 + 3,90 + 1,00 = 10,13 € → 10,15 €"
    );
    let unknown = cod::collect(
        &mut tx,
        ACTOR,
        b.order_id,
        &CollectInput {
            tender: Tender::Unknown,
            collector: Collector::Merchant,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(unknown.code(), "invalid_collection");
    let not_cod = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let err = cod::deliver(&mut tx, ACTOR, not_cod.order_id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_cash_on_delivery");
}

#[sqlx::test(migrations = "../../migrations")]
async fn carrier_cod_report_applies_rows_independently(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "cod-report").await;
    let t = s.shop.tenant;
    set_price(&runtime, &s, 12_345, 523).await;
    let a = place(&runtime, &s, M::Cz, MethodKind::Cod).await;
    let b = place(&runtime, &s, M::Cz, MethodKind::Cod).await;
    let csv = format!(
        "order_number;amount;tender;event\n{a};241,00;cash;collected\n{b};999,00;cash;collected\n{a};241,00;cash;remitted\n999999;1;cash;collected\n",
        a = a.number,
        b = b.number
    );
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let r = cod::import_report(&mut tx, ACTOR, csv.as_bytes())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((r.applied, r.skipped, r.errors), (2, 0, 2), "{:?}", r.rows);
    assert_eq!(r.rows[1].result, "error");
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let mismatched = payments::attempts(&mut tx, b.order_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        mismatched.cod_status,
        Some(CodStatus::Pending),
        "rolled back"
    );
    assert_eq!(
        view(&runtime, t, b.order_id).await.rounding.amount_minor,
        0,
        "no rounding charge from the failed row"
    );
    let again = cod::import_report(&mut tx, ACTOR, csv.as_bytes())
        .await
        .unwrap();
    assert_eq!(again.skipped, 2);
    let bad = cod::import_report(&mut tx, ACTOR, b"number;x\n1;2")
        .await
        .unwrap_err();
    assert_eq!(bad.code(), "invalid_cod_report");
}

// ---------------------------------------------------------------------------------------
// Tenant isolation

#[sqlx::test(migrations = "../../migrations")]
async fn payment_tables_are_tenant_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "rls-a").await;
    let other = testkit::storefront::shop(&runtime, "rls-b").await;
    let a = place(&runtime, &s, M::Cz, MethodKind::Cod).await;
    let cz = account_id(&runtime, s.shop.tenant, s.shop.cz).await;
    import(
        &runtime,
        s.shop.tenant,
        cz,
        vec![line("R1", 100, "CZK", None)],
    )
    .await
    .unwrap();
    let mut tx = tenant_tx(&runtime, s.shop.tenant).await.unwrap();
    cod::collect(
        &mut tx,
        ACTOR,
        a.order_id,
        &CollectInput {
            tender: Tender::Card,
            collector: Collector::Carrier,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    payments::refund(
        &runtime,
        &settings().payments,
        s.shop.tenant,
        a.attempt_id,
        100,
        None,
        ACTOR,
    )
    .await
    .unwrap();
    for table in [
        "stripe_accounts",
        "bank_accounts",
        "bank_transactions",
        "refunds",
    ] {
        let sql = format!("SELECT count(*) FROM {table}");
        let mut tx = tenant_tx(&runtime, s.shop.tenant).await.unwrap();
        let mine: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.clone()))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert!(mine > 0, "{table}");
        let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
        let theirs: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.clone()))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(theirs, 0, "{table} leaks across tenants");
        let insert = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (tenant_id) VALUES ('{}')",
            s.shop.tenant
        )))
        .execute(&mut *tx)
        .await;
        assert!(insert.is_err(), "{table}: writing another tenant's row");
    }
}

// ---------------------------------------------------------------------------------------
// Review regressions

/// A Stripe client whose API cannot be reached: every call's outcome is unknown.
fn unreachable_stripe() -> Payments {
    Payments {
        stripe: Some(Stripe::new(
            &StripeConfig {
                mode: StripeMode::Simulator,
                api_url: "http://127.0.0.1:1/".parse().unwrap(),
                secret_key: "sk_test_x".into(),
                publishable_key: None,
                webhook_secret: "whsec_test_0123456789".into(),
            },
            reqwest::Client::new(),
        )),
        ..Payments::default()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn refunds_follow_the_retained_payment_and_survive_ambiguous_failures(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "refunds").await;
    let t = s.shop.tenant;
    let acct = s.account.as_str();
    let a = place(&runtime, &s, M::Cz, MethodKind::Stripe).await;
    let amount = total(&runtime, t, a.order_id).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE payment_attempts SET provider_ref = 'pi_kept' WHERE id = $1")
        .bind(a.attempt_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    deliver(
        &runtime,
        &event(
            "evt_paid",
            "payment_intent.succeeded",
            acct,
            false,
            intent("pi_kept", a.attempt_id, amount, "czk"),
        ),
    )
    .await;
    // A second payment for the same order (A10: duplicate, to be returned).
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let dup: Uuid = sqlx::query_scalar(
        "INSERT INTO payment_attempts (tenant_id, order_id, method, amount_minor, currency, provider_ref)
         VALUES ($1, $2, 'stripe', $3, 'CZK', 'pi_dup') RETURNING id",
    )
    .bind(t)
    .bind(a.order_id)
    .bind(amount)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    deliver(
        &runtime,
        &event(
            "evt_dup",
            "payment_intent.succeeded",
            acct,
            false,
            intent("pi_dup", dup, amount, "czk"),
        ),
    )
    .await;
    let o = view(&runtime, t, a.order_id).await;
    assert_eq!(o.exception.as_deref(), Some("duplicate_payment"));

    // Returning the duplicate: Stripe cannot be reached, the outcome is unknown → pending, the
    // balance stays reserved, the order keeps its payment.
    let lost = unreachable_stripe();
    let err = payments::refund(&runtime, &lost, t, dup, amount, None, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "service_unavailable");
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let pending: (Uuid, String) =
        sqlx::query_as("SELECT id, status FROM refunds WHERE attempt_id = $1")
            .bind(dup)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(pending.1, "pending");
    let again = payments::refund(&runtime, &lost, t, dup, 1, None, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(
        again.code(),
        "nothing_to_refund",
        "the pending refund reserves the balance"
    );
    // Stripe's webhook confirms it; the order is still paid (only the duplicate went back).
    let refund = |id: &str, pi: &str, amount: i64, status: &str, ours: Option<Uuid>| {
        json!({ "id": id, "object": "refund", "payment_intent": pi, "amount": amount,
                "currency": "czk", "status": status,
                "metadata": ours.map_or(json!({}), |r| json!({ "refund_id": r })) })
    };
    deliver(
        &runtime,
        &event(
            "evt_re1",
            "refund.updated",
            acct,
            false,
            refund("re_1", "pi_dup", amount, "succeeded", Some(pending.0)),
        ),
    )
    .await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let r = payments::refund_row(&mut tx, pending.0).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(r.status, payments::RefundStatus::Succeeded);
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
    // charge.refunded of the duplicate changes nothing either.
    let dup_charge = json!({ "id": "ch_dup", "object": "charge", "payment_intent": "pi_dup",
                             "amount": amount, "amount_refunded": amount, "currency": "czk" });
    assert!(matches!(
        deliver(
            &runtime,
            &event("evt_ch_dup", "charge.refunded", acct, false, dup_charge)
        )
        .await,
        Some(Processed::Ignored(_))
    ));

    // A refund made in the Stripe dashboard enters the ledger and refunds the order partly.
    deliver(
        &runtime,
        &event(
            "evt_re2",
            "refund.created",
            acct,
            false,
            refund("re_dash", "pi_kept", 100, "succeeded", None),
        ),
    )
    .await;
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::PartiallyRefunded
    );
    // The rest: first lost, then retried with the same key against stripe-mock.
    let err = payments::refund(&runtime, &lost, t, a.attempt_id, amount - 100, None, ACTOR)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "service_unavailable");
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let rest: Uuid =
        sqlx::query_scalar("SELECT id FROM refunds WHERE attempt_id = $1 AND status = 'pending'")
            .bind(a.attempt_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    let done = payments::retry_refund(&runtime, &settings().payments, t, rest)
        .await
        .unwrap();
    assert_eq!(done.status, payments::RefundStatus::Succeeded);
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Refunded
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_transfers_pay_an_order_once(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let s = setup(&runtime, "race-bank").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, t, a.order_id).await;
    let cz = account_id(&runtime, t, s.shop.cz).await;
    let (r1, r2) = tokio::join!(
        import(
            &runtime,
            t,
            cz,
            vec![line("C1", amount, "CZK", Some(&a.number))]
        ),
        import(
            &runtime,
            t,
            cz,
            vec![line("C2", amount, "CZK", Some(&a.number))]
        ),
    );
    let (r1, r2) = (r1.unwrap(), r2.unwrap());
    assert_eq!(r1.matched + r2.matched, 1, "one transfer pays the order");
    assert_eq!(
        r1.exceptions + r2.exceptions,
        1,
        "the other waits to be returned"
    );
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let open = bank::transactions(
        &mut tx,
        &bank::TxFilter {
            open: true,
            ..Default::default()
        },
        None,
        10,
    )
    .await
    .unwrap()
    .items;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].reason, Some(TxReason::AlreadyPaid));
}

#[sqlx::test(migrations = "../../migrations")]
async fn assigning_keeps_the_account_scope_and_amounts_explicit(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "assign").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, t, a.order_id).await;
    let cz = account_id(&runtime, t, s.shop.cz).await;
    let sk = account_id(&runtime, t, s.shop.sk).await;
    import(&runtime, t, sk, vec![line("S1", amount, "CZK", None)])
        .await
        .unwrap();
    import(&runtime, t, cz, vec![line("S2", amount - 500, "CZK", None)])
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let open = bank::transactions(
        &mut tx,
        &bank::TxFilter {
            open: true,
            ..Default::default()
        },
        None,
        10,
    )
    .await
    .unwrap()
    .items;
    let by = |id: &str| open.iter().find(|x| x.bank_tx_id == id).unwrap().id;
    let assign = ResolveInput {
        action: ResolveAction::Assign,
        order_number: Some(a.number.clone()),
        note: None,
    };
    // Money on the SK account never pays a CZ-account order (A25).
    let err = bank::resolve(&mut tx, ACTOR, by("S1"), &assign)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_resolution");
    // A short amount becomes a partial payment of that order, not a payment.
    let partial = bank::resolve(&mut tx, ACTOR, by("S2"), &assign)
        .await
        .unwrap();
    assert_eq!(partial.status, TxStatus::Partial);
    assert_eq!(partial.order_number.as_deref(), Some(a.number.as_str()));
    tx.commit().await.unwrap();
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Unpaid
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_new_iban_retires_the_account_which_still_matches_its_orders(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "iban").await;
    let t = s.shop.tenant;
    let a = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let amount = total(&runtime, t, a.order_id).await;
    let old = account_id(&runtime, t, s.shop.cz).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let new = bank::configure_account(
        &mut tx,
        ACTOR,
        None,
        s.shop.cz,
        &BankAccountInput {
            iban: "CZ5855000000001265098001".into(),
            bic: None,
            account_name: "Demo s.r.o.".into(),
            fio_token: None,
            clear_fio_token: false,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(new.id, old);
    assert!(new.active);
    // The old account's statement still pays the order placed while it was active.
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let r = bank::import(
        &mut tx,
        ACTOR,
        old,
        "camt053",
        &Statement {
            iban: Some(CZ_IBAN.into()),
            domestic: None,
            lines: vec![line("I1", amount, "CZK", Some(&a.number))],
        },
    )
    .await
    .unwrap();
    let all = bank::accounts(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(r.matched, 1);
    assert_eq!(all.iter().filter(|x| x.market_id == s.shop.cz).count(), 2);
    assert!(!all.iter().find(|x| x.id == old).unwrap().active);
    // New orders use the new account.
    let b = place(&runtime, &s, M::Cz, MethodKind::BankTransfer).await;
    let bt = view(&runtime, t, b.order_id)
        .await
        .payment
        .bank_transfer
        .unwrap();
    assert_eq!(bt.iban, "CZ5855000000001265098001");
}

#[sqlx::test(migrations = "../../migrations")]
async fn refund_events_are_order_independent(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "refund-order").await;
    let t = s.shop.tenant;
    let acct = s.account.as_str();
    let a = place(&runtime, &s, M::Cz, MethodKind::Stripe).await;
    let amount = total(&runtime, t, a.order_id).await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    sqlx::query("UPDATE payment_attempts SET provider_ref = 'pi_r' WHERE id = $1")
        .bind(a.attempt_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let refund = |status: &str| {
        json!({ "id": "re_x", "object": "refund", "payment_intent": "pi_r", "amount": 100,
                "currency": "czk", "status": status, "metadata": {} })
    };
    // A dashboard refund processed before the payment's success is retried, not dropped.
    let early = event(
        "evt_re_early",
        "refund.created",
        acct,
        false,
        refund("succeeded"),
    );
    let client = stripe_client();
    let sig = client.sign(&early, Utc::now()).unwrap();
    assert!(
        stripe::receive(&runtime, &client, &sig, &early)
            .await
            .unwrap()
    );
    let early_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM platform.provider_events WHERE event_id = 'evt_re_early'",
    )
    .fetch_one(&runtime)
    .await
    .unwrap();
    let err = stripe::process_event(&runtime, early_id).await.unwrap_err();
    assert_eq!(err.code(), "payment_pending");
    deliver(
        &runtime,
        &event(
            "evt_paid_r",
            "payment_intent.succeeded",
            acct,
            false,
            intent("pi_r", a.attempt_id, amount, "czk"),
        ),
    )
    .await;
    assert!(matches!(
        stripe::process_event(&runtime, early_id).await.unwrap(),
        Processed::Applied(_)
    ));
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::PartiallyRefunded
    );
    // An older `pending` event delivered late never regresses the settled refund.
    deliver(
        &runtime,
        &event(
            "evt_re_stale",
            "refund.updated",
            acct,
            false,
            refund("pending"),
        ),
    )
    .await;
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM refunds WHERE provider_ref = 're_x'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(status, "succeeded");
    drop(tx);
    // WP12: the bank returns the refund later (`refund.updated` failed): the money is owed
    // again, so the payment state goes back to paid (the order can be refunded anew).
    deliver(
        &runtime,
        &event(
            "evt_re_failed",
            "refund.updated",
            acct,
            false,
            refund("failed"),
        ),
    )
    .await;
    assert_eq!(
        view(&runtime, t, a.order_id).await.payment.status,
        orders::status::PaymentStatus::Paid
    );
}
