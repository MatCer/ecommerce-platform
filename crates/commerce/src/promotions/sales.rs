//! Sales: automatic discounts that change the effective price (spec §10.2). They are shown on
//! product pages and recorded in the price intervals, including future scheduled starts and
//! ends (A18): every write here recomputes the timelines.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;
use crate::money::Currency;
use crate::pricing::cart::MAX_AMOUNT;
use crate::pricing::intervals::{self, Cause, Scope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SaleDiscount {
    /// Percent off in basis points (1500 = 15 %), rounded half up to the minor unit.
    Percent { basis_points: u32 },
    /// A fixed amount off each unit; applies only to price lists in `currency`.
    Fixed {
        amount_minor: i64,
        currency: Currency,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaleTargets {
    /// Every product.
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub product_ids: Vec<Uuid>,
    /// Products in these categories or any of their subcategories.
    #[serde(default)]
    pub category_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Sale {
    pub id: Uuid,
    pub name: String,
    pub discount: SaleDiscount,
    pub starts_at: DateTime<Utc>,
    /// Open-ended when absent.
    pub ends_at: Option<DateTime<Utc>>,
    pub targets: SaleTargets,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaleInput {
    #[schema(example = "Podzimní výprodej")]
    pub name: String,
    pub discount: SaleDiscount,
    /// Default: now. A start in the past means now; a started sale keeps its start.
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub targets: SaleTargets,
}

const MAX_TARGETS: usize = 1000;

impl SaleInput {
    pub fn validate(&self) -> Result<(), Error> {
        if self.name.trim().is_empty() || self.name.chars().count() > 200 {
            return Err(invalid("invalid_name", "name must be 1-200 characters"));
        }
        match self.discount {
            SaleDiscount::Percent { basis_points } if !(1..=10_000).contains(&basis_points) => {
                return Err(invalid(
                    "invalid_discount",
                    "basis_points must be 1-10000 (0.01-100 %)",
                ));
            }
            SaleDiscount::Fixed { amount_minor, .. }
                if !(1..=MAX_AMOUNT).contains(&amount_minor) =>
            {
                return Err(invalid("invalid_discount", "amount_minor must be positive"));
            }
            _ => {}
        }
        let t = &self.targets;
        let listed = !t.product_ids.is_empty() || !t.category_ids.is_empty();
        if t.all == listed {
            return Err(invalid(
                "invalid_targets",
                "targets must be either all or a list of products/categories",
            ));
        }
        if t.product_ids.len() + t.category_ids.len() > MAX_TARGETS {
            return Err(invalid(
                "invalid_targets",
                format!("at most {MAX_TARGETS} target ids"),
            ));
        }
        if let (Some(s), Some(e)) = (self.starts_at, self.ends_at)
            && e <= s
        {
            return Err(invalid(
                "invalid_schedule",
                "ends_at must be after starts_at",
            ));
        }
        Ok(())
    }
}

/// Stored columns of a discount: (kind, value, currency).
fn columns(d: &SaleDiscount) -> (&'static str, i64, Option<&'static str>) {
    match d {
        SaleDiscount::Percent { basis_points } => ("percent", i64::from(*basis_points), None),
        SaleDiscount::Fixed {
            amount_minor,
            currency,
        } => ("fixed", *amount_minor, Some(currency.code())),
    }
}

struct Row {
    id: Uuid,
    name: String,
    kind: String,
    value: i64,
    currency: Option<String>,
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    targets: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Row {
    fn into_sale(self) -> Result<Sale, Error> {
        let bad = || Error::Internal(format!("malformed sale {}", self.id));
        let discount = match (self.kind.as_str(), self.currency.as_deref()) {
            ("percent", _) => SaleDiscount::Percent {
                basis_points: u32::try_from(self.value).map_err(|_| bad())?,
            },
            ("fixed", Some(c)) => SaleDiscount::Fixed {
                amount_minor: self.value,
                currency: Currency::parse(c).ok_or_else(bad)?,
            },
            _ => return Err(bad()),
        };
        Ok(Sale {
            id: self.id,
            name: self.name,
            discount,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            targets: serde_json::from_value(self.targets).map_err(|_| bad())?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Sales that have not ended at `now` (running or scheduled).
pub async fn live(tx: &mut TenantTx, now: DateTime<Utc>) -> Result<Vec<Sale>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, name, kind, value, currency, starts_at, ends_at, targets, created_at, updated_at
         FROM sales WHERE ends_at IS NULL OR ends_at > $1 ORDER BY id",
        now
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Row::into_sale)
    .collect()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SalePage {
    pub items: Vec<Sale>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

/// Newest first.
pub async fn list(tx: &mut TenantTx, cursor: Option<Uuid>, limit: i64) -> Result<SalePage, Error> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, name, kind, value, currency, starts_at, ends_at, targets, created_at, updated_at
         FROM sales WHERE $1::uuid IS NULL OR id < $1 ORDER BY id DESC LIMIT $2",
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(Row::into_sale)
        .collect::<Result<Vec<_>, _>>()?;
    let limit = usize::try_from(limit).unwrap_or(100);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(SalePage { items, next_cursor })
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Sale, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, name, kind, value, currency, starts_at, ends_at, targets, created_at, updated_at
         FROM sales WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .into_sale()
}

/// Resolves the effective schedule: a missing or past start is now; a started sale keeps its
/// start; the end must lie after both the start and now.
fn schedule(
    input: &SaleInput,
    existing: Option<&Sale>,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, Option<DateTime<Utc>>), Error> {
    let starts_at = match existing {
        Some(s) if s.starts_at <= now => {
            if input.starts_at.is_some_and(|t| t != s.starts_at) {
                return Err(invalid(
                    "sale_started",
                    "the start of a running sale cannot change",
                ));
            }
            s.starts_at
        }
        _ => input.starts_at.map_or(now, |t| t.max(now)),
    };
    if input.ends_at.is_some_and(|e| e <= starts_at || e <= now) {
        return Err(invalid(
            "invalid_schedule",
            "ends_at must be in the future and after starts_at",
        ));
    }
    Ok((starts_at, input.ends_at))
}

async fn rematerialize(tx: &mut TenantTx) -> Result<(), Error> {
    // ponytail: recomputes every priced variant and writes only real differences; narrow the
    // scope to the sale's targets if tenants reach hundreds of thousands of prices.
    intervals::refresh(tx, &Scope::All, Cause::Base, false).await?;
    Ok(())
}

pub async fn create(tx: &mut TenantTx, actor: &str, input: &SaleInput) -> Result<Sale, Error> {
    input.validate()?;
    // Lock before sampling the time (see intervals::refresh).
    intervals::lock(tx).await?;
    let now = Utc::now();
    let (starts_at, ends_at) = schedule(input, None, now)?;
    let (kind, value, currency) = columns(&input.discount);
    let id = crate::id::new_id();
    let targets =
        serde_json::to_value(&input.targets).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "INSERT INTO sales (id, tenant_id, name, kind, value, currency, starts_at, ends_at, targets)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        id,
        tx.tenant_id(),
        input.name.trim(),
        kind,
        value,
        currency,
        starts_at,
        ends_at,
        targets
    )
    .execute(&mut **tx)
    .await?;
    rematerialize(tx).await?;
    let sale = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "sale.created",
        "sale",
        Some(&id.to_string()),
        &json!({ "after": sale }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "sale.created", &json!({ "sale_id": id })).await?;
    Ok(sale)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &SaleInput,
) -> Result<Sale, Error> {
    input.validate()?;
    intervals::lock(tx).await?;
    let before = get(tx, id).await?;
    let now = Utc::now();
    let (starts_at, ends_at) = schedule(input, Some(&before), now)?;
    if before.starts_at <= now && input.discount != before.discount {
        return Err(invalid(
            "sale_started",
            "the discount of a running sale cannot change; end it and create a new one",
        ));
    }
    let (kind, value, currency) = columns(&input.discount);
    let targets =
        serde_json::to_value(&input.targets).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "UPDATE sales SET name = $2, kind = $3, value = $4, currency = $5, starts_at = $6,
                          ends_at = $7, targets = $8, updated_at = now()
         WHERE id = $1",
        id,
        input.name.trim(),
        kind,
        value,
        currency,
        starts_at,
        ends_at,
        targets
    )
    .execute(&mut **tx)
    .await?;
    rematerialize(tx).await?;
    let after = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "sale.updated",
        "sale",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "sale.updated", &json!({ "sale_id": id })).await?;
    Ok(after)
}

/// Deletes a sale. A running one ends now; its past price intervals stay as history.
pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    intervals::lock(tx).await?;
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM sales WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    rematerialize(tx).await?;
    audit::record(
        tx,
        actor,
        "sale.deleted",
        "sale",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "sale.deleted", &json!({ "sale_id": id })).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn input() -> SaleInput {
        SaleInput {
            name: "Výprodej".into(),
            discount: SaleDiscount::Percent { basis_points: 1500 },
            starts_at: None,
            ends_at: None,
            targets: SaleTargets {
                all: true,
                ..SaleTargets::default()
            },
        }
    }

    #[test]
    fn validation() {
        assert!(input().validate().is_ok());
        let code = |i: SaleInput| i.validate().unwrap_err().code();
        assert_eq!(
            code(SaleInput {
                discount: SaleDiscount::Percent { basis_points: 0 },
                ..input()
            }),
            "invalid_discount"
        );
        assert_eq!(
            code(SaleInput {
                discount: SaleDiscount::Fixed {
                    amount_minor: 0,
                    currency: Currency::Czk
                },
                ..input()
            }),
            "invalid_discount"
        );
        assert_eq!(
            code(SaleInput {
                targets: SaleTargets::default(),
                ..input()
            }),
            "invalid_targets"
        );
        assert_eq!(
            code(SaleInput {
                targets: SaleTargets {
                    all: true,
                    product_ids: vec![Uuid::nil()],
                    category_ids: vec![]
                },
                ..input()
            }),
            "invalid_targets"
        );
        assert_eq!(
            code(SaleInput {
                name: " ".into(),
                ..input()
            }),
            "invalid_name"
        );
        let now = Utc::now();
        assert_eq!(
            code(SaleInput {
                starts_at: Some(now),
                ends_at: Some(now),
                ..input()
            }),
            "invalid_schedule"
        );
        assert!(
            serde_json::from_value::<SaleInput>(json!({
                "name": "x", "discount": {"type": "percent", "basis_points": 10},
                "targets": {"all": true, "brands": []}
            }))
            .is_err()
        );
    }

    #[test]
    fn schedules() {
        let now = Utc::now();
        let h = Duration::hours(1);
        // Past or missing start -> now.
        assert_eq!(schedule(&input(), None, now).unwrap().0, now);
        let past = SaleInput {
            starts_at: Some(now - h),
            ..input()
        };
        assert_eq!(schedule(&past, None, now).unwrap().0, now);
        let future = SaleInput {
            starts_at: Some(now + h),
            ends_at: Some(now + h * 2),
            ..input()
        };
        assert_eq!(
            schedule(&future, None, now).unwrap(),
            (now + h, Some(now + h * 2))
        );
        // An end in the past is rejected.
        let ended = SaleInput {
            ends_at: Some(now - h),
            ..input()
        };
        assert_eq!(
            schedule(&ended, None, now).unwrap_err().code(),
            "invalid_schedule"
        );
        // A running sale keeps its start.
        let running = Sale {
            id: Uuid::nil(),
            name: "x".into(),
            discount: SaleDiscount::Percent { basis_points: 1 },
            starts_at: now - h * 5,
            ends_at: None,
            targets: SaleTargets::default(),
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            schedule(&input(), Some(&running), now).unwrap().0,
            now - h * 5
        );
        assert_eq!(
            schedule(&future, Some(&running), now).unwrap_err().code(),
            "sale_started"
        );
    }
}
