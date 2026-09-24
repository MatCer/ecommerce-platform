//! Audit log (spec §5.3): every mutating Admin API call records who changed what, in the same
//! transaction as the change. Append-only for the application role.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// Actor recorded for superadmin (CLI) operations.
pub const PLATFORM_ACTOR: &str = "platform";

pub async fn record(
    tx: &mut TenantTx,
    actor: &str,
    action: &str,
    entity: &str,
    entity_id: Option<&str>,
    diff: &Value,
) -> Result<(), sqlx::Error> {
    let tenant_id = tx.tenant_id();
    // Ids from Rust: `Uuid::now_v7` is monotonic within the process, so entries written in the
    // same millisecond keep their order (the SQL default is random within a millisecond).
    sqlx::query!(
        "INSERT INTO audit_log (id, tenant_id, actor, action, entity, entity_id, diff)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        crate::id::new_id(),
        tenant_id,
        actor,
        action,
        entity,
        entity_id,
        diff
    )
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditEntry {
    pub id: Uuid,
    /// Better Auth user id, or `platform` for superadmin operations.
    pub actor: String,
    pub action: String,
    pub entity: String,
    pub entity_id: Option<String>,
    pub diff: Value,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuditPage {
    pub items: Vec<AuditEntry>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

pub const MAX_PAGE: i64 = 100;

/// Newest first. Ids are UUIDv7, so id order is time order and the last id is the cursor.
pub async fn list(tx: &mut TenantTx, cursor: Option<Uuid>, limit: i64) -> Result<AuditPage, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(Error::Validation {
            code: "invalid_limit",
            detail: format!("limit must be between 1 and {MAX_PAGE}"),
        });
    }
    let mut items = sqlx::query_as!(
        AuditEntry,
        "SELECT id, actor, action, entity, entity_id, diff, at FROM audit_log
         WHERE $1::uuid IS NULL OR id < $1
         ORDER BY id DESC
         LIMIT $2",
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let more = items.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = if more {
        items.last().map(|e| e.id)
    } else {
        None
    };
    Ok(AuditPage { items, next_cursor })
}
