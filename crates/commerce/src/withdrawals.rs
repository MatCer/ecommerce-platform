//! Withdrawal from a distance contract (EU, spec §10.6, A13, A19).
//!
//! Public flow on the checkout origin (`checkout.<host>/withdraw`), no account needed:
//! order number + email → an emailed single-use link (proves control of the mailbox; 24 h,
//! SHA-256 at rest; the answer never reveals whether the order exists) → the order's lines →
//! an explicit "Confirm withdrawal" → an immediate durable receipt: the full declaration is
//! stored (immutable) and emailed. Signed-in customers use the same form from their account.
//!
//! Tracked per withdrawal: `delivered_at`, `declared_at`, `goods_received_at` /
//! `return_proof_at`, `refund_due_at` = declared + 14 days. The merchant confirms the goods
//! (restock, A13) or a proof of dispatch, then refunds through the original payment method
//! (bank refunds to the IBAN from the form); the standard outbound shipping (and the payment
//! fee) is refunded once everything is withdrawn.

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::inventory::{self, MovementRef};
use crate::invoicing::document::RefundLine;
use crate::markets::invalid;
use crate::money::Locale;
use crate::notifications::{self, Brand, Email, Template};
use crate::orders::{
    self,
    status::{
        OrderCommand, ReturnCommand, ReturnLineStatus, ReturnSummary, return_summary,
        return_transition,
    },
};
use crate::payments::Payments;
use crate::refunds::{self, RefundInput, RefundOutcome};
use crate::storefront::{Context, PublicUrls};

/// The statutory withdrawal period and the refund deadline (days).
pub const PERIOD_DAYS: i64 = 14;
/// Validity of the emailed confirmation link.
pub const LINK_HOURS: i64 = 24;
/// At most this many links per order and hour (the form is public).
const LINKS_PER_HOUR: i64 = 3;

// ---------------------------------------------------------------------------------------
// The form

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WithdrawableLine {
    pub order_line_id: Uuid,
    pub name: String,
    pub options_label: String,
    pub sku: String,
    pub quantity: i32,
    /// Not yet withdrawn.
    pub withdrawable: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WithdrawalForm {
    pub order_number: String,
    pub placed_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    /// delivered + 14 days (the statutory period; later declarations are still recorded).
    pub deadline: Option<DateTime<Utc>>,
    /// The order has left the warehouse (before that it is cancelled, not withdrawn).
    pub eligible: bool,
    /// Bank transfer and cash on delivery are refunded to a bank account: the form asks for it.
    pub needs_iban: bool,
    pub lines: Vec<WithdrawableLine>,
    /// Earlier withdrawals of this order.
    pub withdrawals: Vec<WithdrawalReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WithdrawalReceipt {
    pub id: Uuid,
    pub declared_at: DateTime<Utc>,
    pub refund_due_at: DateTime<Utc>,
    /// The declaration as confirmed (also emailed).
    pub declaration: String,
    /// `open` or `refunded`.
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclareInput {
    pub lines: Vec<RefundLine>,
    /// Required for bank transfer and cash on delivery orders.
    #[serde(default)]
    pub iban: Option<String>,
    /// Optional message to the shop (a reason is not required by law).
    #[serde(default)]
    pub note: Option<String>,
    /// The explicit confirmation step (A19): must be `true`.
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkRequest {
    pub order_number: String,
    pub email: String,
}

async fn delivered_at(tx: &mut TenantTx, order_id: Uuid) -> Result<Option<DateTime<Utc>>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT max(delivered_at) FROM shipments WHERE order_id = $1",
        order_id
    )
    .fetch_one(&mut **tx)
    .await?)
}

async fn receipts(tx: &mut TenantTx, order_id: Uuid) -> Result<Vec<WithdrawalReceipt>, Error> {
    Ok(sqlx::query_as!(
        WithdrawalReceipt,
        "SELECT id, declared_at, refund_due_at, declaration, status FROM withdrawals
         WHERE order_id = $1 ORDER BY declared_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// The form for an order.
pub async fn form(tx: &mut TenantTx, order_id: Uuid) -> Result<WithdrawalForm, Error> {
    let o = sqlx::query!(
        "SELECT number, placed_at, status, payment_method FROM orders WHERE id = $1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let lines = sqlx::query!(
        r#"SELECT l.id, l.name, l.options_label, l.sku, l.quantity,
                  coalesce((SELECT sum(r.quantity) FROM return_lines r
                            WHERE r.order_line_id = l.id AND r.status <> 'rejected'), 0)::int
                      AS "withdrawn!"
           FROM order_lines l WHERE l.order_id = $1 ORDER BY l.position"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|l| WithdrawableLine {
        order_line_id: l.id,
        name: l.name,
        options_label: l.options_label,
        sku: l.sku,
        quantity: l.quantity,
        withdrawable: (l.quantity - l.withdrawn).max(0),
    })
    .collect();
    let delivered = delivered_at(tx, order_id).await?;
    Ok(WithdrawalForm {
        order_number: o.number.to_string(),
        placed_at: o.placed_at,
        delivered_at: delivered,
        deadline: delivered.map(|d| d + Duration::days(PERIOD_DAYS)),
        eligible: matches!(o.status.as_str(), "shipped" | "delivered"),
        needs_iban: matches!(o.payment_method.as_str(), "bank_transfer" | "cod"),
        lines,
        withdrawals: receipts(tx, order_id).await?,
    })
}

/// Public step 1: emails a confirmation link if `order_number` + `email` match an order that
/// can be withdrawn from. Always succeeds (no enumeration); links per order are rate-limited.
pub async fn request_link(
    tx: &mut TenantTx,
    ctx: &Context,
    input: &LinkRequest,
) -> Result<(), Error> {
    let Ok(email) = crate::staff::normalize_email(&input.email) else {
        return Err(invalid(
            "invalid_email",
            "enter the email used for the order",
        ));
    };
    let Ok(number) = input.order_number.trim().parse::<i64>() else {
        return Ok(());
    };
    let Some(o) = sqlx::query!(
        "SELECT id, locale, status FROM orders WHERE number = $1 AND email = $2",
        number,
        email
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(());
    };
    if !matches!(o.status.as_str(), "shipped" | "delivered") {
        return Ok(());
    }
    let recent = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM withdrawal_tokens
           WHERE order_id = $1 AND created_at > now() - interval '1 hour'"#,
        o.id
    )
    .fetch_one(&mut **tx)
    .await?;
    if recent >= LINKS_PER_HOUR {
        return Ok(());
    }
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO withdrawal_tokens (token_hash, tenant_id, order_id, expires_at)
         VALUES ($1, $2, $3, $4)",
        minted.hash,
        tx.tenant_id(),
        o.id,
        Utc::now() + Duration::hours(LINK_HOURS)
    )
    .execute(&mut **tx)
    .await?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::WithdrawalLink,
            stream: Stream::Transactional,
            to: &email,
            locale: &o.locale,
            vars: json!({
                "number": number.to_string(),
                "url": ctx.checkout_url(&format!("/withdraw?t={}", minted.token)),
                "hours": LINK_HOURS.to_string(),
            }),
            idempotency_key: format!("withdrawal_link:{}", hex::encode(&minted.hash)),
            sensitive: true,
        },
    )
    .await?;
    Ok(())
}

/// The order behind a valid, unused link.
pub async fn order_by_token(tx: &mut TenantTx, token: &str) -> Result<Uuid, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    sqlx::query_scalar!(
        "SELECT order_id FROM withdrawal_tokens
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

/// Consumes a link atomically (single use); `404` if it was used meanwhile.
pub async fn consume_token(tx: &mut TenantTx, token: &str) -> Result<Uuid, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    sqlx::query_scalar!(
        "UPDATE withdrawal_tokens SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING order_id",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

fn fmt_time(t: DateTime<Utc>, locale: Locale) -> String {
    let local = crate::invoicing::prague::local(t);
    match locale {
        Locale::En => local.format("%-d %b %Y %H:%M").to_string(),
        _ => local.format("%-d. %-m. %Y %H:%M").to_string(),
    }
}

fn fmt_date(t: DateTime<Utc>, locale: Locale) -> String {
    let local = crate::invoicing::prague::date(t);
    match locale {
        Locale::En => local.format("%-d %b %Y").to_string(),
        _ => local.format("%-d. %-m. %Y").to_string(),
    }
}

struct Labels {
    title: &'static str,
    to: &'static str,
    statement: &'static str,
    order: &'static str,
    ordered: &'static str,
    received: &'static str,
    not_received: &'static str,
    goods: &'static str,
    consumer: &'static str,
    iban: &'static str,
    note: &'static str,
    date: &'static str,
    channel: &'static str,
}

fn labels(l: Locale) -> Labels {
    match l {
        Locale::Cs => Labels {
            title: "Odstoupení od kupní smlouvy",
            to: "Adresát",
            statement: "Oznamuji, že tímto odstupuji od smlouvy o nákupu tohoto zboží:",
            order: "Číslo objednávky",
            ordered: "Datum objednání",
            received: "Datum převzetí",
            not_received: "zatím nepřevzato",
            goods: "Zboží",
            consumer: "Spotřebitel",
            iban: "Účet pro vrácení peněz",
            note: "Zpráva",
            date: "Datum a čas prohlášení",
            channel: "Podáno elektronicky formulářem obchodu",
        },
        Locale::Sk => Labels {
            title: "Odstúpenie od kúpnej zmluvy",
            to: "Adresát",
            statement: "Oznamujem, že týmto odstupujem od zmluvy na nákup tohto tovaru:",
            order: "Číslo objednávky",
            ordered: "Dátum objednania",
            received: "Dátum prevzatia",
            not_received: "zatiaľ neprevzaté",
            goods: "Tovar",
            consumer: "Spotrebiteľ",
            iban: "Účet na vrátenie peňazí",
            note: "Správa",
            date: "Dátum a čas vyhlásenia",
            channel: "Podané elektronicky formulárom obchodu",
        },
        Locale::En => Labels {
            title: "Withdrawal from the contract",
            to: "To",
            statement: "I hereby give notice that I withdraw from my contract of sale of the following goods:",
            order: "Order number",
            ordered: "Ordered on",
            received: "Received on",
            not_received: "not received yet",
            goods: "Goods",
            consumer: "Consumer",
            iban: "Account for the refund",
            note: "Message",
            date: "Declared on",
            channel: "Submitted electronically through the shop's form",
        },
    }
}

/// Records a withdrawal (step 3, after the explicit confirmation) and emails the receipt.
pub async fn declare(
    tx: &mut TenantTx,
    ctx: &Context,
    order_id: Uuid,
    input: &DeclareInput,
    channel: &str,
) -> Result<WithdrawalReceipt, Error> {
    if !input.confirm {
        return Err(invalid(
            "confirmation_required",
            "confirm the withdrawal explicitly",
        ));
    }
    orders::lock(tx, order_id).await?;
    let f = form(tx, order_id).await?;
    if !f.eligible {
        return Err(Error::Conflict {
            code: "not_withdrawable",
            detail: "the order has not been dispatched (contact the shop to cancel it)".into(),
        });
    }
    if input.lines.is_empty() {
        return Err(invalid("invalid_withdrawal", "choose at least one item"));
    }
    let mut chosen = Vec::new();
    for r in &input.lines {
        let l = f
            .lines
            .iter()
            .find(|l| l.order_line_id == r.order_line_id)
            .ok_or_else(|| invalid("invalid_withdrawal", "unknown item"))?;
        if r.quantity <= 0
            || r.quantity > l.withdrawable
            || chosen
                .iter()
                .any(|(c, _): &(&WithdrawableLine, i32)| c.order_line_id == l.order_line_id)
        {
            return Err(invalid(
                "invalid_withdrawal",
                format!("{}: at most {} item(s)", l.name, l.withdrawable),
            ));
        }
        chosen.push((l, r.quantity));
    }
    let iban = match input
        .iban
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(
            crate::payments::bank::normalize_iban(raw)
                .ok_or_else(|| invalid("invalid_iban", "the IBAN is not valid"))?,
        ),
        None if f.needs_iban => {
            return Err(invalid(
                "iban_required",
                "enter the bank account (IBAN) for the refund",
            ));
        }
        None => None,
    };
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(1000).collect::<String>());
    let o = sqlx::query!("SELECT email, locale FROM orders WHERE id = $1", order_id)
        .fetch_one(&mut **tx)
        .await?;
    let customer = sqlx::query!(
        "SELECT name, street, city, postal_code, country FROM order_addresses
         WHERE order_id = $1 ORDER BY CASE kind WHEN 'billing' THEN 0 ELSE 1 END LIMIT 1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let supplier = crate::content::legal::entity(tx).await?.entity;
    let locale = Locale::from_tag(&o.locale);
    let lb = labels(locale);
    let now = Utc::now();
    let address = if supplier.returns_address.is_empty() {
        format!(
            "{}, {} {}",
            supplier.street, supplier.postal_code, supplier.city
        )
    } else {
        supplier.returns_address.clone()
    };
    let mut text = format!(
        "{}\n\n{}: {}, {address}",
        lb.title, lb.to, supplier.company_name
    );
    if !supplier.email.is_empty() {
        text.push_str(&format!(", {}", supplier.email));
    }
    text.push_str(&format!(
        "\n\n{}\n\n{}: {}\n{}: {}\n{}: {}\n{}:\n",
        lb.statement,
        lb.order,
        f.order_number,
        lb.ordered,
        fmt_date(f.placed_at, locale),
        lb.received,
        f.delivered_at
            .map_or_else(|| lb.not_received.to_owned(), |d| fmt_date(d, locale)),
        lb.goods
    ));
    for (l, q) in &chosen {
        let opts = if l.options_label.is_empty() {
            String::new()
        } else {
            format!(" ({})", l.options_label)
        };
        text.push_str(&format!("- {q}× {}{opts}, {}\n", l.name, l.sku));
    }
    text.push_str(&format!("\n{}: ", lb.consumer));
    match &customer {
        Some(c) => text.push_str(&format!(
            "{}, {}, {} {}, {}, {}",
            c.name, c.street, c.postal_code, c.city, c.country, o.email
        )),
        None => text.push_str(&o.email),
    }
    if let Some(iban) = &iban {
        text.push_str(&format!("\n{}: {iban}", lb.iban));
    }
    if let Some(n) = &note {
        text.push_str(&format!("\n{}: {n}", lb.note));
    }
    text.push_str(&format!(
        "\n{}: {}\n{}",
        lb.date,
        fmt_time(now, locale),
        lb.channel
    ));
    let id = crate::id::new_id();
    let due = now + Duration::days(PERIOD_DAYS);
    sqlx::query!(
        "INSERT INTO withdrawals (id, tenant_id, order_id, email, locale, channel, declaration,
             iban, note, delivered_at, declared_at, refund_due_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        id,
        tx.tenant_id(),
        order_id,
        o.email,
        o.locale,
        channel,
        text,
        iban,
        note,
        f.delivered_at,
        now,
        due
    )
    .execute(&mut **tx)
    .await?;
    for (l, q) in &chosen {
        // A withdrawal is the consumer's right: requested and approved at once.
        let (approved, _) = return_transition(ReturnLineStatus::Requested, ReturnCommand::Approve)?;
        sqlx::query!(
            "INSERT INTO return_lines (tenant_id, withdrawal_id, order_line_id, quantity, status)
             VALUES ($1, $2, $3, $4, $5)",
            tx.tenant_id(),
            id,
            l.order_line_id,
            q,
            approved.as_str()
        )
        .execute(&mut **tx)
        .await?;
    }
    let late = f.deadline.is_some_and(|d| now > d);
    orders::event(
        tx,
        order_id,
        "withdrawal_declared",
        &json!({ "withdrawal_id": id, "channel": channel, "late": late,
                 "lines": chosen.iter().map(|(l, q)| json!({
                     "order_line_id": l.order_line_id, "quantity": q })).collect::<Vec<_>>() }),
        "customer",
    )
    .await?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::WithdrawalReceipt,
            stream: Stream::Transactional,
            to: &o.email,
            locale: &o.locale,
            vars: json!({
                "number": f.order_number,
                "declared_at": fmt_time(now, locale),
                "declaration": text,
                "refund_due": fmt_date(due, locale),
            }),
            idempotency_key: format!("withdrawal_receipt:{id}"),
            sensitive: false,
        },
    )
    .await?;
    Ok(WithdrawalReceipt {
        id,
        declared_at: now,
        refund_due_at: due,
        declaration: text,
        status: "open".into(),
    })
}

// ---------------------------------------------------------------------------------------
// Admin queue

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReturnLineView {
    pub id: Uuid,
    pub order_line_id: Uuid,
    pub name: String,
    pub sku: String,
    pub quantity: i32,
    pub status: ReturnLineStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Withdrawal {
    pub id: Uuid,
    pub order_id: Uuid,
    pub order_number: String,
    pub email: String,
    pub channel: String,
    /// `open` or `refunded`.
    pub status: String,
    pub declaration: String,
    pub iban: Option<String>,
    pub note: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub declared_at: DateTime<Utc>,
    pub goods_received_at: Option<DateTime<Utc>>,
    pub return_proof_at: Option<DateTime<Utc>>,
    pub refund_due_at: DateTime<Utc>,
    pub refunded_at: Option<DateTime<Utc>>,
    /// Declared after the 14-day period (the merchant decides).
    pub late: bool,
    /// The refund deadline has passed without a refund.
    pub overdue: bool,
    pub lines: Vec<ReturnLineView>,
}

async fn lines_of(tx: &mut TenantTx, id: Uuid) -> Result<Vec<ReturnLineView>, Error> {
    sqlx::query!(
        "SELECT r.id, r.order_line_id, l.name, l.sku, r.quantity, r.status
         FROM return_lines r JOIN order_lines l ON l.id = r.order_line_id
         WHERE r.withdrawal_id = $1 ORDER BY l.position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(ReturnLineView {
            id: r.id,
            order_line_id: r.order_line_id,
            name: r.name,
            sku: r.sku,
            quantity: r.quantity,
            status: r
                .status
                .parse()
                .map_err(|()| Error::Internal(format!("return line status {}", r.status)))?,
        })
    })
    .collect()
}

/// Withdrawals, soonest refund deadline first; `open_only` = still to refund.
pub async fn list(tx: &mut TenantTx, open_only: bool) -> Result<Vec<Withdrawal>, Error> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM withdrawals WHERE (NOT $1 OR status = 'open')
         ORDER BY status = 'open' DESC, refund_due_at, id LIMIT 200",
        open_only
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(get(tx, id).await?);
    }
    Ok(out)
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Withdrawal, Error> {
    let w = sqlx::query!(
        "SELECT w.id, w.order_id, o.number, w.email, w.channel, w.status, w.declaration, w.iban,
                w.note, w.delivered_at, w.declared_at, w.goods_received_at, w.return_proof_at,
                w.refund_due_at, w.refunded_at
         FROM withdrawals w JOIN orders o ON o.id = w.order_id WHERE w.id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let lines = lines_of(tx, id).await?;
    Ok(Withdrawal {
        id: w.id,
        order_id: w.order_id,
        order_number: w.number.to_string(),
        email: w.email,
        channel: w.channel,
        overdue: w.status == "open" && Utc::now() > w.refund_due_at,
        status: w.status,
        declaration: w.declaration,
        iban: w.iban,
        note: w.note,
        late: w
            .delivered_at
            .is_some_and(|d| w.declared_at > d + Duration::days(PERIOD_DAYS)),
        delivered_at: w.delivered_at,
        declared_at: w.declared_at,
        goods_received_at: w.goods_received_at,
        return_proof_at: w.return_proof_at,
        refund_due_at: w.refund_due_at,
        refunded_at: w.refunded_at,
        lines,
    })
}

/// Withdrawals of one order.
pub async fn for_order(tx: &mut TenantTx, order_id: Uuid) -> Result<Vec<Withdrawal>, Error> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM withdrawals WHERE order_id = $1 ORDER BY declared_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(get(tx, id).await?);
    }
    Ok(out)
}

async fn set_line(
    tx: &mut TenantTx,
    line: &ReturnLineView,
    command: ReturnCommand,
) -> Result<ReturnLineStatus, Error> {
    let (next, _) = return_transition(line.status, command)?;
    sqlx::query!(
        "UPDATE return_lines SET status = $2, updated_at = now() WHERE id = $1",
        line.id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    Ok(next)
}

/// The withdrawal's order, locked; the withdrawal is read after the lock (fresh states).
async fn locked(tx: &mut TenantTx, id: Uuid) -> Result<(orders::OrderRow, Withdrawal), Error> {
    let order_id = sqlx::query_scalar!("SELECT order_id FROM withdrawals WHERE id = $1", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let o = orders::lock(tx, order_id).await?;
    Ok((o, get(tx, id).await?))
}

/// A13: the order status derives from the return line states; once every unit is back (received
/// or refunded) the order is `returned`.
async fn settle_order(
    tx: &mut TenantTx,
    o: &mut orders::OrderRow,
    actor: &str,
) -> Result<(), Error> {
    let per_line = sqlx::query!(
        "SELECT l.quantity, coalesce(array_agg(r.quantity) FILTER (WHERE r.id IS NOT NULL), '{}')
                    AS \"returned!\",
                coalesce(array_agg(r.status) FILTER (WHERE r.id IS NOT NULL), '{}') AS \"states!\"
         FROM order_lines l LEFT JOIN return_lines r ON r.order_line_id = l.id
         WHERE l.order_id = $1 GROUP BY l.id, l.quantity",
        o.id
    )
    .fetch_all(&mut **tx)
    .await?;
    let summary_input: Vec<(u32, Vec<(u32, ReturnLineStatus)>)> = per_line
        .into_iter()
        .map(|r| {
            (
                u32::try_from(r.quantity).unwrap_or(0),
                r.returned
                    .into_iter()
                    .zip(r.states)
                    .filter_map(|(q, s)| Some((u32::try_from(q).ok()?, s.parse().ok()?)))
                    .collect(),
            )
        })
        .collect();
    if return_summary(&summary_input) == ReturnSummary::Full
        && matches!(o.status.as_str(), "shipped" | "delivered")
    {
        orders::apply_order(
            tx,
            o,
            OrderCommand::MarkReturned(ReturnSummary::Full),
            actor,
        )
        .await?;
    }
    Ok(())
}

/// The merchant confirms the goods arrived: every line is restocked (A13, movement identity
/// `restock/return_line/<id>`, exactly once).
pub async fn receive(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<Withdrawal, Error> {
    let (mut o, w) = locked(tx, id).await?;
    if w.goods_received_at.is_some() {
        return Err(Error::Conflict {
            code: "already_received",
            detail: "the goods were already received".into(),
        });
    }
    // A parcel that came back undelivered was restocked as a whole already.
    let fulfillment =
        sqlx::query_scalar!("SELECT fulfillment_status FROM orders WHERE id = $1", o.id)
            .fetch_one(&mut **tx)
            .await?;
    if fulfillment == "returned" {
        return Err(Error::Conflict {
            code: "parcel_returned",
            detail: "the whole parcel came back and was restocked".into(),
        });
    }
    for l in &w.lines {
        set_line(tx, l, ReturnCommand::Receive).await?;
        let variant = sqlx::query_scalar!(
            "SELECT variant_id FROM order_lines WHERE id = $1",
            l.order_line_id
        )
        .fetch_one(&mut **tx)
        .await?;
        if let Some(variant) = variant {
            let ref_id = l.id.to_string();
            inventory::restock(
                tx,
                actor,
                &MovementRef {
                    ref_type: "return_line",
                    ref_id: &ref_id,
                },
                variant,
                l.quantity,
            )
            .await?;
        }
    }
    sqlx::query!(
        "UPDATE withdrawals SET goods_received_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    settle_order(tx, &mut o, actor).await?;
    orders::event(
        tx,
        w.order_id,
        "withdrawal_goods_received",
        &json!({ "withdrawal_id": id }),
        actor,
    )
    .await?;
    crate::audit::record(
        tx,
        actor,
        "withdrawal.goods_received",
        "order",
        Some(&w.order_id.to_string()),
        &json!({ "withdrawal_id": id }),
    )
    .await?;
    get(tx, id).await
}

/// The customer proved dispatch of the goods (A19: the refund may then go out before they
/// arrive).
pub async fn record_proof(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<Withdrawal, Error> {
    let (_, w) = locked(tx, id).await?;
    sqlx::query!(
        "UPDATE withdrawals SET return_proof_at = coalesce(return_proof_at, now()) WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    orders::event(
        tx,
        w.order_id,
        "withdrawal_proof_received",
        &json!({ "withdrawal_id": id }),
        actor,
    )
    .await?;
    get(tx, id).await
}

/// Refunds a withdrawal once the goods (or a proof of dispatch) arrived: its lines, plus the
/// shipping and payment fee when nothing else of the order remains. At most one refund per
/// withdrawal (a unique index; a concurrent second request gets `409 already_refunded`).
pub async fn refund(
    db: &sqlx::PgPool,
    payments: &Payments,
    urls: &PublicUrls,
    tenant_id: Uuid,
    actor: &str,
    id: Uuid,
) -> Result<RefundOutcome, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let (mut o, w) = locked(&mut tx, id).await?;
    if w.status != "open" {
        return Err(Error::Conflict {
            code: "already_refunded",
            detail: "the withdrawal was already refunded".into(),
        });
    }
    if w.goods_received_at.is_none() && w.return_proof_at.is_none() {
        return Err(Error::Conflict {
            code: "goods_not_back",
            detail: "confirm the returned goods or a proof of dispatch first".into(),
        });
    }
    let mut input = RefundInput {
        lines: w
            .lines
            .iter()
            .map(|l| RefundLine {
                order_line_id: l.order_line_id,
                quantity: l.quantity,
            })
            .collect(),
        reason: Some("withdrawal".into()),
        iban: w.iban.clone(),
        ..RefundInput::default()
    };
    // Everything withdrawn: the outbound shipping and the payment fee go back too.
    let remaining = refunds::full_input(&mut tx, w.order_id).await?;
    let all_lines = remaining.lines.iter().all(|r| {
        input
            .lines
            .iter()
            .any(|l| l.order_line_id == r.order_line_id && l.quantity >= r.quantity)
    });
    if all_lines {
        input.shipping = remaining.shipping;
        input.payment_fee = remaining.payment_fee;
    }
    let prepared = refunds::prepare(&mut tx, &mut o, actor, &input, Some(id)).await?;
    tx.commit().await?;
    refunds::complete(db, payments, urls, tenant_id, actor, prepared).await
}

/// Marks a withdrawal refunded (called by the refund's finalization, under the order lock):
/// each line moves on from its current state, and the order becomes `returned` when every
/// unit is back.
pub(crate) async fn complete_refund(tx: &mut TenantTx, id: Uuid, actor: &str) -> Result<(), Error> {
    let (mut o, w) = locked(tx, id).await?;
    if w.status == "refunded" {
        return Ok(());
    }
    for l in &w.lines {
        if matches!(
            l.status,
            ReturnLineStatus::Approved | ReturnLineStatus::Received
        ) {
            set_line(tx, l, ReturnCommand::Refund).await?;
        }
    }
    sqlx::query!(
        "UPDATE withdrawals SET status = 'refunded', refunded_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    settle_order(tx, &mut o, actor).await
}
