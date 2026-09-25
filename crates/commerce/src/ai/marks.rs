//! AI Act transparency (§12.1): fields written from an accepted AI proposal carry an
//! `ai_generated_at` marker, shown as a label in the admin. A marker holds the hash of the value
//! it was written with; once someone rewrites the field, the label no longer shows.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use super::fields::{Doc, EntityType};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AiMark {
    pub locale: String,
    pub field: String,
    pub feature: String,
    pub model: String,
    pub ai_generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AiMarkList {
    pub items: Vec<AiMark>,
}

pub fn value_hash(v: &Value) -> String {
    hex::encode(Sha256::digest(v.to_string().as_bytes()))
}

pub(crate) struct NewMark<'a> {
    pub locale: &'a str,
    pub field: &'a str,
    pub value: &'a Value,
    pub feature: &'a str,
    pub model: &'a str,
    pub proposal_id: Uuid,
    pub accepted_by: &'a str,
}

pub(crate) async fn record(
    tx: &mut TenantTx,
    entity_type: EntityType,
    entity_id: &str,
    m: &NewMark<'_>,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO ai_marks (tenant_id, entity_type, entity_id, locale, field, value_sha256,
                               feature, model, proposal_id, accepted_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (tenant_id, entity_type, entity_id, locale, field) DO UPDATE SET
             value_sha256 = EXCLUDED.value_sha256, feature = EXCLUDED.feature,
             model = EXCLUDED.model, proposal_id = EXCLUDED.proposal_id,
             accepted_by = EXCLUDED.accepted_by, ai_generated_at = now()",
        tx.tenant_id(),
        entity_type.as_str(),
        entity_id,
        m.locale,
        m.field,
        value_hash(m.value),
        m.feature,
        m.model,
        m.proposal_id,
        m.accepted_by
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The fields of an entity whose current value is still the AI-written one.
pub async fn list(
    tx: &mut TenantTx,
    entity_type: EntityType,
    entity_id: &str,
) -> Result<AiMarkList, Error> {
    let rows = sqlx::query!(
        "SELECT locale, field, value_sha256, feature, model, ai_generated_at FROM ai_marks
         WHERE entity_type = $1 AND entity_id = $2 ORDER BY locale, field",
        entity_type.as_str(),
        entity_id
    )
    .fetch_all(&mut **tx)
    .await?;
    if rows.is_empty() {
        return Ok(AiMarkList { items: vec![] });
    }
    let doc = Doc::load(tx, entity_type, entity_id).await?;
    let items = rows
        .into_iter()
        .filter(|r| {
            doc.get(&r.locale, &r.field)
                .is_some_and(|v| value_hash(&v) == r.value_sha256)
        })
        .map(|r| AiMark {
            locale: r.locale,
            field: r.field,
            feature: r.feature,
            model: r.model,
            ai_generated_at: r.ai_generated_at,
        })
        .collect();
    Ok(AiMarkList { items })
}
