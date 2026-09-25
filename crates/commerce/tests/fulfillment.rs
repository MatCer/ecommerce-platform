//! WP12 against a real Postgres as the runtime role: invoices per A17 scenario with gapless
//! numbers and immutability, dispatch with stock commit (A13), refunds reversing allocations
//! with credit notes (A15), cancellation, the A19 withdrawal flow, returns to sender, and
//! tenant isolation of the new tables. Carriers are exercised end to end against the mocks
//! (e2e); here the shop ships by personal pickup (no carrier call).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use commerce::carriers::Carriers;
use commerce::cart::{self, NewLine, Scope};
use commerce::checkout::{
    self, AddressesInput, CheckoutAddress, ContactInput, PaymentInput, PlaceOrderInput, Placer,
    Settings, ShippingInput,
};
use commerce::content::legal::{self, LegalEntity};
use commerce::fulfillment::{self, LabelInput, ShipmentStatus};
use commerce::inventory;
use commerce::invoicing::{self, Issued, Rates, document::RefundLine};
use commerce::orders;
use commerce::payments::bank::{self, BankAccountInput};
use commerce::payments::{self, MethodKind, Outcome, PaymentMethodInput, Payments};
use commerce::refunds::{self, CancelInput, RefundInput};
use commerce::shipping::{self, Carrier, ShippingMethodInput};
use commerce::storefront::{self, Context, PublicUrls};
use commerce::withdrawals::{self, DeclareInput, LinkRequest};
use platform::Error;
use platform::crypto::SecretBox;
use platform::db::{TenantTx, tenant_tx};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const ACTOR: &str = "test";

fn settings() -> Settings {
    Settings {
        payments: Payments {
            fake: None,
            stripe: None,
            secrets: Some(Arc::new(SecretBox::new(&[7; 32]))),
        },
        packeta: None,
    }
}

/// No carrier or ČNB endpoint is reachable: CZK documents need no rate, pickup no carrier.
fn rates() -> Rates {
    Rates {
        http: reqwest::Client::new(),
        url: "http://127.0.0.1:1/cnb".into(),
    }
}

fn carriers() -> Carriers {
    Carriers::new(
        "http://127.0.0.1:1/packeta".into(),
        "http://127.0.0.1:1/validate".into(),
        "http://127.0.0.1:1/ppl".into(),
        None,
    )
    .unwrap()
}

struct Setup {
    shop: Shop,
    pickup: Uuid,
}

async fn ctx(tx: &mut TenantTx, market: Uuid) -> Context {
    storefront::context(tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap()
}

/// Personal pickup (COD allowed) in CZ, bank transfer and COD, a receiving account and the
/// seller's legal entity.
async fn setup(runtime: &PgPool, slug: &str) -> Setup {
    let shop = testkit::storefront::shop(runtime, slug).await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let pickup = shipping::create(
        &mut tx,
        ACTOR,
        &ShippingMethodInput {
            market_id: shop.cz,
            carrier: Carrier::PersonalPickup,
            name_i18n: [("cs".to_owned(), "Osobní odběr".to_owned())].into(),
            description_i18n: Default::default(),
            price_minor: 5_000,
            free_over_minor: None,
            weight_tiers: vec![],
            cod_allowed: true,
            cod_fee_minor: 2_000,
            active: true,
            position: 0,
        },
    )
    .await
    .unwrap()
    .id;
    bank::configure_account(
        &mut tx,
        ACTOR,
        None,
        shop.cz,
        &BankAccountInput {
            iban: "CZ6508000000192000145399".into(),
            bic: Some("GIBACZPX".into()),
            account_name: "Demo s.r.o.".into(),
            fio_token: None,
            clear_fio_token: false,
        },
    )
    .await
    .unwrap();
    for kind in [MethodKind::BankTransfer, MethodKind::Cod] {
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
    legal::put_entity(
        &mut tx,
        ACTOR,
        &LegalEntity {
            company_name: "Demo s.r.o.".into(),
            company_id: "12345678".into(),
            street: "Dlouhá 1".into(),
            city: "Praha".into(),
            postal_code: "110 00".into(),
            country: "CZ".into(),
            email: "shop@example.test".into(),
            ..LegalEntity::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    Setup { shop, pickup }
}

/// Places 2 × variant 0 + 1 × variant 1 in CZ with `kind`; returns (order, attempt).
async fn place(runtime: &PgPool, s: &Setup, kind: MethodKind) -> (Uuid, Uuid) {
    let market = s.shop.cz;
    let mut tx = tenant_tx(runtime, s.shop.tenant).await.unwrap();
    let ctx = ctx(&mut tx, market).await;
    let (_, token) = cart::create(&mut tx, &ctx).await.unwrap();
    let c = cart::find(&mut tx, &ctx, &token, None).await.unwrap();
    for (v, q) in [(s.shop.variants[0], 2), (s.shop.variants[1], 1)] {
        cart::add_line(
            &mut tx,
            &ctx,
            &c,
            &NewLine {
                variant_id: v,
                quantity: q,
            },
        )
        .await
        .unwrap();
    }
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
    checkout::set_addresses(
        &mut tx,
        &ctx,
        &c,
        &AddressesInput {
            billing: CheckoutAddress {
                name: "Jana Nováková".into(),
                company: None,
                street: "Dlouhá 12".into(),
                city: "Praha".into(),
                postal_code: "110 00".into(),
                country: "CZ".into(),
                phone: None,
            },
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
            method_id: s.pickup,
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
    let placed = checkout::place_order(
        &mut tx,
        &ctx,
        &settings(),
        &checkout_token,
        &Uuid::now_v7().to_string(),
        "hash",
        &PlaceOrderInput {
            version: v.cart.version,
            total_minor: v.totals.total.amount_minor,
            accept_terms: true,
            accept_withdrawal: true,
            email_marketing: false,
            review_invites: false,
            notes: None,
        },
        &Placer::default(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (placed.order_id, placed.attempt_id)
}

async fn pay(runtime: &PgPool, tenant: Uuid, attempt: Uuid) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    payments::apply_outcome(&mut tx, attempt, Outcome::Succeeded, ACTOR)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn on_hand(runtime: &PgPool, tenant: Uuid, variant: Uuid) -> (i32, i32) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let l = inventory::get(&mut tx, variant).await.unwrap();
    (l.on_hand, l.reserved)
}

async fn run<T>(
    runtime: &PgPool,
    tenant: Uuid,
    f: impl AsyncFnOnce(&mut TenantTx) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let out = f(&mut tx).await?;
    tx.commit().await.unwrap();
    Ok(out)
}

async fn emails(runtime: &PgPool, tenant: Uuid, template: &str) -> Vec<(String, String)> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT coalesce(body_text, ''), attachments::text FROM email_messages
         WHERE template = $1 ORDER BY created_at",
    )
    .bind(template)
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    rows
}

/// Label (personal pickup) → shipped → delivered.
async fn ship_and_deliver(runtime: &PgPool, s: &Setup, order: Uuid) {
    let t = s.shop.tenant;
    fulfillment::create_label(
        runtime,
        &carriers(),
        &testkit::memory_storage(),
        t,
        ACTOR,
        order,
        &LabelInput::default(),
    )
    .await
    .unwrap();
    let urls = PublicUrls::default();
    run(runtime, t, async |tx| {
        fulfillment::ship(tx, &urls, ACTOR, order).await
    })
    .await
    .unwrap();
    run(runtime, t, async |tx| {
        fulfillment::deliver(tx, &urls, ACTOR, order).await
    })
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn prepaid_invoice_dispatch_and_partial_refund_with_credit_note(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "wp12-prepaid").await;
    let t = s.shop.tenant;
    let (order, attempt) = place(&runtime, &s, MethodKind::BankTransfer).await;

    // A17.1: nothing before the payment.
    assert_eq!(
        invoicing::issue(&runtime, &rates(), t, order, Utc::now())
            .await
            .unwrap(),
        Issued::NotDue
    );
    pay(&runtime, t, attempt).await;
    let Issued::New(invoice) = invoicing::issue(&runtime, &rates(), t, order, Utc::now())
        .await
        .unwrap()
    else {
        panic!("expected a new invoice");
    };
    assert_eq!(
        invoicing::issue(&runtime, &rates(), t, order, Utc::now())
            .await
            .unwrap(),
        Issued::Existing(invoice),
        "idempotent"
    );
    let (_, doc, _) = run(&runtime, t, async |tx| invoicing::get(tx, invoice).await)
        .await
        .unwrap();
    let year = invoicing::prague::date(Utc::now()).format("%Y").to_string();
    assert_eq!(doc.number, format!("FV{year}00001"));
    assert_eq!(doc.taxable_supply_date, invoicing::prague::date(Utc::now()));
    assert!(doc.payment.paid);
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(
        doc.totals.gross_minor, o.total.amount_minor,
        "A15: never re-priced"
    );
    assert_eq!(doc.totals.vat_minor, o.vat_total.amount_minor);

    // Immutable: the runtime role cannot rewrite an issued document.
    let mut tx = tenant_tx(&runtime, t).await.unwrap();
    let denied = sqlx::query("UPDATE invoices SET document = '{}' WHERE id = $1")
        .bind(invoice)
        .execute(&mut *tx)
        .await;
    assert!(denied.is_err(), "invoices must be immutable");
    drop(tx);

    // Dispatch commits the reserved stock (A13).
    let before = on_hand(&runtime, t, s.shop.variants[0]).await;
    ship_and_deliver(&runtime, &s, order).await;
    let after = on_hand(&runtime, t, s.shop.variants[0]).await;
    assert_eq!((after.0, after.1), (before.0 - 2, before.1 - 2));
    assert_eq!(emails(&runtime, t, "order_shipped").await.len(), 1);
    assert_eq!(emails(&runtime, t, "order_delivered").await.len(), 1);
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(o.status.as_str(), "delivered");

    // A15: one of two units now, the other later; together exactly the line's allocation.
    let line = o.lines[0].clone();
    let line_id = run(&runtime, t, async |tx| {
        Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM order_lines WHERE order_id = $1 ORDER BY position LIMIT 1",
        )
        .bind(order)
        .fetch_one(&mut **tx)
        .await?)
    })
    .await
    .unwrap();
    let urls = PublicUrls::default();
    let one = |q| RefundInput {
        lines: vec![RefundLine {
            order_line_id: line_id,
            quantity: q,
        }],
        iban: Some("CZ65 0800 0000 1920 0014 5399".into()),
        ..RefundInput::default()
    };
    let first = refunds::refund_order(
        &runtime,
        &settings().payments,
        &urls,
        t,
        ACTOR,
        order,
        &one(1),
    )
    .await
    .unwrap();
    assert!(first.credit_note_id.is_some());
    assert_eq!(first.plan.amount.amount_minor, line.total.amount_minor / 2);
    // Over-refunding is refused.
    let err = refunds::refund_order(
        &runtime,
        &settings().payments,
        &urls,
        t,
        ACTOR,
        order,
        &one(2),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Validation {
                code: "invalid_refund",
                ..
            }
        ),
        "{err:?}"
    );
    let second = refunds::refund_order(
        &runtime,
        &settings().payments,
        &urls,
        t,
        ACTOR,
        order,
        &one(1),
    )
    .await
    .unwrap();
    assert_eq!(
        first.plan.amount.amount_minor + second.plan.amount.amount_minor,
        line.total.amount_minor
    );
    let (_, cn, _) = run(&runtime, t, async |tx| {
        invoicing::get(tx, second.credit_note_id.unwrap()).await
    })
    .await
    .unwrap();
    assert_eq!(
        cn.number,
        format!("DB{year}00002"),
        "gapless credit note series"
    );
    assert_eq!(cn.original.as_ref().unwrap().id, invoice);
    assert_eq!(cn.totals.gross_minor, -second.plan.amount.amount_minor);
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(o.payment.status.as_str(), "partially_refunded");
    assert_eq!(emails(&runtime, t, "order_refunded").await.len(), 2);
    // The dashboard nets refunds against the order's revenue (WP14 follow-up).
    let today = Utc::now().date_naive();
    let d = run(&runtime, t, async |tx| {
        commerce::analytics::dashboard(
            tx,
            &commerce::analytics::DashboardQuery {
                from: today - chrono::Days::new(1),
                to: today + chrono::Days::new(1),
                market_id: None,
            },
        )
        .await
    })
    .await
    .unwrap();
    assert_eq!(
        d.sales[0].revenue_minor,
        o.total.amount_minor - line.total.amount_minor
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn cod_is_invoiced_on_dispatch_and_a_returned_parcel_is_restocked(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "wp12-cod").await;
    let t = s.shop.tenant;
    let start = on_hand(&runtime, t, s.shop.variants[0]).await;
    let (order, _) = place(&runtime, &s, MethodKind::Cod).await;
    assert_eq!(
        invoicing::issue(&runtime, &rates(), t, order, Utc::now())
            .await
            .unwrap(),
        Issued::NotDue,
        "A17.2: COD is invoiced on dispatch"
    );
    fulfillment::create_label(
        &runtime,
        &carriers(),
        &testkit::memory_storage(),
        t,
        ACTOR,
        order,
        &LabelInput::default(),
    )
    .await
    .unwrap();
    let urls = PublicUrls::default();
    run(&runtime, t, async |tx| {
        fulfillment::ship(tx, &urls, ACTOR, order).await
    })
    .await
    .unwrap();
    // The dispatch invoice must exist before a returned parcel can be corrected.
    let early = run(&runtime, t, async |tx| {
        fulfillment::returned_to_sender(tx, ACTOR, order, Utc::now()).await
    })
    .await;
    assert!(
        matches!(
            early,
            Err(Error::Conflict {
                code: "invoice_pending",
                ..
            })
        ),
        "{early:?}"
    );
    let Issued::New(invoice) = invoicing::issue(&runtime, &rates(), t, order, Utc::now())
        .await
        .unwrap()
    else {
        panic!("expected the dispatch invoice");
    };
    let (_, doc, _) = run(&runtime, t, async |tx| invoicing::get(tx, invoice).await)
        .await
        .unwrap();
    assert!(!doc.payment.paid);
    assert_eq!(doc.payment.method, "cod");

    run(&runtime, t, async |tx| {
        fulfillment::returned_to_sender(tx, ACTOR, order, Utc::now()).await
    })
    .await
    .unwrap();
    let back = on_hand(&runtime, t, s.shop.variants[0]).await;
    assert_eq!(back, start, "restocked and nothing reserved");
    let invoices = run(&runtime, t, async |tx| {
        invoicing::list_for_order(tx, order).await
    })
    .await
    .unwrap();
    assert_eq!(
        invoices.len(),
        2,
        "the unpaid invoice is cancelled by a credit note"
    );
    assert_eq!(
        invoices[1].total.amount_minor,
        -invoices[0].total.amount_minor
    );
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(o.status.as_str(), "returned");
}

#[sqlx::test(migrations = "../../migrations")]
async fn cancelling_a_paid_order_releases_stock_refunds_and_issues_a_credit_note(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "wp12-cancel").await;
    let t = s.shop.tenant;
    let start = on_hand(&runtime, t, s.shop.variants[0]).await;
    let (order, attempt) = place(&runtime, &s, MethodKind::BankTransfer).await;
    pay(&runtime, t, attempt).await;
    let urls = PublicUrls::default();
    let input = CancelInput {
        reason: Some("out of stock".into()),
        iban: Some("CZ6508000000192000145399".into()),
    };
    // A17: the credit note needs the invoice, which the job has not issued yet.
    let err = refunds::cancel(
        &runtime,
        &settings().payments,
        &urls,
        t,
        ACTOR,
        order,
        &input,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::Conflict {
                code: "invoice_pending",
                ..
            }
        ),
        "{err:?}"
    );
    invoicing::issue(&runtime, &rates(), t, order, Utc::now())
        .await
        .unwrap();
    let out = refunds::cancel(
        &runtime,
        &settings().payments,
        &urls,
        t,
        ACTOR,
        order,
        &input,
    )
    .await
    .unwrap();
    let refund = out.refund.unwrap();
    assert!(refund.plan.full);
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(refund.plan.amount.amount_minor, o.total.amount_minor);
    assert_eq!(o.status.as_str(), "cancelled");
    assert_eq!(o.payment.status.as_str(), "refunded");
    assert_eq!(on_hand(&runtime, t, s.shop.variants[0]).await, start);
    assert_eq!(emails(&runtime, t, "order_cancelled").await.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn withdrawal_link_declaration_receipt_restock_and_refund(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "wp12-withdraw").await;
    let t = s.shop.tenant;
    let (order, attempt) = place(&runtime, &s, MethodKind::BankTransfer).await;
    pay(&runtime, t, attempt).await;
    invoicing::issue(&runtime, &rates(), t, order, Utc::now())
        .await
        .unwrap();
    let number = run(&runtime, t, async |tx| {
        Ok(orders::view(tx, order).await?.number)
    })
    .await
    .unwrap();
    let link = |email: &str| LinkRequest {
        order_number: number.clone(),
        email: email.into(),
    };
    // Not dispatched yet: no link (but the same neutral answer).
    run(&runtime, t, async |tx| {
        let c = ctx(tx, s.shop.cz).await;
        withdrawals::request_link(tx, &c, &link("jana@example.test")).await
    })
    .await
    .unwrap();
    assert!(emails(&runtime, t, "withdrawal_link").await.is_empty());
    ship_and_deliver(&runtime, &s, order).await;
    // A wrong email gets the same answer and no link.
    run(&runtime, t, async |tx| {
        let c = ctx(tx, s.shop.cz).await;
        withdrawals::request_link(tx, &c, &link("someone@example.test")).await
    })
    .await
    .unwrap();
    assert!(emails(&runtime, t, "withdrawal_link").await.is_empty());
    run(&runtime, t, async |tx| {
        let c = ctx(tx, s.shop.cz).await;
        withdrawals::request_link(tx, &c, &link(" Jana@Example.test ")).await
    })
    .await
    .unwrap();
    let mail = emails(&runtime, t, "withdrawal_link").await;
    assert_eq!(mail.len(), 1);
    let token = mail[0]
        .0
        .split("/withdraw?t=")
        .nth(1)
        .unwrap()
        .chars()
        .take(64)
        .collect::<String>();

    let form = run(&runtime, t, async |tx| {
        let o = withdrawals::order_by_token(tx, &token).await?;
        withdrawals::form(tx, o).await
    })
    .await
    .unwrap();
    assert!(form.eligible && form.needs_iban);
    assert!(form.deadline.is_some());
    let all: Vec<RefundLine> = form
        .lines
        .iter()
        .map(|l| RefundLine {
            order_line_id: l.order_line_id,
            quantity: l.withdrawable,
        })
        .collect();
    let declare = |confirm: bool, iban: Option<&str>| DeclareInput {
        lines: all.clone(),
        iban: iban.map(str::to_owned),
        note: None,
        confirm,
    };
    // A19: the explicit confirmation step, and the bank account for a transfer refund.
    for (input, code) in [
        (
            declare(false, Some("CZ6508000000192000145399")),
            "confirmation_required",
        ),
        (declare(true, None), "iban_required"),
    ] {
        let err = run(&runtime, t, async |tx| {
            let c = ctx(tx, s.shop.cz).await;
            let o = withdrawals::consume_token(tx, &token).await?;
            withdrawals::declare(tx, &c, o, &input, "web").await
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, Error::Validation { code: c, .. } if c == code),
            "{err:?}"
        );
    }
    let receipt = run(&runtime, t, async |tx| {
        let c = ctx(tx, s.shop.cz).await;
        let o = withdrawals::consume_token(tx, &token).await?;
        withdrawals::declare(
            tx,
            &c,
            o,
            &declare(true, Some("CZ6508000000192000145399")),
            "web",
        )
        .await
    })
    .await
    .unwrap();
    assert!(receipt.declaration.contains(&number));
    assert_eq!(
        receipt.refund_due_at - receipt.declared_at,
        chrono::Duration::days(14)
    );
    let r = emails(&runtime, t, "withdrawal_receipt").await;
    assert_eq!(r.len(), 1);
    assert!(
        r[0].0.contains(&number),
        "the receipt carries the declaration"
    );
    // Single use.
    let reused = run(&runtime, t, async |tx| {
        withdrawals::consume_token(tx, &token).await
    })
    .await;
    assert!(matches!(reused, Err(Error::NotFound)));

    let w = run(&runtime, t, async |tx| withdrawals::list(tx, true).await)
        .await
        .unwrap()
        .remove(0);
    let urls = PublicUrls::default();
    let early = withdrawals::refund(&runtime, &settings().payments, &urls, t, ACTOR, w.id).await;
    assert!(matches!(
        early,
        Err(Error::Conflict {
            code: "goods_not_back",
            ..
        })
    ));
    let stock = on_hand(&runtime, t, s.shop.variants[0]).await;
    run(&runtime, t, async |tx| {
        withdrawals::receive(tx, ACTOR, w.id).await
    })
    .await
    .unwrap();
    assert_eq!(
        on_hand(&runtime, t, s.shop.variants[0]).await.0,
        stock.0 + 2,
        "restocked"
    );
    let out = withdrawals::refund(&runtime, &settings().payments, &urls, t, ACTOR, w.id)
        .await
        .unwrap();
    // Everything was withdrawn: the outbound shipping and the COD-free payment go back too.
    assert!(out.plan.full);
    assert!(out.plan.lines.iter().any(|l| l.kind == "shipping"));
    let o = run(&runtime, t, async |tx| orders::view(tx, order).await)
        .await
        .unwrap();
    assert_eq!(o.status.as_str(), "returned");
    assert_eq!(o.payment.status.as_str(), "refunded");
    let w = run(&runtime, t, async |tx| withdrawals::get(tx, w.id).await)
        .await
        .unwrap();
    assert_eq!(w.status, "refunded");
    // Refunded once: a repeated (or concurrent) request is refused.
    let again = withdrawals::refund(&runtime, &settings().payments, &urls, t, ACTOR, w.id).await;
    assert!(
        matches!(
            again,
            Err(Error::Conflict {
                code: "already_refunded",
                ..
            })
        ),
        "{again:?}"
    );

    assert_eq!(w.iban.as_deref(), Some("CZ6508000000192000145399"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn wp12_tables_are_isolated_per_tenant(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let a = setup(&runtime, "wp12-iso-a").await;
    let b = setup(&runtime, "wp12-iso-b").await;
    let (order, attempt) = place(&runtime, &a, MethodKind::BankTransfer).await;
    pay(&runtime, a.shop.tenant, attempt).await;
    invoicing::issue(&runtime, &rates(), a.shop.tenant, order, Utc::now())
        .await
        .unwrap();
    ship_and_deliver(&runtime, &a, order).await;
    run(&runtime, a.shop.tenant, async |tx| {
        let c = ctx(tx, a.shop.cz).await;
        let number = orders::view(tx, order).await?.number;
        withdrawals::request_link(
            tx,
            &c,
            &LinkRequest {
                order_number: number,
                email: "jana@example.test".into(),
            },
        )
        .await
    })
    .await
    .unwrap();
    let mut tx = tenant_tx(&runtime, b.shop.tenant).await.unwrap();
    for table in [
        "shipments",
        "invoices",
        "invoice_series",
        "withdrawal_tokens",
        "carrier_accounts",
        "documents",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
    }
    // Writing into another tenant's rows is refused by the policy.
    let forged = sqlx::query(
        "INSERT INTO shipments (tenant_id, order_id, carrier, status, created_by)
         VALUES ($1, $2, 'ppl', 'creating', 'x')",
    )
    .bind(a.shop.tenant)
    .bind(order)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err());
    drop(tx);
    // The withdrawal link of tenant A does not open in tenant B.
    let mut tx = tenant_tx(&runtime, a.shop.tenant).await.unwrap();
    let body: String = sqlx::query_scalar(
        "SELECT body_text FROM email_messages WHERE template = 'withdrawal_link'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    drop(tx);
    let token: String = body
        .split("/withdraw?t=")
        .nth(1)
        .unwrap()
        .chars()
        .take(64)
        .collect();
    let other = run(&runtime, b.shop.tenant, async |tx| {
        withdrawals::order_by_token(tx, &token).await
    })
    .await;
    assert!(matches!(other, Err(Error::NotFound)));
    // Nor its shipments.
    let s = run(&runtime, b.shop.tenant, async |tx| {
        fulfillment::shipments(tx, order).await
    })
    .await
    .unwrap();
    assert!(s.is_empty());
    let own = run(&runtime, a.shop.tenant, async |tx| {
        fulfillment::shipments(tx, order).await
    })
    .await
    .unwrap();
    assert_eq!(own[0].status, ShipmentStatus::Delivered);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_returned_parcel_after_a_withdrawal_restocks_only_what_is_not_back(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let s = setup(&runtime, "wp12-restock").await;
    let t = s.shop.tenant;
    let (order, attempt) = place(&runtime, &s, MethodKind::BankTransfer).await;
    pay(&runtime, t, attempt).await;
    invoicing::issue(&runtime, &rates(), t, order, Utc::now())
        .await
        .unwrap();
    ship_and_deliver(&runtime, &s, order).await;
    let (v0, v1) = (s.shop.variants[0], s.shop.variants[1]);
    let (a0, a1) = (
        on_hand(&runtime, t, v0).await.0,
        on_hand(&runtime, t, v1).await.0,
    );
    // The second line (1 × variant 1) is withdrawn and comes back.
    let w = run(&runtime, t, async |tx| {
        let c = ctx(tx, s.shop.cz).await;
        let f = withdrawals::form(tx, order).await?;
        let line = f.lines[1].order_line_id;
        withdrawals::declare(
            tx,
            &c,
            order,
            &DeclareInput {
                lines: vec![RefundLine {
                    order_line_id: line,
                    quantity: 1,
                }],
                iban: Some("CZ6508000000192000145399".into()),
                note: None,
                confirm: true,
            },
            "account",
        )
        .await
    })
    .await
    .unwrap();
    run(&runtime, t, async |tx| {
        withdrawals::receive(tx, ACTOR, w.id).await
    })
    .await
    .unwrap();
    assert_eq!(on_hand(&runtime, t, v1).await.0, a1 + 1);
    // Then the (whole) parcel is reported back: only what is not back yet is restocked.
    run(&runtime, t, async |tx| {
        fulfillment::returned_to_sender(tx, ACTOR, order, Utc::now()).await
    })
    .await
    .unwrap();
    assert_eq!(on_hand(&runtime, t, v0).await.0, a0 + 2);
    assert_eq!(on_hand(&runtime, t, v1).await.0, a1 + 1, "never twice");
}
