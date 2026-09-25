//! Archived (imported historical) orders: read-only for staff (A28).

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::markets::invalid;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArchivedOrder {
    pub id: Uuid,
    /// The old shop's order number.
    pub number: String,
    pub placed_at: DateTime<Utc>,
    pub email: String,
    pub customer_id: Option<Uuid>,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub currency: String,
    pub total_minor: i64,
    /// The old shop's status, as imported.
    pub status_label: Option<String>,
    /// `{name, company, street, city, postal_code, country}`
    #[schema(value_type = Option<Object>)]
    pub address: Option<Value>,
    /// `[{sku, name, quantity, unit_price_minor}]`
    #[schema(value_type = Vec<Object>)]
    pub lines: Value,
    pub import_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ArchivedOrderFilter {
    /// Exact order number or part of the email address (case-insensitive).
    pub q: Option<String>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArchivedOrderPage {
    pub items: Vec<ArchivedOrder>,
    pub next_cursor: Option<Uuid>,
}

/// Newest (by placement) first.
pub async fn list(tx: &mut TenantTx, f: &ArchivedOrderFilter) -> Result<ArchivedOrderPage, Error> {
    let limit = f.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let q = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let pattern = q.map(|q| {
        let escaped = q
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    });
    let mut items = sqlx::query_as!(
        ArchivedOrder,
        "SELECT id, number, placed_at, email, customer_id, name, phone, currency, total_minor,
                status_label, address, lines, import_id, created_at, updated_at
         FROM archived_orders a
         WHERE ($1::text IS NULL OR a.number = $1 OR a.email LIKE $2)
           AND ($3::uuid IS NULL OR (a.placed_at, a.id) <
                (SELECT c.placed_at, c.id FROM archived_orders c WHERE c.id = $3))
         ORDER BY a.placed_at DESC, a.id DESC LIMIT $4",
        q,
        pattern,
        f.cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let limit = usize::try_from(limit).unwrap_or(50);
    let more = items.len() > limit;
    items.truncate(limit);
    Ok(ArchivedOrderPage {
        next_cursor: if more {
            items.last().map(|o| o.id)
        } else {
            None
        },
        items,
    })
}
