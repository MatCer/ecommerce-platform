//! Refunds of orders (spec §10.4, A10, A13, A15, A17, A19): by line and quantity plus the
//! shipping and payment fee charges, reversing the persisted allocations (A15, the residual
//! goes to the last units), paid back through the original payment (Stripe via the API with
//! the application fee refunded proportionally; bank transfer and cash on delivery recorded
//! with the customer's IBAN), with a credit note referencing the original invoice (A17).
//!
//! Order of effects: the refund row (with what it covers) is committed first; a Stripe refund
//! is then submitted outside any transaction; unless the provider rejected it, the credit note
//! and the refund email follow in one transaction. A rejected refund leaves no credit note and
//! frees its allocations for another try.
//!
//! Also here: cancelling an order (release stock, refund what was paid) and returning money
//! the order cannot keep (A10 late/duplicate payments).

use chrono::Utc;
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::invoicing::{
    self,
    document::{
        DocLine, RefundLine, Reversed, refunded_charge_lines, refunded_goods_line, reverse_units,
    },
};
use crate::markets::invalid;
use crate::money::{Currency, Locale, Money, MoneyView};
use crate::notifications::Template;
use crate::orders::{self, status::OrderCommand};
use crate::payments::{self, Payments, Refund, RefundDetails, RefundStatus};
use crate::pricing::cart::ChargeKind;
use crate::storefront::PublicUrls;

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundInput {
    /// Goods to refund: more units of order lines.
    #[serde(default)]
    pub lines: Vec<RefundLine>,
    /// Refund the whole shipping charge.
    #[serde(default)]
    pub shipping: bool,
    /// Refund the whole payment fee.
    #[serde(default)]
    pub payment_fee: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// Where a bank refund goes (bank transfer, cash on delivery), shown to the person paying it.
    #[serde(default)]
    pub iban: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlannedLine {
    pub order_line_id: Option<Uuid>,
    /// `goods`, `shipping`, `payment_fee`, `rounding`.
    pub kind: String,
    pub name: String,
    pub quantity: i32,
    pub amount: MoneyView,
}

/// What a refund would return (also the admin preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RefundPlan {
    pub lines: Vec<PlannedLine>,
    pub amount: MoneyView,
    /// Everything the customer paid is refunded after this.
    pub full: bool,
    #[serde(skip)]
    doc_lines: Vec<DocLine>,
    #[serde(skip)]
    ledger: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RefundOutcome {
    pub refund: Refund,
    pub credit_note_id: Option<Uuid>,
    pub plan: RefundPlan,
}

/// What earlier refunds (not rejected, or with a credit note) already reversed: per order line
/// and per charge.
async fn reversed(
    tx: &mut TenantTx,
    order_id: Uuid,
) -> Result<(std::collections::HashMap<Uuid, Reversed>, Vec<String>), Error> {
    let rows = sqlx::query_scalar!(
        r#"SELECT lines AS "lines!" FROM refunds
           WHERE order_id = $1 AND lines IS NOT NULL
             AND (status <> 'failed' OR credit_note_id IS NOT NULL)"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut lines: std::collections::HashMap<Uuid, Reversed> = Default::default();
    let mut charges = Vec::new();
    for entry in rows.iter().filter_map(Value::as_array).flatten() {
        if let Some(charge) = entry.get("charge").and_then(Value::as_str) {
            charges.push(charge.to_owned());
            continue;
        }
        let Some(id) = entry
            .get("order_line_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
        else {
            continue;
        };
        let n = |k: &str| entry.get(k).and_then(Value::as_i64).unwrap_or(0);
        let r = lines.entry(id).or_default();
        r.quantity += i32::try_from(n("quantity")).unwrap_or(0);
        r.gross_minor += n("gross_minor");
        r.vat_minor += n("vat_minor");
    }
    Ok((lines, charges))
}

/// Plans a refund (A15) without writing anything.
pub async fn plan(
    tx: &mut TenantTx,
    order_id: Uuid,
    input: &RefundInput,
) -> Result<RefundPlan, Error> {
    let src = invoicing::order_source(tx, order_id).await?;
    let loc = Locale::from_tag(&src.locale);
    let money = |m: i64| Money::new(m, src.currency).view(loc);
    let (done, charges_done) = reversed(tx, order_id).await?;
    let mut seen = std::collections::HashSet::new();
    let mut planned = Vec::new();
    let mut doc_lines = Vec::new();
    let mut ledger = Vec::new();
    for r in &input.lines {
        if !seen.insert(r.order_line_id) {
            return Err(invalid("invalid_refund", "each order line at most once"));
        }
        let l = src
            .lines
            .iter()
            .find(|l| l.id == r.order_line_id)
            .ok_or_else(|| invalid("invalid_refund", "unknown order line"))?;
        let d = done.get(&l.id).copied().unwrap_or_default();
        if r.quantity <= 0 || r.quantity > l.quantity - d.quantity {
            return Err(invalid(
                "invalid_refund",
                format!(
                    "{}: at most {} more unit(s) can be refunded",
                    l.sku,
                    (l.quantity - d.quantity).max(0)
                ),
            ));
        }
        let (gross, vat) = reverse_units(l.quantity, l.total_minor, l.tax_minor, d, r.quantity);
        planned.push(PlannedLine {
            order_line_id: Some(l.id),
            kind: "goods".into(),
            name: l.name.clone(),
            quantity: r.quantity,
            amount: money(gross),
        });
        doc_lines.push(refunded_goods_line(l, r.quantity, gross, vat));
        ledger.push(json!({ "order_line_id": l.id, "quantity": r.quantity,
                            "gross_minor": gross, "vat_minor": vat }));
    }
    let lines_full = src.lines.iter().all(|l| {
        let d = done.get(&l.id).map_or(0, |d| d.quantity);
        let now = input
            .lines
            .iter()
            .find(|r| r.order_line_id == l.id)
            .map_or(0, |r| r.quantity);
        d + now >= l.quantity
    });
    let mut wanted = Vec::new();
    if input.shipping {
        wanted.push(ChargeKind::Shipping);
    }
    if input.payment_fee {
        wanted.push(ChargeKind::PaymentFee);
    }
    let name_of = |k: ChargeKind| match k {
        ChargeKind::Shipping => "shipping",
        ChargeKind::PaymentFee => "payment_fee",
        ChargeKind::Rounding => "rounding",
    };
    for kind in &wanted {
        if charges_done.iter().any(|c| c == name_of(*kind)) {
            return Err(invalid(
                "invalid_refund",
                format!("the {} charge was already refunded", name_of(*kind)),
            ));
        }
    }
    let charges_full = src.charges.iter().all(|c| {
        c.kind == ChargeKind::Rounding
            || wanted.contains(&c.kind)
            || charges_done.iter().any(|d| d == name_of(c.kind))
    });
    let full = lines_full && charges_full;
    for c in &src.charges {
        // Cash rounding goes back with the last refund, so a full refund returns what was paid.
        let include = wanted.contains(&c.kind)
            || (c.kind == ChargeKind::Rounding
                && full
                && !charges_done.iter().any(|d| d == "rounding"));
        if !include {
            continue;
        }
        planned.push(PlannedLine {
            order_line_id: None,
            kind: name_of(c.kind).into(),
            name: c.name.clone(),
            quantity: 1,
            amount: money(c.total_minor),
        });
        doc_lines.extend(refunded_charge_lines(c, &src.locale, src.vat_payer));
        ledger.push(
            json!({ "charge": name_of(c.kind), "gross_minor": c.total_minor,
                            "vat_minor": c.tax_minor }),
        );
    }
    let amount: i64 = doc_lines.iter().map(|l| l.gross_minor).sum();
    if planned.is_empty() {
        return Err(invalid("invalid_refund", "choose what to refund"));
    }
    Ok(RefundPlan {
        lines: planned,
        amount: money(amount),
        full,
        doc_lines,
        ledger: Value::Array(ledger),
    })
}

fn clean_iban(iban: Option<&str>) -> Result<Option<String>, Error> {
    match iban.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(raw) => payments::bank::normalize_iban(raw)
            .map(Some)
            .ok_or_else(|| invalid("invalid_iban", "the IBAN is not valid")),
    }
}

/// Refunds part or all of a paid order (see the module docs). `withdrawal_id`: the refund of an
/// A19 withdrawal (its lines).
#[allow(clippy::too_many_arguments)]
pub async fn refund_order(
    db: &sqlx::PgPool,
    payments_cfg: &Payments,
    urls: &PublicUrls,
    tenant_id: Uuid,
    actor: &str,
    order_id: Uuid,
    input: &RefundInput,
    withdrawal_id: Option<Uuid>,
) -> Result<RefundOutcome, Error> {
    let iban = clean_iban(input.iban.as_deref())?;
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let mut order = orders::lock(&mut tx, order_id).await?;
    if !matches!(order.payment_status.as_str(), "paid" | "partially_refunded") {
        return Err(Error::Conflict {
            code: "nothing_to_refund",
            detail: "the order is not paid".into(),
        });
    }
    let attempt = sqlx::query_scalar!("SELECT paid_attempt_id FROM orders WHERE id = $1", order_id)
        .fetch_one(&mut *tx)
        .await?
        .ok_or_else(|| Error::Conflict {
            code: "nothing_to_refund",
            detail: "the order has no retained payment".into(),
        })?;
    // The credit note needs the invoice: refuse before any money moves (A17).
    if invoicing::invoice_of(&mut tx, order_id).await?.is_none()
        && invoicing::expects_invoice(&mut tx, order_id).await?
    {
        return Err(Error::Conflict {
            code: "invoice_pending",
            detail: "the order's invoice is being issued; retry in a moment".into(),
        });
    }
    let plan = plan(&mut tx, order_id, input).await?;
    let (refund_id, manual) = payments::record_refund(
        &mut tx,
        &mut order,
        attempt,
        plan.amount.amount_minor,
        input.reason.as_deref(),
        actor,
        &RefundDetails {
            lines: Some(plan.ledger.clone()),
            withdrawal_id,
            iban: iban.clone(),
        },
    )
    .await?;
    tx.commit().await?;

    let submitted = if manual {
        None
    } else {
        Some(payments::submit_refund(db, payments_cfg, tenant_id, refund_id).await)
    };
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let refund = payments::refund_row(&mut tx, refund_id).await?;
    if refund.status == RefundStatus::Failed {
        tx.commit().await?;
        return Err(match submitted {
            Some(Err(e)) => e,
            _ => Error::Conflict {
                code: "refund_rejected",
                detail: "the payment provider rejected the refund".into(),
            },
        });
    }
    orders::lock(&mut tx, order_id).await?;
    let credit_note = invoicing::issue_credit_note(
        &mut tx,
        order_id,
        plan.doc_lines.clone(),
        input.reason.clone(),
        actor,
        Utc::now(),
    )
    .await?;
    if let Some(cn) = credit_note {
        sqlx::query!(
            "UPDATE refunds SET credit_note_id = $2 WHERE id = $1",
            refund_id,
            cn
        )
        .execute(&mut *tx)
        .await?;
    }
    orders::event(
        &mut tx,
        order_id,
        "refunded",
        &json!({ "refund_id": refund_id, "amount_minor": plan.amount.amount_minor,
                 "lines": plan.ledger, "credit_note_id": credit_note, "status": refund.status }),
        actor,
    )
    .await?;
    let method = match (refund.status, manual, &iban) {
        (_, false, _) => "card",
        (_, true, Some(_)) => "bank",
        _ => "other",
    };
    orders::mail::send(
        &mut tx,
        urls,
        order_id,
        Template::OrderRefunded,
        json!({ "refund": {
            "amount": plan.amount.formatted,
            "method": method,
            "iban": iban.clone().unwrap_or_default(),
            "lines": plan.lines.iter().map(|l| json!({
                "name": l.name, "quantity": l.quantity, "amount": l.amount.formatted,
            })).collect::<Vec<_>>(),
        } }),
        format!("order_refunded:{refund_id}"),
        &[],
    )
    .await?;
    let refund = payments::refund_row(&mut tx, refund_id).await?;
    tx.commit().await?;
    // A pending Stripe refund answered "unavailable": recorded, reported as pending.
    if let Some(Err(e)) = submitted
        && refund.status != RefundStatus::Pending
    {
        return Err(e);
    }
    Ok(RefundOutcome {
        refund,
        credit_note_id: credit_note,
        plan,
    })
}

/// Everything still refundable of an order: all remaining units, and the charges.
pub async fn full_input(tx: &mut TenantTx, order_id: Uuid) -> Result<RefundInput, Error> {
    let src = invoicing::order_source(tx, order_id).await?;
    let (done, charges_done) = reversed(tx, order_id).await?;
    let lines = src
        .lines
        .iter()
        .filter_map(|l| {
            let left = l.quantity - done.get(&l.id).map_or(0, |d| d.quantity);
            (left > 0).then_some(RefundLine {
                order_line_id: l.id,
                quantity: left,
            })
        })
        .collect();
    let has = |k: ChargeKind, name: &str| {
        src.charges.iter().any(|c| c.kind == k) && !charges_done.iter().any(|d| d == name)
    };
    Ok(RefundInput {
        lines,
        shipping: has(ChargeKind::Shipping, "shipping"),
        payment_fee: has(ChargeKind::PaymentFee, "payment_fee"),
        reason: None,
        iban: None,
    })
}

// ---------------------------------------------------------------------------------------
// Cancellation

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelInput {
    #[serde(default)]
    pub reason: Option<String>,
    /// Bank transfer / COD: where the refund of a paid order goes.
    #[serde(default)]
    pub iban: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CancelOutcome {
    /// The refund of a paid order (with its credit note).
    pub refund: Option<RefundOutcome>,
}

/// Cancels an order that has not left (A13): open payment attempts end, reserved stock and the
/// coupon are released, a label not yet handed over is voided, the customer is told. A paid
/// order is then refunded in full with a credit note (A17.1).
#[allow(clippy::too_many_arguments)]
pub async fn cancel(
    db: &sqlx::PgPool,
    payments_cfg: &Payments,
    urls: &PublicUrls,
    tenant_id: Uuid,
    actor: &str,
    order_id: Uuid,
    input: &CancelInput,
) -> Result<CancelOutcome, Error> {
    let reason = input
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if reason.is_some_and(|r| r.chars().count() > 500) {
        return Err(invalid(
            "invalid_cancel",
            "the reason is at most 500 characters",
        ));
    }
    let iban = clean_iban(input.iban.as_deref())?;
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let mut o = orders::lock(&mut tx, order_id).await?;
    let paid = matches!(o.payment_status.as_str(), "paid" | "partially_refunded");
    if paid
        && invoicing::invoice_of(&mut tx, order_id).await?.is_none()
        && invoicing::expects_invoice(&mut tx, order_id).await?
    {
        return Err(Error::Conflict {
            code: "invoice_pending",
            detail: "the order's invoice is being issued; retry in a moment".into(),
        });
    }
    let live = sqlx::query!(
        "SELECT id, status FROM shipments WHERE order_id = $1 AND status <> 'cancelled'",
        order_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(s) = &live {
        if !matches!(s.status.as_str(), "creating" | "label_created") {
            return Err(Error::Conflict {
                code: "already_shipped",
                detail: "the parcel has left: refund a return instead".into(),
            });
        }
        crate::fulfillment::cancel_label(&mut tx, actor, order_id).await?;
    }
    orders::apply_order(&mut tx, &mut o, OrderCommand::Cancel, actor).await?;
    sqlx::query!(
        "UPDATE payment_attempts SET status = 'expired', completed_at = now(), updated_at = now()
         WHERE order_id = $1 AND status = 'pending'",
        order_id
    )
    .execute(&mut *tx)
    .await?;
    orders::release_stock_and_coupon(&mut tx, &o).await?;
    orders::event(
        &mut tx,
        order_id,
        "cancelled_by_merchant",
        &json!({ "reason": reason }),
        actor,
    )
    .await?;
    crate::audit::record(
        &mut tx,
        actor,
        "order.cancelled",
        "order",
        Some(&order_id.to_string()),
        &json!({ "reason": reason, "refund": paid }),
    )
    .await?;
    platform::queue::publish(
        &mut *tx,
        orders::CANCELLED_EVENT,
        &json!({ "order_id": o.id, "number": o.number.to_string(),
                 "total_minor": o.total_minor, "currency": o.currency }),
    )
    .await?;
    orders::mail::send(
        &mut tx,
        urls,
        order_id,
        Template::OrderCancelled,
        json!({ "reason": "merchant", "refund": paid }),
        format!("order_cancelled:{order_id}"),
        &[],
    )
    .await?;
    let full = if paid {
        Some(full_input(&mut tx, order_id).await?)
    } else {
        None
    };
    tx.commit().await?;
    let refund = match full {
        Some(mut input) => {
            input.reason = Some(reason.unwrap_or("order cancelled").to_owned());
            input.iban = iban;
            Some(
                refund_order(
                    db,
                    payments_cfg,
                    urls,
                    tenant_id,
                    actor,
                    order_id,
                    &input,
                    None,
                )
                .await?,
            )
        }
        None => None,
    };
    Ok(CancelOutcome { refund })
}

// ---------------------------------------------------------------------------------------
// A10 exceptions: money the order cannot keep

/// Returns late or duplicate payments of an order (amount-only refunds, no credit note: none of
/// that money was invoiced) and resolves its exception. `409 no_open_exception` otherwise.
pub async fn refund_exception(
    db: &sqlx::PgPool,
    payments_cfg: &Payments,
    tenant_id: Uuid,
    actor: &str,
    order_id: Uuid,
) -> Result<Vec<Refund>, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let o = sqlx::query!(
        "SELECT exception, status, paid_attempt_id FROM orders WHERE id = $1
             AND exception IS NOT NULL AND exception_resolved_at IS NULL",
        order_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::Conflict {
        code: "no_open_exception",
        detail: "the order has no open exception".into(),
    })?;
    // Late payment (the order is cancelled): every successful attempt goes back. Duplicate:
    // all but the retained one.
    let keep = if o.status == "cancelled" {
        None
    } else {
        o.paid_attempt_id
    };
    let due = sqlx::query!(
        r#"SELECT a.id, a.amount_minor - coalesce((SELECT sum(r.amount_minor) FROM refunds r
                    WHERE r.attempt_id = a.id AND r.status <> 'failed'), 0)::bigint AS "left!"
           FROM payment_attempts a
           WHERE a.order_id = $1 AND a.status = 'succeeded'
             AND a.id IS DISTINCT FROM $2"#,
        order_id,
        keep
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut refunds = Vec::new();
    for a in due.into_iter().filter(|a| a.left > 0) {
        refunds.push(
            payments::refund(
                db,
                payments_cfg,
                tenant_id,
                a.id,
                a.left,
                Some(&format!(
                    "{} refund",
                    o.exception.as_deref().unwrap_or("exception")
                )),
                actor,
            )
            .await?,
        );
    }
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let total: i64 = refunds.iter().map(|r| r.amount_minor).sum();
    orders::resolve_exception(
        &mut tx,
        actor,
        order_id,
        &format!("refunded {} (minor units) to the customer", total),
    )
    .await?;
    tx.commit().await?;
    Ok(refunds)
}

/// The order's refunds, newest last.
pub async fn list(tx: &mut TenantTx, order_id: Uuid) -> Result<Vec<RefundView>, Error> {
    let locale = sqlx::query_scalar!("SELECT locale FROM orders WHERE id = $1", order_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    sqlx::query!(
        "SELECT id, amount_minor, currency, status, reason, iban, credit_note_id, withdrawal_id,
                created_by, created_at
         FROM refunds WHERE order_id = $1 ORDER BY created_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        let currency = Currency::parse(&r.currency)
            .ok_or_else(|| Error::Internal(format!("stored currency {}", r.currency)))?;
        Ok(RefundView {
            id: r.id,
            amount: Money::new(r.amount_minor, currency).view(Locale::from_tag(&locale)),
            status: r.status,
            reason: r.reason,
            iban: r.iban,
            credit_note_id: r.credit_note_id,
            withdrawal_id: r.withdrawal_id,
            created_by: r.created_by,
            created_at: r.created_at,
        })
    })
    .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RefundView {
    pub id: Uuid,
    pub amount: MoneyView,
    /// `pending`, `succeeded`, `failed`.
    pub status: String,
    pub reason: Option<String>,
    /// Bank refunds: the account to pay.
    pub iban: Option<String>,
    pub credit_note_id: Option<Uuid>,
    pub withdrawal_id: Option<Uuid>,
    pub created_by: String,
    pub created_at: chrono::DateTime<Utc>,
}
