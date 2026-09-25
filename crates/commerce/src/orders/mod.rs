//! Orders (spec §7.3, §10.6, A4, A10, A13, A15). Placement lives in `commerce::checkout`;
//! this module owns the order row's lifecycle (every status change goes through the WP9
//! machines in [`status`] and writes an `order_events` row), the order capability tokens and
//! the read models (the customer's order page, the account list, the admin list and detail).

pub mod status;

use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::cart::VatRow;
use crate::inventory::{self, MovementRef};
use crate::money::{Currency, Locale, Money, MoneyView};
use crate::payments::{self, Attempt, AttemptStatus, MethodKind};
use crate::pricing::cart::{ChargeKind, VatRecapRow};
use crate::promotions::coupons;
use crate::shipping::Carrier;
use status::{
    FulfillmentStatus, OrderCommand, OrderStatus, PaymentCommand, PaymentEvent, PaymentStatus,
    order_transition, payment_transition,
};

/// A4: order capability tokens are valid for 90 days.
pub const TOKEN_DAYS: i64 = 90;

/// Outbox events (§8.5). Payload: `{order_id, number}` (+ `total_minor`, `currency`).
pub const CREATED_EVENT: &str = "order.created";
pub const PAID_EVENT: &str = "order.paid";
pub const CANCELLED_EVENT: &str = "order.cancelled";
/// A10: money arrived for an expired/cancelled order; a refund task for WP11/12.
pub const EXCEPTION_EVENT: &str = "order.exception";

/// Appends to the order's timeline.
pub async fn event(
    tx: &mut TenantTx,
    order_id: Uuid,
    kind: &str,
    data: &Value,
    actor: &str,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO order_events (id, tenant_id, order_id, kind, data, actor)
         VALUES ($1, $2, $3, $4, $5, $6)",
        crate::id::new_id(),
        tx.tenant_id(),
        order_id,
        kind,
        data,
        actor
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Mints a read-only capability for `/o/<token>` (A4).
pub async fn issue_token(tx: &mut TenantTx, order_id: Uuid) -> Result<String, Error> {
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO order_tokens (token_hash, tenant_id, order_id, expires_at)
         VALUES ($1, $2, $3, $4)",
        minted.hash,
        tx.tenant_id(),
        order_id,
        Utc::now() + Duration::days(TOKEN_DAYS)
    )
    .execute(&mut **tx)
    .await?;
    Ok(minted.token)
}

/// The order behind a capability token, `404` for unknown or expired tokens.
pub async fn by_token(tx: &mut TenantTx, token: &str) -> Result<Uuid, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    sqlx::query_scalar!(
        "SELECT order_id FROM order_tokens WHERE token_hash = $1 AND expires_at > now()",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

// ---------------------------------------------------------------------------------------
// Lifecycle

/// The mutable part of an order, locked for the transaction.
#[derive(Debug, Clone)]
pub(crate) struct OrderRow {
    pub id: Uuid,
    pub number: i64,
    pub currency: String,
    pub status: String,
    pub payment_status: String,
    pub payment_method: String,
    pub total_minor: i64,
    pub coupon_id: Option<Uuid>,
    pub payment_expires_at: Option<DateTime<Utc>>,
}

/// Locks the order row: every change to an order and its payment attempts happens under it.
pub(crate) async fn lock(tx: &mut TenantTx, id: Uuid) -> Result<OrderRow, Error> {
    sqlx::query_as!(
        OrderRow,
        "SELECT id, number, currency, status, payment_status, payment_method, total_minor,
                coupon_id, payment_expires_at
         FROM orders WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

fn stored<T: FromStr>(v: &str) -> Result<T, Error> {
    v.parse()
        .map_err(|_| Error::Internal(format!("unknown stored state {v}")))
}

fn publish_payload(o: &OrderRow) -> Value {
    json!({ "order_id": o.id, "number": o.number.to_string(), "total_minor": o.total_minor,
            "currency": o.currency })
}

pub(crate) async fn apply_order(
    tx: &mut TenantTx,
    o: &mut OrderRow,
    command: OrderCommand,
    actor: &str,
) -> Result<(), Error> {
    let from: OrderStatus = stored(&o.status)?;
    let (next, events) = order_transition(from, command)?;
    sqlx::query!(
        "UPDATE orders SET status = $2, updated_at = now() WHERE id = $1",
        o.id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    for e in events {
        event(
            tx,
            o.id,
            "status_changed",
            &json!({ "from": from, "to": next, "event": e }),
            actor,
        )
        .await?;
    }
    o.status = next.as_str().to_owned();
    Ok(())
}

pub(crate) async fn apply_payment(
    tx: &mut TenantTx,
    o: &mut OrderRow,
    command: PaymentCommand,
    actor: &str,
) -> Result<Vec<PaymentEvent>, Error> {
    let from: PaymentStatus = stored(&o.payment_status)?;
    let (next, events) = payment_transition(from, command, stored(&o.status)?)?;
    sqlx::query!(
        "UPDATE orders SET payment_status = $2, updated_at = now() WHERE id = $1",
        o.id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    event(
        tx,
        o.id,
        "payment_status_changed",
        &json!({ "from": from, "to": next, "events": events }),
        actor,
    )
    .await?;
    o.payment_status = next.as_str().to_owned();
    Ok(events)
}

/// The money for the order arrived: `paid`, and a pending order is confirmed. After expiry or
/// cancellation it is a late payment instead (A10): the order gets the `late_payment`
/// exception and a refund task event; its stock stays released.
pub(crate) async fn payment_succeeded(
    tx: &mut TenantTx,
    o: &mut OrderRow,
    attempt_id: Uuid,
    actor: &str,
) -> Result<(), Error> {
    // The payment the order keeps; refunds of any other attempt return extra money (A10).
    sqlx::query!(
        "UPDATE orders SET paid_attempt_id = $2 WHERE id = $1 AND paid_attempt_id IS NULL",
        o.id,
        attempt_id
    )
    .execute(&mut **tx)
    .await?;
    let events = apply_payment(tx, o, PaymentCommand::Succeed, actor).await?;
    if events.contains(&PaymentEvent::LatePayment) {
        return flag_exception(tx, o, "late_payment", actor).await;
    }
    if o.status == OrderStatus::Pending.as_str() {
        apply_order(tx, o, OrderCommand::Confirm, actor).await?;
    }
    platform::queue::publish(&mut **tx, PAID_EVENT, &publish_payload(o)).await?;
    Ok(())
}

/// Money the order cannot keep (A10): `late_payment` (after expiry or cancellation) or
/// `duplicate_payment` (a second attempt succeeded). Recorded with a refund task event; stock
/// is never touched.
pub(crate) async fn flag_exception(
    tx: &mut TenantTx,
    o: &OrderRow,
    exception: &str,
    actor: &str,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE orders SET exception = $2, exception_resolved_at = NULL, exception_note = NULL,
             updated_at = now()
         WHERE id = $1",
        o.id,
        exception
    )
    .execute(&mut **tx)
    .await?;
    event(
        tx,
        o.id,
        "exception",
        &json!({ "exception": exception, "action": "refund_required" }),
        actor,
    )
    .await?;
    platform::queue::publish(&mut **tx, EXCEPTION_EVENT, &publish_payload(o)).await?;
    Ok(())
}

/// A person settled an order's exception (refunded the money, or decided to keep it and ship):
/// it leaves the refund work list (audited). `409 no_open_exception` otherwise.
pub async fn resolve_exception(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    note: &str,
) -> Result<(), Error> {
    let note = note.trim();
    if note.is_empty() || note.chars().count() > 500 {
        return Err(crate::markets::invalid(
            "invalid_resolution",
            "a note of 1-500 characters is required",
        ));
    }
    let o = lock(tx, order_id).await?;
    let open = sqlx::query_scalar!(
        "SELECT exception FROM orders WHERE id = $1 AND exception IS NOT NULL
             AND exception_resolved_at IS NULL",
        o.id
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten()
    .ok_or_else(|| Error::Conflict {
        code: "no_open_exception",
        detail: "the order has no open exception".into(),
    })?;
    sqlx::query!(
        "UPDATE orders SET exception_resolved_at = now(), exception_note = $2, updated_at = now()
         WHERE id = $1",
        o.id,
        note
    )
    .execute(&mut **tx)
    .await?;
    let data = json!({ "exception": open, "note": note });
    event(tx, o.id, "exception_resolved", &data, actor).await?;
    crate::audit::record(
        tx,
        actor,
        "order.exception_resolved",
        "order",
        Some(&o.id.to_string()),
        &data,
    )
    .await?;
    Ok(())
}

/// Cancels an unpaid order whose payment window closed (A10, A13): open attempts expire, the
/// payment becomes `expired`, the stock reservations and the coupon redemption are released.
pub(crate) async fn expire_unpaid(
    tx: &mut TenantTx,
    o: &mut OrderRow,
    actor: &str,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE payment_attempts SET status = 'expired', completed_at = now(), updated_at = now()
         WHERE order_id = $1 AND status = 'pending'",
        o.id
    )
    .execute(&mut **tx)
    .await?;
    if matches!(
        o.payment_status.as_str(),
        "unpaid" | "authorized" | "failed"
    ) {
        apply_payment(tx, o, PaymentCommand::Expire, actor).await?;
    }
    apply_order(tx, o, OrderCommand::Cancel, actor).await?;
    let lines = sqlx::query!(
        "SELECT variant_id AS \"variant_id!\", sum(quantity)::int AS \"quantity!\"
         FROM order_lines WHERE order_id = $1 AND variant_id IS NOT NULL
         GROUP BY variant_id ORDER BY variant_id",
        o.id
    )
    .fetch_all(&mut **tx)
    .await?;
    let ref_id = o.id.to_string();
    let r = MovementRef {
        ref_type: "order",
        ref_id: &ref_id,
    };
    for l in lines {
        inventory::release(tx, &r, l.variant_id, l.quantity).await?;
    }
    if let Some(coupon) = o.coupon_id {
        coupons::release(tx, coupon, &ref_id).await?;
    }
    platform::queue::publish(&mut **tx, CANCELLED_EVENT, &publish_payload(o)).await?;
    Ok(())
}

/// A5: links the guest orders placed with the customer's (verified) email to the account.
pub async fn link_guest_orders(tx: &mut TenantTx, customer_id: Uuid) -> Result<u64, Error> {
    Ok(sqlx::query!(
        "UPDATE orders o SET customer_id = c.id, updated_at = now()
         FROM customers c
         WHERE c.id = $1 AND c.email_verified_at IS NOT NULL
           AND o.customer_id IS NULL AND o.email = c.email",
        customer_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Whether the requester may pay the order (new attempts, provider init). The order token is
/// a read-only capability (A4), so paying needs more: the checkout cart capability of the cart
/// the order was placed from (the same browser), or the session of the order's customer.
pub async fn may_pay(
    tx: &mut TenantTx,
    order_id: Uuid,
    cart_token: Option<&str>,
    customer_id: Option<Uuid>,
) -> Result<bool, Error> {
    let r = sqlx::query!(
        "SELECT o.customer_id, c.checkout_token_hash FROM orders o JOIN carts c ON c.id = o.cart_id
         WHERE o.id = $1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let by_customer = customer_id.is_some() && r.customer_id == customer_id;
    let by_cart = cart_token.is_some_and(|t| {
        capability::well_formed(t)
            && r.checkout_token_hash.as_deref() == Some(&capability::hash(t)[..])
    });
    Ok(by_customer || by_cart)
}

/// The customer an order belongs to (`None` for a guest order), `404` for unknown orders.
pub async fn customer_of(tx: &mut TenantTx, id: Uuid) -> Result<Option<Uuid>, Error> {
    sqlx::query_scalar!("SELECT customer_id FROM orders WHERE id = $1", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)
}

// ---------------------------------------------------------------------------------------
// Read models

/// A pickup point as chosen in the carrier's widget (snapshot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PickupPoint {
    /// The carrier's point id.
    pub id: String,
    pub name: String,
    pub street: String,
    pub city: String,
    pub zip: String,
    /// ISO 3166-1 alpha-2, upper case.
    pub country: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrderAddress {
    pub name: String,
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
    pub phone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderLineView {
    pub variant_id: Option<Uuid>,
    pub sku: String,
    pub name: String,
    pub options_label: String,
    pub quantity: i32,
    pub unit_price: MoneyView,
    /// This line's share of the coupon (A15).
    pub discount: MoneyView,
    pub total: MoneyView,
    pub tax_rate: String,
    pub tax: MoneyView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderChargeView {
    pub kind: ChargeKind,
    pub total: MoneyView,
    pub tax: MoneyView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShippingSummary {
    pub carrier: Carrier,
    /// The method's name in the order's locale, as it was at placement.
    pub name: String,
    pub pickup_point: Option<PickupPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AttemptView {
    pub id: Uuid,
    pub status: AttemptStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PaymentView {
    pub method: MethodKind,
    pub status: PaymentStatus,
    /// The latest attempt.
    pub attempt: Option<AttemptView>,
    /// Whether a new attempt can be started now (A10); only for a viewer who may pay.
    pub can_retry: bool,
    /// Whether this viewer may start or continue payments: the order token alone is read-only
    /// (A4); the browser that placed the order or its signed-in customer may pay.
    pub can_pay: bool,
    /// Unpaid orders are cancelled after this.
    pub expires_at: Option<DateTime<Utc>>,
    /// Bank transfer: account, variable symbol, amount and the QR code (A25).
    pub bank_transfer: Option<payments::bank::BankTransferView>,
}

/// An order as its customer sees it (`/o/<token>`, the account).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderView {
    pub id: Uuid,
    pub number: String,
    pub placed_at: DateTime<Utc>,
    pub status: OrderStatus,
    pub fulfillment_status: FulfillmentStatus,
    /// `late_payment` (A10) when money arrived for an expired or cancelled order.
    pub exception: Option<String>,
    pub email: String,
    pub phone: Option<String>,
    pub locale: String,
    pub currency: Currency,
    pub lines: Vec<OrderLineView>,
    pub charges: Vec<OrderChargeView>,
    pub subtotal: MoneyView,
    pub discount: MoneyView,
    pub coupon_code: Option<String>,
    pub shipping_total: MoneyView,
    pub payment_fee: MoneyView,
    pub rounding: MoneyView,
    pub vat: Vec<VatRow>,
    pub vat_total: MoneyView,
    pub total: MoneyView,
    pub shipping: ShippingSummary,
    pub billing_address: Option<OrderAddress>,
    pub shipping_address: Option<OrderAddress>,
    pub payment: PaymentView,
    pub notes: Option<String>,
}

/// The shipping method as it was at placement (`orders.shipping_method_snapshot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MethodSnapshot {
    pub id: Uuid,
    pub carrier: Carrier,
    pub name: String,
    pub price_minor: i64,
}

fn money(minor: i64, currency: Currency, locale: Locale) -> MoneyView {
    Money::new(minor, currency).view(locale)
}

/// The customer-facing view of an order.
pub async fn view(tx: &mut TenantTx, id: Uuid) -> Result<OrderView, Error> {
    let o = sqlx::query!(
        "SELECT id, number, placed_at, status, payment_status, fulfillment_status, exception, email,
                phone, locale, currency, subtotal_minor, discount_minor, shipping_minor,
                payment_fee_minor, tax_minor, rounding_minor, total_minor, vat_recap, coupon_code,
                shipping_method_snapshot, payment_method, pickup_point, notes, payment_expires_at
         FROM orders WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let currency = Currency::parse(&o.currency)
        .ok_or_else(|| Error::Internal(format!("stored currency {}", o.currency)))?;
    let locale = Locale::from_tag(&o.locale);
    let m = |minor: i64| money(minor, currency, locale);
    let bad = |e: serde_json::Error| Error::Internal(format!("stored order: {e}"));

    let lines = sqlx::query!(
        "SELECT variant_id, sku, name, options_label, quantity, unit_gross_minor, discount_minor,
                total_minor, tax_rate, tax_minor
         FROM order_lines WHERE order_id = $1 ORDER BY position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|l| OrderLineView {
        variant_id: l.variant_id,
        sku: l.sku,
        name: l.name,
        options_label: l.options_label,
        quantity: l.quantity,
        unit_price: m(l.unit_gross_minor),
        discount: m(l.discount_minor),
        total: m(l.total_minor),
        tax_rate: l.tax_rate,
        tax: m(l.tax_minor),
    })
    .collect();
    let charges = sqlx::query!(
        "SELECT kind, total_minor, tax_minor FROM order_charges WHERE order_id = $1
         ORDER BY CASE kind WHEN 'shipping' THEN 0 WHEN 'payment_fee' THEN 1 ELSE 2 END",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|c| {
        Ok(OrderChargeView {
            kind: serde_json::from_value(json!(c.kind)).map_err(bad)?,
            total: m(c.total_minor),
            tax: m(c.tax_minor),
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;
    let mut billing = None;
    let mut shipping = None;
    for a in sqlx::query!(
        "SELECT kind, name, company, street, city, postal_code, country, phone
         FROM order_addresses WHERE order_id = $1",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let addr = OrderAddress {
            name: a.name,
            company: a.company,
            street: a.street,
            city: a.city,
            postal_code: a.postal_code,
            country: a.country,
            phone: a.phone,
        };
        if a.kind == "billing" {
            billing = Some(addr);
        } else {
            shipping = Some(addr);
        }
    }
    let recap: Vec<VatRecapRow> = serde_json::from_value(o.vat_recap).map_err(bad)?;
    let snapshot: MethodSnapshot =
        serde_json::from_value(o.shipping_method_snapshot).map_err(bad)?;
    let pickup: Option<PickupPoint> = o
        .pickup_point
        .map(serde_json::from_value)
        .transpose()
        .map_err(bad)?;
    let method = MethodKind::parse(&o.payment_method)?;
    let status: OrderStatus = stored(&o.status)?;
    let payment_status: PaymentStatus = stored(&o.payment_status)?;
    let latest = payments::attempts(tx, id).await?.pop();
    let bank_transfer = match latest
        .as_ref()
        .filter(|a| a.method == MethodKind::BankTransfer)
    {
        Some(a) => sqlx::query_scalar!(
            "SELECT instructions FROM payment_attempts WHERE id = $1",
            a.id
        )
        .fetch_one(&mut **tx)
        .await?
        .map(|i| payments::bank::view(&i, locale))
        .transpose()?,
        None => None,
    };
    let can_retry = status == OrderStatus::Pending
        && !method.is_cod()
        && matches!(
            payment_status,
            PaymentStatus::Failed | PaymentStatus::Unpaid
        )
        && !latest
            .as_ref()
            .is_some_and(|a: &Attempt| a.status == AttemptStatus::Pending)
        && o.payment_expires_at.is_none_or(|e| e > Utc::now());
    Ok(OrderView {
        id: o.id,
        number: o.number.to_string(),
        placed_at: o.placed_at,
        status,
        fulfillment_status: stored(&o.fulfillment_status)?,
        exception: o.exception,
        email: o.email,
        phone: o.phone,
        locale: o.locale,
        currency,
        lines,
        charges,
        subtotal: m(o.subtotal_minor),
        discount: m(o.discount_minor),
        coupon_code: o.coupon_code,
        shipping_total: m(o.shipping_minor),
        payment_fee: m(o.payment_fee_minor),
        rounding: m(o.rounding_minor),
        vat: recap
            .iter()
            .map(|r| VatRow {
                rate: r.tax_rate.to_string(),
                net: m(r.net_minor),
                vat: m(r.vat_minor),
                gross: m(r.gross_minor),
            })
            .collect(),
        vat_total: m(o.tax_minor),
        total: m(o.total_minor),
        shipping: ShippingSummary {
            carrier: snapshot.carrier,
            name: snapshot.name,
            pickup_point: pickup,
        },
        billing_address: billing,
        shipping_address: shipping,
        payment: PaymentView {
            method,
            status: payment_status,
            attempt: latest.map(|a| AttemptView {
                id: a.id,
                status: a.status,
                created_at: a.created_at,
            }),
            can_retry,
            can_pay: true,
            expires_at: o.payment_expires_at,
            bank_transfer,
        },
        notes: o.notes,
    })
}

/// One row of an order list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderSummary {
    pub id: Uuid,
    pub number: String,
    pub placed_at: DateTime<Utc>,
    pub email: String,
    pub status: OrderStatus,
    pub payment_status: PaymentStatus,
    pub fulfillment_status: FulfillmentStatus,
    pub exception: Option<String>,
    pub payment_method: MethodKind,
    pub total: MoneyView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderPage {
    pub items: Vec<OrderSummary>,
    /// Pass as `cursor` for the next page; `null` on the last page.
    pub next_cursor: Option<Uuid>,
}

/// Which orders to list.
#[derive(Debug, Clone, Default)]
pub struct OrderFilter {
    pub customer_id: Option<Uuid>,
    pub status: Option<OrderStatus>,
    /// Only orders with an unresolved exception (`late_payment`, `duplicate_payment`): the
    /// refund work list.
    pub exception: bool,
}

/// Orders newest first; keyset-paginated by id (UUIDv7, so id order is placement order).
pub async fn list(
    tx: &mut TenantTx,
    filter: &OrderFilter,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<OrderPage, Error> {
    let limit = limit.clamp(1, 100);
    let rows = sqlx::query!(
        "SELECT id, number, placed_at, email, status, payment_status, fulfillment_status,
                exception, payment_method, total_minor, currency, locale
         FROM orders
         WHERE ($1::uuid IS NULL OR customer_id = $1)
           AND ($2::text IS NULL OR status = $2)
           AND ($3::uuid IS NULL OR id < $3)
           AND (NOT $5 OR (exception IS NOT NULL AND exception_resolved_at IS NULL))
         ORDER BY id DESC
         LIMIT $4",
        filter.customer_id,
        filter.status.map(OrderStatus::as_str),
        cursor,
        limit + 1,
        filter.exception
    )
    .fetch_all(&mut **tx)
    .await?;
    let more = rows.len() > usize::try_from(limit).unwrap_or(100);
    let items = rows
        .into_iter()
        .take(usize::try_from(limit).unwrap_or(100))
        .map(|r| {
            let currency = Currency::parse(&r.currency)
                .ok_or_else(|| Error::Internal(format!("stored currency {}", r.currency)))?;
            Ok(OrderSummary {
                id: r.id,
                number: r.number.to_string(),
                placed_at: r.placed_at,
                email: r.email,
                status: stored(&r.status)?,
                payment_status: stored(&r.payment_status)?,
                fulfillment_status: stored(&r.fulfillment_status)?,
                exception: r.exception,
                payment_method: MethodKind::parse(&r.payment_method)?,
                total: money(r.total_minor, currency, Locale::from_tag(&r.locale)),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(OrderPage {
        next_cursor: if more {
            items.last().map(|o| o.id)
        } else {
            None
        },
        items,
    })
}

/// One `order_events` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderEventView {
    pub kind: String,
    pub data: Value,
    pub actor: String,
    pub at: DateTime<Utc>,
}

/// The admin's read-only order detail (full management is WP12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AdminOrder {
    pub order: OrderView,
    /// When a person settled the exception (it then leaves the work list), and how.
    pub exception_resolved_at: Option<DateTime<Utc>>,
    pub exception_note: Option<String>,
    pub customer_id: Option<Uuid>,
    pub market_id: Uuid,
    pub ship_to_country: String,
    pub attempts: Vec<Attempt>,
    pub events: Vec<OrderEventView>,
}

pub async fn admin_detail(tx: &mut TenantTx, id: Uuid) -> Result<AdminOrder, Error> {
    let order = view(tx, id).await?;
    let o = sqlx::query!(
        "SELECT customer_id, market_id, ship_to_country, exception_resolved_at, exception_note
         FROM orders WHERE id = $1",
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    let events = sqlx::query_as!(
        OrderEventView,
        "SELECT kind, data, actor, at FROM order_events WHERE order_id = $1 ORDER BY at, id",
        id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(AdminOrder {
        order,
        exception_resolved_at: o.exception_resolved_at,
        exception_note: o.exception_note,
        customer_id: o.customer_id,
        market_id: o.market_id,
        ship_to_country: o.ship_to_country,
        attempts: payments::attempts(tx, id).await?,
        events,
    })
}
