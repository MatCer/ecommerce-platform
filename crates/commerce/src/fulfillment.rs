//! Fulfillment (spec §10.5, §10.6, A13, A16): shipments, labels, dispatch, delivery,
//! tracking, and the non-financial order edits allowed in M1.
//!
//! Every state change goes through the WP9 machines (order + fulfillment), writes the
//! timeline and is idempotent:
//! - label: a `creating` shipment row is committed before the carrier call and replaced by its
//!   outcome (a failed call cancels it, so the admin can retry); the label PDF goes to the
//!   private bucket (A21); the order moves to `processing`.
//! - dispatch (`ship`, manual or from tracking): reserved stock is committed (A13, movement
//!   identity `commit/order/<id>`), `order.shipped` is published (a COD order is invoiced on
//!   dispatch, A17) and the customer gets the tracking link.
//! - delivery: the order is `delivered`; cash on delivery becomes `delivered` (A16).
//! - returned to sender: the merchant confirms the parcel is back; its goods are restocked
//!   (A13) and an unpaid (COD) invoice is cancelled by a credit note.

use chrono::{DateTime, Utc};
use object_store::ObjectStoreExt;
use platform::Error;
use platform::db::TenantTx;
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::carriers::{self, CarrierKind, Carriers, ShipmentRequest, Tracked};
use crate::checkout::{CheckoutAddress, clean_address};
use crate::inventory::{self, MovementRef};
use crate::invoicing;
use crate::markets::invalid;
use crate::notifications::Template;
use crate::orders::{
    self, OrderRow,
    status::{
        FulfillmentCommand, FulfillmentStatus, OrderCommand, OrderStatus, ReturnSummary,
        fulfillment_transition,
    },
};
use crate::shipping::Carrier;
use crate::storefront::PublicUrls;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShipmentStatus {
    Creating,
    LabelCreated,
    Shipped,
    Delivered,
    Returned,
    Cancelled,
}

impl ShipmentStatus {
    fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "creating" => Self::Creating,
            "label_created" => Self::LabelCreated,
            "shipped" => Self::Shipped,
            "delivered" => Self::Delivered,
            "returned" => Self::Returned,
            "cancelled" => Self::Cancelled,
            other => return Err(Error::Internal(format!("shipment status {other}"))),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShipmentView {
    pub id: Uuid,
    pub carrier: Carrier,
    pub status: ShipmentStatus,
    pub carrier_ref: Option<String>,
    pub tracking_number: Option<String>,
    pub tracking_url: Option<String>,
    /// The carrier's last reported state.
    pub carrier_status: Option<String>,
    /// A label PDF is stored (`GET …/label`).
    pub has_label: bool,
    pub created_at: DateTime<Utc>,
    pub shipped_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
}

pub async fn shipments(tx: &mut TenantTx, order_id: Uuid) -> Result<Vec<ShipmentView>, Error> {
    sqlx::query!(
        "SELECT id, carrier, status, carrier_ref, tracking_number, tracking_url, carrier_status,
                label_key IS NOT NULL AS \"has_label!\", created_at, shipped_at, delivered_at
         FROM shipments WHERE order_id = $1 ORDER BY created_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(ShipmentView {
            id: r.id,
            carrier: Carrier::parse(&r.carrier)?,
            status: ShipmentStatus::parse(&r.status)?,
            carrier_ref: r.carrier_ref,
            tracking_number: r.tracking_number,
            tracking_url: r.tracking_url,
            carrier_status: r.carrier_status,
            has_label: r.has_label,
            created_at: r.created_at,
            shipped_at: r.shipped_at,
            delivered_at: r.delivered_at,
        })
    })
    .collect()
}

/// The order's live (non-cancelled) shipment.
async fn live(tx: &mut TenantTx, order_id: Uuid) -> Result<Option<ShipmentView>, Error> {
    Ok(shipments(tx, order_id)
        .await?
        .into_iter()
        .find(|s| s.status != ShipmentStatus::Cancelled))
}

async fn set_shipment(tx: &mut TenantTx, id: Uuid, status: ShipmentStatus) -> Result<(), Error> {
    let s = serde_json::to_value(status).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "UPDATE shipments SET status = $2,
             shipped_at = CASE WHEN $2 = 'shipped' THEN coalesce(shipped_at, now()) ELSE shipped_at END,
             delivered_at = CASE WHEN $2 = 'delivered' THEN coalesce(delivered_at, now())
                                 ELSE delivered_at END,
             updated_at = now()
         WHERE id = $1",
        id,
        s.as_str().unwrap_or_default()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn fulfillment_of(tx: &mut TenantTx, order_id: Uuid) -> Result<FulfillmentStatus, Error> {
    let s = sqlx::query_scalar!(
        "SELECT fulfillment_status FROM orders WHERE id = $1",
        order_id
    )
    .fetch_one(&mut **tx)
    .await?;
    s.parse()
        .map_err(|()| Error::Internal(format!("fulfillment status {s}")))
}

/// Applies a fulfillment command (WP9 machine) and records it on the timeline.
async fn apply_fulfillment(
    tx: &mut TenantTx,
    order_id: Uuid,
    command: FulfillmentCommand,
    actor: &str,
) -> Result<(), Error> {
    let from = fulfillment_of(tx, order_id).await?;
    let (next, events) = fulfillment_transition(from, command)?;
    sqlx::query!(
        "UPDATE orders SET fulfillment_status = $2, updated_at = now() WHERE id = $1",
        order_id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    orders::event(
        tx,
        order_id,
        "fulfillment_changed",
        &json!({ "from": from, "to": next, "events": events }),
        actor,
    )
    .await
}

fn order_status(o: &OrderRow) -> Result<OrderStatus, Error> {
    o.status
        .parse()
        .map_err(|()| Error::Internal(format!("order status {}", o.status)))
}

fn method_carrier(snapshot: &Value) -> Result<Carrier, Error> {
    Carrier::parse(
        snapshot
            .get("carrier")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------------------------
// Labels

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelInput {
    /// Parcel weight in grams; default: the variants' weights (at least 100 g).
    #[serde(default)]
    pub weight_g: Option<i64>,
}

/// Private-bucket key of a shipment's label.
pub fn label_key(tenant_id: Uuid, shipment_id: Uuid) -> String {
    format!("labels/{tenant_id}/{shipment_id}.pdf")
}

/// The private-bucket key of the live shipment's label.
pub async fn label_of(tx: &mut TenantTx, order_id: Uuid) -> Result<Option<String>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT label_key FROM shipments WHERE order_id = $1 AND status <> 'cancelled'",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten())
}

/// What the carrier needs, from the order.
async fn shipment_request(
    tx: &mut TenantTx,
    order_id: Uuid,
    weight_g: Option<i64>,
) -> Result<ShipmentRequest, Error> {
    let o = sqlx::query!(
        "SELECT number, email, phone, currency, total_minor, payment_method, pickup_point
         FROM orders WHERE id = $1",
        order_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let a = sqlx::query!(
        "SELECT name, company, street, city, postal_code, country, phone FROM order_addresses
         WHERE order_id = $1 ORDER BY CASE kind WHEN 'shipping' THEN 0 ELSE 1 END LIMIT 1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| invalid("address_required", "the order has no address"))?;
    let weight = match weight_g {
        Some(w) if (1..=50_000).contains(&w) => w,
        Some(_) => return Err(invalid("invalid_weight", "weight_g must be 1-50000")),
        None => sqlx::query_scalar!(
            r#"SELECT coalesce(sum(coalesce(v.weight_g, 0)::bigint * l.quantity), 0)::bigint AS "w!"
               FROM order_lines l LEFT JOIN variants v ON v.id = l.variant_id
               WHERE l.order_id = $1"#,
            order_id
        )
        .fetch_one(&mut **tx)
        .await?
        .max(100),
    };
    Ok(ShipmentRequest {
        reference: o.number.to_string(),
        recipient_name: a.name,
        company: a.company,
        email: o.email,
        phone: a.phone.or(o.phone),
        street: a.street,
        city: a.city,
        postal_code: a.postal_code,
        country: a.country,
        pickup_point: o
            .pickup_point
            .as_ref()
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        cod_minor: (o.payment_method == "cod").then_some(o.total_minor),
        value_minor: o.total_minor,
        currency: o.currency,
        weight_g: weight,
    })
}

/// Creates the shipment and its label at the carrier (personal pickup: no carrier). The order
/// must be confirmed or processing (paid, or cash on delivery) and not fulfilled yet.
///
/// Two phases, both outside any transaction, each persisted before the next: the carrier
/// accepts the shipment (its reference is stored), then the label is fetched and stored. A
/// failure after the first phase leaves the shipment `creating` with its reference, and calling
/// this again resumes with the label instead of creating a second shipment. A carrier that
/// refuses the shipment cancels the row (fix the order, retry); an unanswered first call leaves
/// the outcome unknown (`creating` without a reference: check the carrier portal, then cancel
/// the label and retry).
pub async fn create_label(
    db: &sqlx::PgPool,
    carriers: &Carriers,
    storage: &Storage,
    tenant_id: Uuid,
    actor: &str,
    order_id: Uuid,
    input: &LabelInput,
) -> Result<ShipmentView, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let o = orders::lock(&mut tx, order_id).await?;
    if !matches!(
        order_status(&o)?,
        OrderStatus::Confirmed | OrderStatus::Processing
    ) {
        return Err(Error::Conflict {
            code: "order_not_ready",
            detail: "only confirmed (paid or cash on delivery) orders can be shipped".into(),
        });
    }
    let snapshot = sqlx::query_scalar!(
        "SELECT shipping_method_snapshot FROM orders WHERE id = $1",
        order_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let carrier = method_carrier(&snapshot)?;
    let account = match CarrierKind::of(carrier) {
        Some(kind) => Some(carriers::account(&mut tx, carriers, kind).await?),
        None => None,
    };
    // Resume a shipment the carrier already accepted, or start a new one.
    let (id, mut announced) = match live(&mut tx, order_id).await? {
        Some(s) if s.status == ShipmentStatus::Creating => match s.carrier_ref.clone() {
            Some(carrier_ref) => (
                s.id,
                Some(carriers::Announced {
                    carrier_ref,
                    tracking_number: s.tracking_number.clone(),
                    tracking_url: s.tracking_url.clone(),
                }),
            ),
            None => {
                return Err(Error::Conflict {
                    code: "label_in_progress",
                    detail: "the carrier has not confirmed the shipment; check its portal, then \
                             cancel the label and retry"
                        .into(),
                });
            }
        },
        Some(_) => {
            return Err(Error::Conflict {
                code: "label_exists",
                detail: "the order already has a shipment; cancel it first".into(),
            });
        }
        None => {
            // The fulfillment machine must allow a label now (unfulfilled).
            fulfillment_transition(
                fulfillment_of(&mut tx, order_id).await?,
                FulfillmentCommand::CreateLabel,
            )?;
            let id = crate::id::new_id();
            sqlx::query!(
                "INSERT INTO shipments (id, tenant_id, order_id, carrier, status, created_by)
                 VALUES ($1, $2, $3, $4, 'creating', $5)",
                id,
                tenant_id,
                order_id,
                carrier.as_str(),
                actor
            )
            .execute(&mut *tx)
            .await?;
            (id, None)
        }
    };
    let req = shipment_request(&mut tx, order_id, input.weight_g).await?;
    tx.commit().await?;

    let mut label = None;
    if let Some(a) = &account {
        if announced.is_none() {
            match carriers.announce(a, carrier, &req).await {
                Ok(ann) => {
                    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
                    sqlx::query!(
                        "UPDATE shipments SET carrier_ref = $2, tracking_number = $3,
                             tracking_url = $4, updated_at = now()
                         WHERE id = $1 AND status = 'creating'",
                        id,
                        ann.carrier_ref,
                        ann.tracking_number,
                        ann.tracking_url
                    )
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    announced = Some(ann);
                }
                Err(e) => {
                    let refused = matches!(e, Error::Validation { .. });
                    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
                    sqlx::query!(
                        "UPDATE shipments SET carrier_status = $2,
                             status = CASE WHEN $3 THEN 'cancelled' ELSE status END,
                             updated_at = now()
                         WHERE id = $1 AND status = 'creating'",
                        id,
                        if refused {
                            e.to_string().chars().take(200).collect::<String>()
                        } else {
                            "carrier outcome unknown".to_owned()
                        },
                        refused
                    )
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Err(e);
                }
            }
        }
        let ann = announced
            .as_ref()
            .ok_or_else(|| Error::Internal("announced shipment missing".into()))?;
        // A failure here keeps the reference: the next call resumes with the label.
        let l = carriers.label(a, ann).await?;
        let key = label_key(tenant_id, id);
        storage
            .private
            .put(
                &object_store::path::Path::from(key.as_str()),
                l.pdf.clone().into(),
            )
            .await?;
        label = Some((l, key));
    }

    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let mut o = orders::lock(&mut tx, order_id).await?;
    let updated = sqlx::query!(
        "UPDATE shipments SET status = 'label_created', carrier_ref = $2, tracking_number = $3,
             tracking_url = $4, label_key = $5, carrier_status = NULL, updated_at = now()
         WHERE id = $1 AND status = 'creating'",
        id,
        label.as_ref().map(|(l, _)| l.carrier_ref.clone()),
        label.as_ref().map(|(l, _)| l.tracking_number.clone()),
        label.as_ref().map(|(l, _)| l.tracking_url.clone()),
        label.as_ref().map(|(_, k)| k.clone())
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        // Cancelled meanwhile by an admin: the carrier's shipment stays orphaned (logged).
        tracing::warn!(shipment = %id, "shipment cancelled while its label was being created");
        return Err(Error::Conflict {
            code: "label_cancelled",
            detail: "the shipment was cancelled while the label was being created".into(),
        });
    }
    apply_fulfillment(&mut tx, order_id, FulfillmentCommand::CreateLabel, actor).await?;
    if o.status == OrderStatus::Confirmed.as_str() {
        orders::apply_order(&mut tx, &mut o, OrderCommand::StartProcessing, actor).await?;
    }
    orders::event(
        &mut tx,
        order_id,
        "label_created",
        &json!({ "shipment_id": id, "carrier": carrier,
                 "tracking_number": label.as_ref().map(|(l, _)| &l.tracking_number) }),
        actor,
    )
    .await?;
    let view = live(&mut tx, order_id).await?.ok_or(Error::NotFound)?;
    tx.commit().await?;
    Ok(view)
}

/// Cancels the live shipment before dispatch (a failed or unwanted label). The carrier-side
/// packet is not cancelled (M1: cancel it in the carrier's portal if needed).
pub async fn cancel_label(tx: &mut TenantTx, actor: &str, order_id: Uuid) -> Result<(), Error> {
    orders::lock(tx, order_id).await?;
    let s = live(tx, order_id).await?.ok_or_else(|| Error::Conflict {
        code: "no_shipment",
        detail: "the order has no shipment to cancel".into(),
    })?;
    if !matches!(
        s.status,
        ShipmentStatus::Creating | ShipmentStatus::LabelCreated
    ) {
        return Err(Error::Conflict {
            code: "already_shipped",
            detail: "the parcel has left; it can no longer be cancelled".into(),
        });
    }
    set_shipment(tx, s.id, ShipmentStatus::Cancelled).await?;
    if s.status == ShipmentStatus::LabelCreated {
        apply_fulfillment(tx, order_id, FulfillmentCommand::CancelLabel, actor).await?;
    }
    orders::event(
        tx,
        order_id,
        "label_cancelled",
        &json!({ "shipment_id": s.id }),
        actor,
    )
    .await
}

// ---------------------------------------------------------------------------------------
// Dispatch and delivery

/// Starts packing a confirmed order (`confirmed → processing`).
pub async fn start_processing(tx: &mut TenantTx, actor: &str, order_id: Uuid) -> Result<(), Error> {
    let mut o = orders::lock(tx, order_id).await?;
    orders::apply_order(tx, &mut o, OrderCommand::StartProcessing, actor).await
}

async fn commit_stock(tx: &mut TenantTx, order_id: Uuid) -> Result<(), Error> {
    let lines = sqlx::query!(
        r#"SELECT variant_id AS "variant_id!", sum(quantity)::int AS "quantity!"
           FROM order_lines WHERE order_id = $1 AND variant_id IS NOT NULL
           GROUP BY variant_id ORDER BY variant_id"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let ref_id = order_id.to_string();
    let r = MovementRef {
        ref_type: "order",
        ref_id: &ref_id,
    };
    for l in lines {
        inventory::commit(tx, &r, l.variant_id, l.quantity).await?;
    }
    Ok(())
}

/// The carrier took the parcel (manual, or seen by tracking): stock commit (A13),
/// `order.shipped` (COD is invoiced on dispatch, A17), the shipped email.
pub async fn ship(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    actor: &str,
    order_id: Uuid,
) -> Result<(), Error> {
    let mut o = orders::lock(tx, order_id).await?;
    let s = live(tx, order_id).await?.ok_or_else(|| Error::Conflict {
        code: "no_shipment",
        detail: "create the label (shipment) first".into(),
    })?;
    if s.status != ShipmentStatus::LabelCreated {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: format!("the shipment is {:?}", s.status).to_lowercase(),
        });
    }
    apply_fulfillment(tx, order_id, FulfillmentCommand::Ship, actor).await?;
    orders::apply_order(tx, &mut o, OrderCommand::Ship, actor).await?;
    set_shipment(tx, s.id, ShipmentStatus::Shipped).await?;
    commit_stock(tx, order_id).await?;
    platform::queue::publish(
        &mut **tx,
        orders::SHIPPED_EVENT,
        &json!({ "order_id": o.id, "number": o.number.to_string(), "shipment_id": s.id,
                 "carrier": s.carrier, "tracking_number": s.tracking_number }),
    )
    .await?;
    let pickup = sqlx::query_scalar!("SELECT pickup_point FROM orders WHERE id = $1", order_id)
        .fetch_one(&mut **tx)
        .await?
        .and_then(|p| {
            Some(format!(
                "{}, {}, {}",
                p.get("name")?.as_str()?,
                p.get("street")?.as_str()?,
                p.get("city")?.as_str()?
            ))
        });
    let method = sqlx::query_scalar!(
        "SELECT shipping_method_snapshot->>'name' FROM orders WHERE id = $1",
        order_id
    )
    .fetch_one(&mut **tx)
    .await?
    .unwrap_or_default();
    orders::mail::send(
        tx,
        urls,
        order_id,
        Template::OrderShipped,
        json!({ "shipment": { "carrier": method, "tracking_number": s.tracking_number,
                              "tracking_url": s.tracking_url, "pickup_point": pickup } }),
        format!("order_shipped:{}", s.id),
        &[],
    )
    .await
}

/// The parcel reached the customer: order `delivered`, COD `delivered` (A16), email.
pub async fn deliver(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    actor: &str,
    order_id: Uuid,
) -> Result<(), Error> {
    let mut o = orders::lock(tx, order_id).await?;
    let s = live(tx, order_id).await?.ok_or_else(|| Error::Conflict {
        code: "no_shipment",
        detail: "the order has no shipment".into(),
    })?;
    if s.status != ShipmentStatus::Shipped {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: "only a shipped parcel can be delivered".into(),
        });
    }
    apply_fulfillment(tx, order_id, FulfillmentCommand::Deliver, actor).await?;
    orders::apply_order(tx, &mut o, OrderCommand::Deliver, actor).await?;
    set_shipment(tx, s.id, ShipmentStatus::Delivered).await?;
    if o.payment_method == "cod" {
        // A COD whose cash was already reported collected stays as it is.
        let cod = sqlx::query_scalar!(
            "SELECT cod_status FROM payment_attempts WHERE order_id = $1 AND method = 'cod'
             ORDER BY created_at DESC LIMIT 1",
            order_id
        )
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
        if cod.as_deref() == Some("pending") {
            crate::payments::cod::deliver(tx, actor, order_id).await?;
        }
    }
    orders::mail::send(
        tx,
        urls,
        order_id,
        Template::OrderDelivered,
        json!({}),
        format!("order_delivered:{}", s.id),
        &[],
    )
    .await
}

/// The parcel came back undelivered (refused, not collected) and the merchant confirms it
/// arrived: goods restocked (A13), the order `returned`; an invoiced but unpaid (COD) order
/// gets a credit note. Paid orders are refunded separately (the refund issues the credit note).
pub async fn returned_to_sender(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut o = orders::lock(tx, order_id).await?;
    let s = live(tx, order_id).await?.ok_or_else(|| Error::Conflict {
        code: "no_shipment",
        detail: "the order has no shipment".into(),
    })?;
    if !matches!(
        s.status,
        ShipmentStatus::Shipped | ShipmentStatus::Delivered
    ) {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: "only a dispatched parcel can come back".into(),
        });
    }
    let unpaid = !matches!(
        o.payment_status.as_str(),
        "paid" | "partially_refunded" | "refunded"
    );
    // The correction needs the dispatch invoice (A17): refuse before anything changes.
    if unpaid
        && invoicing::invoice_of(tx, order_id).await?.is_none()
        && invoicing::expects_invoice(tx, order_id).await?
    {
        return Err(Error::Conflict {
            code: "invoice_pending",
            detail: "the order's invoice is being issued; retry in a moment".into(),
        });
    }
    apply_fulfillment(tx, order_id, FulfillmentCommand::ReturnToSender, actor).await?;
    orders::apply_order(
        tx,
        &mut o,
        OrderCommand::MarkReturned(ReturnSummary::Full),
        actor,
    )
    .await?;
    set_shipment(tx, s.id, ShipmentStatus::Returned).await?;
    // Units a withdrawal already brought back were restocked there (A13: once).
    let lines = sqlx::query!(
        r#"SELECT l.variant_id AS "variant_id!",
                  sum(l.quantity - coalesce((SELECT sum(r.quantity) FROM return_lines r
                      WHERE r.order_line_id = l.id
                        AND r.status IN ('received', 'refunded')), 0))::int AS "quantity!"
           FROM order_lines l WHERE l.order_id = $1 AND l.variant_id IS NOT NULL
           GROUP BY l.variant_id ORDER BY l.variant_id"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let ref_id = s.id.to_string();
    let r = MovementRef {
        ref_type: "returned_parcel",
        ref_id: &ref_id,
    };
    for l in lines.into_iter().filter(|l| l.quantity > 0) {
        inventory::restock(tx, actor, &r, l.variant_id, l.quantity).await?;
    }
    // Withdrawn goods came back with the parcel (restocked just above): their withdrawals
    // count as received, so they can be refunded.
    crate::withdrawals::received_with_parcel(tx, order_id).await?;
    if unpaid && invoicing::invoice_of(tx, order_id).await?.is_some() {
        let src = invoicing::order_source(tx, order_id).await?;
        let mut doc_lines = Vec::new();
        for l in &src.lines {
            doc_lines.push(invoicing::document::refunded_goods_line(
                l,
                l.quantity,
                l.total_minor,
                l.tax_minor,
            ));
        }
        for c in &src.charges {
            doc_lines.extend(invoicing::document::refunded_charge_lines(
                c,
                &src.locale,
                src.vat_payer,
            ));
        }
        invoicing::issue_credit_note(
            tx,
            order_id,
            doc_lines,
            Some("returned_to_sender".into()),
            actor,
            now,
        )
        .await?;
    }
    orders::event(
        tx,
        order_id,
        "returned_to_sender",
        &json!({ "shipment_id": s.id }),
        actor,
    )
    .await
}

// ---------------------------------------------------------------------------------------
// Tracking

/// Polls the carriers for dispatched parcels (cron): a parcel the carrier took is shipped, a
/// delivered one delivered; returns are only flagged (the merchant confirms receipt). One
/// failing shipment does not stop the others. Returns how many were polled.
pub async fn track_due(
    db: &sqlx::PgPool,
    carriers: &Carriers,
    urls: &PublicUrls,
    max: i32,
) -> Result<usize, Error> {
    let due = sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", shipment_id AS "shipment_id!"
           FROM platform.trackable_shipments($1)"#,
        max
    )
    .fetch_all(db)
    .await?;
    let mut polled = 0;
    for d in due {
        match track_one(db, carriers, urls, d.tenant_id, d.shipment_id).await {
            Ok(()) => polled += 1,
            Err(e) => {
                tracing::warn!(tenant = %d.tenant_id, shipment = %d.shipment_id, error = %e,
                    "tracking poll failed");
                // Back off this shipment until the next round.
                if let Ok(mut tx) = platform::db::tenant_tx(db, d.tenant_id).await {
                    let _ = sqlx::query!(
                        "UPDATE shipments SET tracked_at = now() WHERE id = $1",
                        d.shipment_id
                    )
                    .execute(&mut *tx)
                    .await;
                    let _ = tx.commit().await;
                }
            }
        }
    }
    Ok(polled)
}

async fn track_one(
    db: &sqlx::PgPool,
    carriers: &Carriers,
    urls: &PublicUrls,
    tenant_id: Uuid,
    shipment_id: Uuid,
) -> Result<(), Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let s = sqlx::query!(
        "SELECT order_id, carrier, carrier_ref, status FROM shipments WHERE id = $1",
        shipment_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let Some(carrier_ref) = s.carrier_ref else {
        return Ok(());
    };
    let Some(kind) = CarrierKind::of(Carrier::parse(&s.carrier)?) else {
        return Ok(());
    };
    let account = carriers::account(&mut tx, carriers, kind).await?;
    tx.commit().await?;
    let (state, text) = carriers.track(&account, &carrier_ref).await?;

    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    orders::lock(&mut tx, s.order_id).await?;
    let before = sqlx::query!(
        "SELECT status, carrier_status FROM shipments WHERE id = $1",
        shipment_id
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE shipments SET carrier_status = $2, tracked_at = now() WHERE id = $1",
        shipment_id,
        text
    )
    .execute(&mut *tx)
    .await?;
    let (status, previous) = (before.status, before.carrier_status);
    let label_only = status == "label_created";
    match state {
        Tracked::Announced => {}
        Tracked::InTransit if label_only => ship(&mut tx, urls, "carrier", s.order_id).await?,
        Tracked::InTransit => {}
        Tracked::Delivered => {
            if label_only {
                ship(&mut tx, urls, "carrier", s.order_id).await?;
            }
            if label_only || status == "shipped" {
                deliver(&mut tx, urls, "carrier", s.order_id).await?;
            }
        }
        Tracked::Returned | Tracked::Cancelled => {
            if previous.as_deref() != Some(text.as_str()) {
                orders::event(
                    &mut tx,
                    s.order_id,
                    "carrier_exception",
                    &json!({ "shipment_id": shipment_id, "state": state, "carrier_status": text }),
                    "carrier",
                )
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Non-financial edits (A13)

/// A staff note on the timeline (audited).
pub async fn add_note(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    note: &str,
) -> Result<(), Error> {
    let note = note.trim();
    if note.is_empty() || note.chars().count() > 2000 {
        return Err(invalid(
            "invalid_note",
            "a note of 1-2000 characters is required",
        ));
    }
    orders::lock(tx, order_id).await?;
    orders::event(tx, order_id, "note", &json!({ "note": note }), actor).await
}

/// Changes the shipping address while no label exists (audited, old value on the timeline).
pub async fn update_shipping_address(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    input: &CheckoutAddress,
) -> Result<(), Error> {
    let a = clean_address(input)?;
    let o = orders::lock(tx, order_id).await?;
    if matches!(
        o.status.as_str(),
        "shipped" | "delivered" | "cancelled" | "returned"
    ) || live(tx, order_id).await?.is_some()
    {
        return Err(Error::Conflict {
            code: "address_locked",
            detail: "the address can only change before a label exists".into(),
        });
    }
    let country_ok = sqlx::query_scalar!(
        r#"SELECT $2 = ANY (m.country_codes) AS "ok!" FROM orders o
           JOIN markets m ON m.id = o.market_id WHERE o.id = $1"#,
        order_id,
        a.country
    )
    .fetch_one(&mut **tx)
    .await?;
    if !country_ok {
        return Err(invalid(
            "ship_to_not_allowed",
            "the order's market does not ship there",
        ));
    }
    let old = sqlx::query!(
        "SELECT name, street, city, postal_code, country FROM order_addresses
         WHERE order_id = $1 AND kind = 'shipping'",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|r| {
        json!({ "name": r.name, "street": r.street, "city": r.city,
                     "postal_code": r.postal_code, "country": r.country })
    });
    // The VAT of an order depends on its destination: M1 allows no financial edits (A13).
    let same_country =
        sqlx::query_scalar!("SELECT ship_to_country FROM orders WHERE id = $1", order_id)
            .fetch_one(&mut **tx)
            .await?
            == a.country;
    if !same_country {
        return Err(invalid(
            "country_change_not_allowed",
            "changing the destination country changes VAT: cancel and place a new order",
        ));
    }
    sqlx::query!(
        "INSERT INTO order_addresses (tenant_id, order_id, kind, name, company, street, city,
             postal_code, country, phone)
         VALUES ($1, $2, 'shipping', $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (tenant_id, order_id, kind) DO UPDATE SET name = EXCLUDED.name,
             company = EXCLUDED.company, street = EXCLUDED.street, city = EXCLUDED.city,
             postal_code = EXCLUDED.postal_code, country = EXCLUDED.country,
             phone = EXCLUDED.phone",
        tx.tenant_id(),
        order_id,
        a.name,
        a.company,
        a.street,
        a.city,
        a.postal_code,
        a.country,
        a.phone
    )
    .execute(&mut **tx)
    .await?;
    let data = json!({ "from": old, "to": a });
    orders::event(tx, order_id, "address_changed", &data, actor).await?;
    audit::record(
        tx,
        actor,
        "order.address_changed",
        "order",
        Some(&order_id.to_string()),
        &data,
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// What an admin can do now

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct OrderActions {
    pub start_processing: bool,
    pub create_label: bool,
    pub cancel_label: bool,
    pub ship: bool,
    pub deliver: bool,
    pub returned_to_sender: bool,
    pub cancel: bool,
    pub refund: bool,
    pub edit_address: bool,
}

pub async fn actions(tx: &mut TenantTx, order_id: Uuid) -> Result<OrderActions, Error> {
    let o = sqlx::query!(
        "SELECT status, payment_status FROM orders WHERE id = $1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let shipment = live(tx, order_id).await?;
    // A shipment the carrier accepted but whose label is missing resumes (no second one).
    let resumable = shipment
        .as_ref()
        .is_some_and(|s| s.status == ShipmentStatus::Creating && s.carrier_ref.is_some());
    let s = shipment.map(|s| s.status);
    let status = o.status.as_str();
    let open = matches!(status, "pending" | "confirmed" | "processing");
    let paid = matches!(o.payment_status.as_str(), "paid" | "partially_refunded");
    Ok(OrderActions {
        start_processing: status == "confirmed",
        create_label: matches!(status, "confirmed" | "processing") && (s.is_none() || resumable),
        cancel_label: matches!(
            s,
            Some(ShipmentStatus::Creating | ShipmentStatus::LabelCreated)
        ),
        ship: s == Some(ShipmentStatus::LabelCreated),
        deliver: s == Some(ShipmentStatus::Shipped),
        returned_to_sender: matches!(s, Some(ShipmentStatus::Shipped | ShipmentStatus::Delivered)),
        cancel: open
            && !matches!(
                s,
                Some(
                    ShipmentStatus::Shipped | ShipmentStatus::Delivered | ShipmentStatus::Returned
                )
            ),
        refund: paid,
        edit_address: open && s.is_none(),
    })
}
