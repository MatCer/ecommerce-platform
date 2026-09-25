//! Per-tenant recommendation settings: which strategies run and which products are never
//! recommended. No stored row means the defaults (everything on, nothing excluded).

use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::Strategy;
use crate::audit;
use crate::markets::invalid;

pub const MAX_EXCLUDED: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecommendationSettings {
    pub bestsellers: bool,
    pub bought_together: bool,
    pub seasonal: bool,
    pub recently_viewed: bool,
    pub personalized: bool,
    /// Never recommended (gift cards, samples, ...). At most 500.
    #[serde(default)]
    pub excluded_product_ids: Vec<Uuid>,
}

impl Default for RecommendationSettings {
    fn default() -> Self {
        Self {
            bestsellers: true,
            bought_together: true,
            seasonal: true,
            recently_viewed: true,
            personalized: true,
            excluded_product_ids: Vec::new(),
        }
    }
}

impl RecommendationSettings {
    /// Whether `strategy` may run. Collections a theme asks for and the newest-products
    /// fallback cannot be switched off.
    pub fn enabled(&self, strategy: Strategy) -> bool {
        match strategy {
            Strategy::Bestsellers => self.bestsellers,
            Strategy::BoughtTogether => self.bought_together,
            Strategy::Seasonal => self.seasonal,
            Strategy::RecentlyViewed => self.recently_viewed,
            Strategy::Personalized => self.personalized,
            Strategy::Collection | Strategy::Newest => true,
        }
    }
}

pub async fn get(tx: &mut TenantTx) -> Result<RecommendationSettings, Error> {
    Ok(sqlx::query_as!(
        RecommendationSettings,
        "SELECT bestsellers, bought_together, seasonal, recently_viewed, personalized,
                excluded_product_ids
         FROM recommendation_settings"
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_default())
}

/// Replaces the settings. Excluded ids must be products of this tenant (deduplicated, order
/// kept).
pub async fn put(
    tx: &mut TenantTx,
    actor: &str,
    input: &RecommendationSettings,
) -> Result<RecommendationSettings, Error> {
    let mut excluded: Vec<Uuid> = Vec::with_capacity(input.excluded_product_ids.len());
    for id in &input.excluded_product_ids {
        if !excluded.contains(id) {
            excluded.push(*id);
        }
    }
    if excluded.len() > MAX_EXCLUDED {
        return Err(invalid(
            "too_many_exclusions",
            format!("at most {MAX_EXCLUDED} excluded products"),
        ));
    }
    let known = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM products WHERE id = ANY($1)"#,
        &excluded
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).ok() != Some(excluded.len()) {
        return Err(invalid(
            "unknown_product",
            "an excluded product does not exist",
        ));
    }
    let before = get(tx).await?;
    sqlx::query!(
        "INSERT INTO recommendation_settings (tenant_id, bestsellers, bought_together, seasonal,
                                              recently_viewed, personalized, excluded_product_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (tenant_id) DO UPDATE SET
             bestsellers = EXCLUDED.bestsellers, bought_together = EXCLUDED.bought_together,
             seasonal = EXCLUDED.seasonal, recently_viewed = EXCLUDED.recently_viewed,
             personalized = EXCLUDED.personalized,
             excluded_product_ids = EXCLUDED.excluded_product_ids, updated_at = now()",
        tx.tenant_id(),
        input.bestsellers,
        input.bought_together,
        input.seasonal,
        input.recently_viewed,
        input.personalized,
        &excluded
    )
    .execute(&mut **tx)
    .await?;
    let after = get(tx).await?;
    audit::record(
        tx,
        actor,
        "recommendations.settings_updated",
        "recommendation_settings",
        None,
        &json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}
