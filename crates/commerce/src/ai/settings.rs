//! The shop's AI switches (owners and admins; audited).

use platform::Error;
use platform::db::TenantTx;
use platform::queue;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::audit;

pub const CHANGED_EVENT: &str = "ai_settings.changed";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    /// AI summaries of verified reviews on product pages, labelled as AI-generated. Off by
    /// default.
    pub review_summaries: bool,
    /// AI alt texts for product images uploaded from now on, in every market language that
    /// has none. Off by default.
    #[serde(default)]
    pub alt_texts: bool,
}

pub async fn get(tx: &mut TenantTx) -> Result<AiSettings, Error> {
    Ok(sqlx::query_as!(
        AiSettings,
        r#"SELECT review_summaries, alt_texts_since IS NOT NULL AS "alt_texts!" FROM ai_settings"#
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_default())
}

pub async fn put(tx: &mut TenantTx, actor: &str, input: &AiSettings) -> Result<AiSettings, Error> {
    sqlx::query!(
        "INSERT INTO ai_settings (tenant_id) VALUES ($1) ON CONFLICT (tenant_id) DO NOTHING",
        tx.tenant_id()
    )
    .execute(&mut **tx)
    .await?;
    let before = sqlx::query_as!(
        AiSettings,
        r#"SELECT review_summaries, alt_texts_since IS NOT NULL AS "alt_texts!" FROM ai_settings
           WHERE tenant_id = $1 FOR UPDATE"#,
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE ai_settings SET review_summaries = $2,
             alt_texts_since = CASE WHEN $3 THEN coalesce(alt_texts_since, now()) END,
             updated_at = now()
         WHERE tenant_id = $1",
        tx.tenant_id(),
        input.review_summaries,
        input.alt_texts
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "ai.settings_updated",
        "ai_settings",
        None,
        &json!({ "before": before, "after": input }),
    )
    .await?;
    if before.review_summaries != input.review_summaries {
        queue::publish(&mut **tx, CHANGED_EVENT, &json!({})).await?;
    }
    if input.review_summaries && !before.review_summaries {
        super::review_summary::enqueue_all(tx).await?;
    }
    Ok(*input)
}
