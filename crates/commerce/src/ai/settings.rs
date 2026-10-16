use platform::Error;
use platform::db::TenantTx;
use platform::queue;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::audit;

pub const CHANGED_EVENT: &str = "ai_settings.changed";

/// The shop's AI switches; every one is off by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct AiSettings {
    /// AI summaries of verified reviews on product pages, labelled as AI-generated.
    pub review_summaries: bool,
    /// AI alt texts for product images uploaded from now on, in every market language that
    /// has none.
    pub alt_texts: bool,
    /// AI hypotheses and theme-editor drafts for the top insights (the detection runs anyway).
    pub insights: bool,
}

/// A change to the AI switches; a switch left out keeps its value.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSettingsChange {
    pub review_summaries: Option<bool>,
    pub alt_texts: Option<bool>,
    pub insights: Option<bool>,
}

pub async fn get(tx: &mut TenantTx) -> Result<AiSettings, Error> {
    Ok(sqlx::query_as!(
        AiSettings,
        r#"SELECT review_summaries, alt_texts_since IS NOT NULL AS "alt_texts!", insights
           FROM ai_settings"#
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_default())
}

pub async fn put(
    tx: &mut TenantTx,
    actor: &str,
    change: &AiSettingsChange,
) -> Result<AiSettings, Error> {
    sqlx::query!(
        "INSERT INTO ai_settings (tenant_id) VALUES ($1) ON CONFLICT (tenant_id) DO NOTHING",
        tx.tenant_id()
    )
    .execute(&mut **tx)
    .await?;
    let before = sqlx::query_as!(
        AiSettings,
        r#"SELECT review_summaries, alt_texts_since IS NOT NULL AS "alt_texts!", insights
           FROM ai_settings WHERE tenant_id = $1 FOR UPDATE"#,
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    let after = AiSettings {
        review_summaries: change.review_summaries.unwrap_or(before.review_summaries),
        alt_texts: change.alt_texts.unwrap_or(before.alt_texts),
        insights: change.insights.unwrap_or(before.insights),
    };
    sqlx::query!(
        "UPDATE ai_settings SET review_summaries = $2,
             alt_texts_since = CASE WHEN $3 THEN coalesce(alt_texts_since, now()) END,
             insights = $4,
             updated_at = now()
         WHERE tenant_id = $1",
        tx.tenant_id(),
        after.review_summaries,
        after.alt_texts,
        after.insights
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "ai.settings_updated",
        "ai_settings",
        None,
        &json!({ "before": before, "after": after }),
    )
    .await?;
    if before.review_summaries != after.review_summaries {
        queue::publish(&mut **tx, CHANGED_EVENT, &json!({})).await?;
    }
    if after.review_summaries && !before.review_summaries {
        super::review_summary::enqueue_all(tx).await?;
    }
    Ok(after)
}
