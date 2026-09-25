//! Cash on delivery (spec §10.4, A16): the courier (or the merchant at a pickup counter)
//! collects the money, later the carrier remits it. States `pending → delivered → collected →
//! remitted` go through the WP9 COD machine; every manual action is audited.
//!
//! At collection the tender becomes known: cash is rounded (CZK to whole koruna, EUR in
//! Slovakia to €0.05) as a separate `rounding` charge on the order, outside the VAT base unless
//! the tax profile says otherwise (A15 step 6). The attempt then succeeds for the rounded
//! amount and the order is paid.

use std::collections::BTreeMap;

use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Attempt, Outcome};
use crate::audit;
use crate::markets::invalid;
use crate::money::Currency;
use crate::orders::{self, status::CodCommand, status::CodStatus, status::cod_transition};
use crate::pricing::cart::Tender;
use crate::pricing::cart::{VatRecapRow, cash_rounding, rounding_charge};
use crate::tax::{self, TaxRate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Collector {
    Carrier,
    Merchant,
}

impl Collector {
    fn as_str(self) -> &'static str {
        match self {
            Self::Carrier => "carrier",
            Self::Merchant => "merchant",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "carrier" => Ok(Self::Carrier),
            "merchant" => Ok(Self::Merchant),
            other => Err(Error::Internal(format!("stored collector {other}"))),
        }
    }
}

pub(crate) fn parse_tender(s: &str) -> Result<Tender, Error> {
    match s {
        "cash" => Ok(Tender::Cash),
        "card" => Ok(Tender::Card),
        "unknown" => Ok(Tender::Unknown),
        other => Err(Error::Internal(format!("stored tender {other}"))),
    }
}

fn tender_str(t: Tender) -> &'static str {
    match t {
        Tender::Cash => "cash",
        Tender::Card => "card",
        Tender::Unknown => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectInput {
    /// `cash` (rounded, A16) or `card`.
    pub tender: Tender,
    pub collector: Collector,
}

/// The order's cash-on-delivery attempt, locked with the order.
async fn cod_attempt(tx: &mut TenantTx, order_id: Uuid) -> Result<Attempt, Error> {
    let id = sqlx::query_scalar!(
        "SELECT id FROM payment_attempts WHERE order_id = $1 AND method = 'cod'
         ORDER BY created_at DESC LIMIT 1",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::Conflict {
        code: "not_cash_on_delivery",
        detail: "this order is not paid by cash on delivery".into(),
    })?;
    super::attempt(tx, id).await
}

async fn transition(
    tx: &mut TenantTx,
    order_id: Uuid,
    command: CodCommand,
) -> Result<(orders::OrderRow, Attempt, CodStatus), Error> {
    let order = orders::lock(tx, order_id).await?;
    if order.status == "cancelled" {
        return Err(Error::Conflict {
            code: "order_cancelled",
            detail: "the order is cancelled".into(),
        });
    }
    let a = cod_attempt(tx, order_id).await?;
    let from = a
        .cod_status
        .ok_or_else(|| Error::Internal("COD attempt without state".into()))?;
    let (next, _) = cod_transition(from, command)?;
    Ok((order, a, next))
}

/// The parcel reached the customer (carrier status or manual).
pub async fn deliver(tx: &mut TenantTx, actor: &str, order_id: Uuid) -> Result<Attempt, Error> {
    let (_, a, next) = transition(tx, order_id, CodCommand::Deliver).await?;
    sqlx::query!(
        "UPDATE payment_attempts SET cod_status = $2, delivered_at = now(), updated_at = now()
         WHERE id = $1",
        a.id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    record(
        tx,
        actor,
        order_id,
        "cod_delivered",
        &json!({ "attempt_id": a.id }),
    )
    .await?;
    super::attempt(tx, a.id).await
}

/// The money was collected: the tender and collector are recorded, cash is rounded (a
/// `rounding` charge on the order), the attempt succeeds for the collected amount and the
/// order is paid.
pub async fn collect(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    input: &CollectInput,
) -> Result<Attempt, Error> {
    if input.tender == Tender::Unknown {
        return Err(invalid(
            "invalid_collection",
            "the tender must be cash or card",
        ));
    }
    let (order, a, next) = transition(tx, order_id, CodCommand::Collect).await?;
    let amount = if input.tender == Tender::Cash {
        apply_cash_rounding(tx, &order).await?
    } else {
        order.total_minor
    };
    sqlx::query!(
        "UPDATE payment_attempts SET cod_status = $2, tender = $3, collector = $4,
             amount_minor = $5, collected_at = now(), updated_at = now()
         WHERE id = $1",
        a.id,
        next.as_str(),
        tender_str(input.tender),
        input.collector.as_str(),
        amount
    )
    .execute(&mut **tx)
    .await?;
    record(
        tx,
        actor,
        order_id,
        "cod_collected",
        &json!({ "attempt_id": a.id, "tender": input.tender, "collector": input.collector,
                 "amount_minor": amount }),
    )
    .await?;
    super::apply_outcome(tx, a.id, Outcome::Succeeded, actor).await
}

/// The carrier paid the collected money out to the merchant.
pub async fn remit(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    note: Option<&str>,
) -> Result<Attempt, Error> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 500) {
        return Err(invalid(
            "invalid_remittance",
            "the note is at most 500 characters",
        ));
    }
    let (_, a, next) = transition(tx, order_id, CodCommand::Remit).await?;
    sqlx::query!(
        "UPDATE payment_attempts SET cod_status = $2, remitted_at = now(), updated_at = now()
         WHERE id = $1",
        a.id,
        next.as_str()
    )
    .execute(&mut **tx)
    .await?;
    record(
        tx,
        actor,
        order_id,
        "cod_remitted",
        &json!({ "attempt_id": a.id, "amount_minor": a.amount_minor, "note": note }),
    )
    .await?;
    super::attempt(tx, a.id).await
}

async fn record(
    tx: &mut TenantTx,
    actor: &str,
    order_id: Uuid,
    kind: &str,
    data: &serde_json::Value,
) -> Result<(), Error> {
    orders::event(tx, order_id, kind, data, actor).await?;
    audit::record(
        tx,
        actor,
        &format!("order.{kind}"),
        "order",
        Some(&order_id.to_string()),
        data,
    )
    .await?;
    Ok(())
}

/// Adds the cash rounding charge (if any) to the order and returns the new total.
async fn apply_cash_rounding(tx: &mut TenantTx, order: &orders::OrderRow) -> Result<i64, Error> {
    let o = sqlx::query!(
        "SELECT ship_to_country, vat_payer, vat_recap,
                EXISTS (SELECT 1 FROM order_charges c WHERE c.order_id = o.id AND c.kind = 'rounding')
                    AS \"rounded!\"
         FROM orders o WHERE o.id = $1",
        order.id
    )
    .fetch_one(&mut **tx)
    .await?;
    let currency = Currency::parse(&order.currency)
        .ok_or_else(|| Error::Internal(format!("stored currency {}", order.currency)))?;
    let in_vat_base = tax::require(tx).await?.cash_rounding_in_vat_base;
    let rule = cash_rounding(currency, &o.ship_to_country, Tender::Cash, in_vat_base);
    let Some(rule) = rule.filter(|_| !o.rounded) else {
        return Ok(order.total_minor);
    };
    let mut goods: BTreeMap<TaxRate, i64> = BTreeMap::new();
    for l in sqlx::query!(
        "SELECT tax_rate, total_minor FROM order_lines WHERE order_id = $1",
        order.id
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let rate: TaxRate = l
            .tax_rate
            .parse()
            .map_err(|()| Error::Internal(format!("stored tax rate {}", l.tax_rate)))?;
        *goods.entry(rate).or_default() += l.total_minor;
    }
    let fallback = goods.keys().next_back().copied().unwrap_or(TaxRate::ZERO);
    let Some(charge) = rounding_charge(order.total_minor, rule, &goods, fallback)? else {
        return Ok(order.total_minor);
    };
    let portions: Vec<_> = if o.vat_payer {
        charge.portions.clone()
    } else {
        vec![]
    };
    let vat: i64 = portions.iter().map(|p| p.vat_minor).sum();
    sqlx::query!(
        "INSERT INTO order_charges (tenant_id, order_id, kind, base_minor, discount_minor,
             total_minor, tax_minor, net_minor, portions)
         VALUES ($1, $2, 'rounding', $3, 0, $3, $4, $5, $6)",
        tx.tenant_id(),
        order.id,
        charge.gross_minor,
        vat,
        charge.gross_minor - vat,
        serde_json::to_value(&portions).map_err(|e| Error::Internal(e.to_string()))?
    )
    .execute(&mut **tx)
    .await?;
    let mut recap: Vec<VatRecapRow> = serde_json::from_value(o.vat_recap)
        .map_err(|e| Error::Internal(format!("stored recap: {e}")))?;
    for p in &portions {
        match recap.iter_mut().find(|r| r.tax_rate == p.tax_rate) {
            Some(r) => {
                r.gross_minor += p.gross_minor;
                r.vat_minor += p.vat_minor;
                r.net_minor += p.net_minor;
            }
            None => recap.push(VatRecapRow {
                tax_rate: p.tax_rate,
                gross_minor: p.gross_minor,
                vat_minor: p.vat_minor,
                net_minor: p.net_minor,
            }),
        }
    }
    recap.sort_by_key(|r| r.tax_rate);
    let total = order.total_minor + charge.gross_minor;
    sqlx::query!(
        "UPDATE orders SET rounding_minor = rounding_minor + $2, total_minor = $3,
             tax_minor = tax_minor + $4, vat_recap = $5, updated_at = now()
         WHERE id = $1",
        order.id,
        charge.gross_minor,
        total,
        vat,
        serde_json::to_value(&recap).map_err(|e| Error::Internal(e.to_string()))?
    )
    .execute(&mut **tx)
    .await?;
    orders::event(
        tx,
        order.id,
        "cash_rounding",
        &json!({ "rounding_minor": charge.gross_minor, "total_minor": total,
                 "in_vat_base": rule.in_vat_base }),
        "system",
    )
    .await?;
    Ok(total)
}

// ---------------------------------------------------------------------------------------
// Carrier COD report (stub format until a carrier's real export is integrated, WP12)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CodReportRow {
    /// 1-based line number in the file.
    pub line: u32,
    pub order_number: String,
    /// `applied`, `skipped` (already in that state) or `error`.
    pub result: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct CodReport {
    pub applied: u32,
    pub skipped: u32,
    pub errors: u32,
    pub rows: Vec<CodReportRow>,
}

const MAX_REPORT_ROWS: usize = 5_000;

/// Imports a carrier COD report: CSV with the header
/// `order_number;amount;tender;event` (`,` works too), `event` = `collected` (the carrier
/// took the money: tender `cash`|`card`, amount as collected, e.g. `1290,00`) or `remitted`
/// (the carrier paid it out). Each row applies on its own (a savepoint); a collected amount
/// that differs from what the order expects is an error row, never applied.
pub async fn import_report(tx: &mut TenantTx, actor: &str, csv: &[u8]) -> Result<CodReport, Error> {
    let text = std::str::from_utf8(csv)
        .map_err(|_| invalid("invalid_cod_report", "the report must be UTF-8 CSV"))?
        .trim_start_matches('\u{feff}');
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (_, header) = lines
        .next()
        .ok_or_else(|| invalid("invalid_cod_report", "the report is empty"))?;
    let sep = if header.contains(';') { ';' } else { ',' };
    let cols: Vec<String> = header
        .split(sep)
        .map(|h| h.trim().trim_matches('"').to_ascii_lowercase())
        .collect();
    let col = |name: &str| cols.iter().position(|c| c == name);
    let (Some(num_col), Some(amount_col), Some(tender_col), Some(event_col)) = (
        col("order_number"),
        col("amount"),
        col("tender"),
        col("event"),
    ) else {
        return Err(invalid(
            "invalid_cod_report",
            "the header must be order_number;amount;tender;event",
        ));
    };
    let mut report = CodReport::default();
    for (n, line) in lines {
        if report.rows.len() >= MAX_REPORT_ROWS {
            return Err(invalid(
                "invalid_cod_report",
                format!("at most {MAX_REPORT_ROWS} rows"),
            ));
        }
        let f: Vec<&str> = line
            .split(sep)
            .map(|v| v.trim().trim_matches('"'))
            .collect();
        let get = |i: usize| f.get(i).copied().unwrap_or_default();
        let number = get(num_col).to_owned();
        sqlx::query("SAVEPOINT cod_row").execute(&mut **tx).await?;
        let result = report_row(
            tx,
            actor,
            &number,
            get(amount_col),
            get(tender_col),
            get(event_col),
        )
        .await;
        let (kind, detail) = match result {
            Ok(true) => ("applied", None),
            Ok(false) => ("skipped", None),
            Err(e) => ("error", Some(e.to_string())),
        };
        let release = if kind == "error" {
            "ROLLBACK TO SAVEPOINT cod_row"
        } else {
            "RELEASE SAVEPOINT cod_row"
        };
        sqlx::query(release).execute(&mut **tx).await?;
        match kind {
            "applied" => report.applied += 1,
            "skipped" => report.skipped += 1,
            _ => report.errors += 1,
        }
        report.rows.push(CodReportRow {
            line: u32::try_from(n + 1).unwrap_or(u32::MAX),
            order_number: number,
            result: kind.into(),
            detail,
        });
    }
    audit::record(
        tx,
        actor,
        "cod_report.imported",
        "order",
        None,
        &json!({ "applied": report.applied, "skipped": report.skipped, "errors": report.errors }),
    )
    .await?;
    Ok(report)
}

/// One report row: `Ok(true)` applied, `Ok(false)` already done.
async fn report_row(
    tx: &mut TenantTx,
    actor: &str,
    number: &str,
    amount: &str,
    tender: &str,
    event: &str,
) -> Result<bool, Error> {
    let number: i64 = number
        .parse()
        .map_err(|_| invalid("invalid_cod_report", "invalid order number"))?;
    let order_id = sqlx::query_scalar!("SELECT id FROM orders WHERE number = $1", number)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| invalid("invalid_cod_report", "unknown order number"))?;
    let state = cod_attempt(tx, order_id)
        .await?
        .cod_status
        .ok_or_else(|| Error::Internal("COD attempt without state".into()))?;
    match event {
        "collected" => {
            if matches!(state, CodStatus::Collected | CodStatus::Remitted) {
                return Ok(false);
            }
            let tender = match tender {
                "cash" => Tender::Cash,
                "card" => Tender::Card,
                _ => return Err(invalid("invalid_cod_report", "tender must be cash or card")),
            };
            let reported = super::statements::parse_amount(amount)?;
            let a = collect(
                tx,
                actor,
                order_id,
                &CollectInput {
                    tender,
                    collector: Collector::Carrier,
                },
            )
            .await?;
            if a.amount_minor != reported {
                return Err(invalid(
                    "cod_amount_mismatch",
                    format!(
                        "expected {} minor units, reported {reported}",
                        a.amount_minor
                    ),
                ));
            }
            Ok(true)
        }
        "remitted" => {
            if state == CodStatus::Remitted {
                return Ok(false);
            }
            remit(tx, actor, order_id, Some("carrier COD report")).await?;
            Ok(true)
        }
        _ => Err(invalid(
            "invalid_cod_report",
            "event must be collected or remitted",
        )),
    }
}
