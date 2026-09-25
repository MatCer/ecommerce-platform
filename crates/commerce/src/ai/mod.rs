//! AI helpers (spec §12): metering and quotas over `platform::ai`, the tenant glossary, AI Act
//! labels, proposals (descriptions, SEO, translations) and bulk edit by prompt.
//!
//! The model never touches the database: its JSON output is validated here and every write
//! goes through the existing catalog/content/pricing services (audit log, outbox events).

pub mod fields;
pub mod glossary;
pub mod marks;
pub mod plan;
mod prompts;
pub mod proposals;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use platform::Error;
use platform::ai::{AiError, Client, Failure, PriceTable, Request};
use platform::config::{AiConfig, AiProvider};
use platform::db::{TenantTx, tenant_tx};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

/// Features (metering key, fake fixture name).
pub const PRODUCT_DESCRIPTION: &str = "product_description";
pub const CATEGORY_DESCRIPTION: &str = "category_description";
pub const SEO: &str = "seo";
pub const TRANSLATE: &str = "translate";
pub const BULK_PLAN: &str = "bulk_plan";

/// Fake provider fixtures (minijinja templates rendering JSON from the request data).
const FIXTURES: &[(&str, &str)] = &[
    (
        PRODUCT_DESCRIPTION,
        include_str!("fixtures/product_description.j2"),
    ),
    (
        CATEGORY_DESCRIPTION,
        include_str!("fixtures/category_description.j2"),
    ),
    (SEO, include_str!("fixtures/seo.j2")),
    (TRANSLATE, include_str!("fixtures/translate.j2")),
    (BULK_PLAN, include_str!("fixtures/bulk_plan.j2")),
];

/// The AI services of a process (API or worker).
#[derive(Clone)]
pub struct Ai {
    /// `None`: disabled (production without a key).
    client: Option<Client>,
    pub helper_model: String,
    prices: Arc<PriceTable>,
    plan_quotas: Arc<BTreeMap<String, i64>>,
}

impl Ai {
    pub fn from_config(cfg: &AiConfig) -> Result<Self, AiError> {
        let client = match &cfg.provider {
            AiProvider::Anthropic { api_key } => Some(Client::Anthropic(Arc::new(
                platform::ai::Anthropic::new(
                    &cfg.base_url,
                    api_key.clone(),
                    cfg.timeout,
                    cfg.max_retries,
                )
                .map_err(|e| AiError::Rejected(e.to_string()))?,
            ))),
            AiProvider::Fake => Some(Client::Fake(Arc::new(platform::ai::Fake::new(
                FIXTURES.iter().copied(),
            )?))),
            AiProvider::Disabled => None,
        };
        Ok(Self {
            client,
            helper_model: cfg.helper_model.clone(),
            prices: Arc::new(cfg.prices.clone()),
            plan_quotas: Arc::new(cfg.plan_quotas.clone()),
        })
    }

    /// The fake provider with default models and quotas (tests, tools).
    pub fn fake() -> Self {
        Self::fake_with_client(
            platform::ai::Fake::new(FIXTURES.iter().copied())
                .map(|f| Client::Fake(Arc::new(f)))
                .ok(),
        )
    }

    /// Default models and quotas around any client (tests with a stub API).
    pub fn fake_with_client(client: Option<Client>) -> Self {
        Self {
            client,
            helper_model: "claude-sonnet-5".into(),
            prices: Arc::new(PriceTable::default()),
            plan_quotas: Arc::new(BTreeMap::from([("standard".to_owned(), 2_000_000)])),
        }
    }

    /// `anthropic`, `fake` or `disabled`.
    pub fn provider(&self) -> &'static str {
        match &self.client {
            Some(Client::Anthropic(_)) => "anthropic",
            Some(Client::Fake(_)) => "fake",
            None => "disabled",
        }
    }

    fn quota_for(&self, plan: &str, overridden: Option<i64>) -> i64 {
        overridden
            .or_else(|| self.plan_quotas.get(plan).copied())
            .or_else(|| self.plan_quotas.get("standard").copied())
            .unwrap_or(0)
    }
}

/// `503 service_unavailable`: no provider configured (production without a key).
pub fn unavailable() -> Error {
    Error::Unavailable("AI helpers are not configured (ANTHROPIC_API_KEY)".into())
}

/// `402 ai_quota_exceeded`.
pub fn quota_exceeded() -> Error {
    Error::PaymentRequired {
        code: "ai_quota_exceeded",
        detail: "the monthly AI allowance of this shop is used up".into(),
    }
}

// ---------------------------------------------------------------------------------------
// Quota and usage

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeatureUsage {
    pub feature: String,
    pub calls: i64,
    pub tokens: i64,
    pub cost_micros: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UsageSummary {
    /// `anthropic`, `fake` (demo fixtures, no key configured) or `disabled`.
    pub provider: String,
    pub model: String,
    /// Start of the metered calendar month (UTC).
    pub month_start: DateTime<Utc>,
    /// Tokens used this month (input incl. cache reads/writes + output).
    pub tokens_used: i64,
    /// Monthly allowance: the plan default or a superadmin override.
    pub tokens_quota: i64,
    /// USD micros this month at the configured list prices.
    pub cost_micros: i64,
    pub by_feature: Vec<FeatureUsage>,
}

pub fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

async fn quota_state(tx: &mut TenantTx, ai: &Ai) -> Result<(i64, i64, i64), Error> {
    let tenant = sqlx::query!(
        "SELECT plan, ai_monthly_tokens FROM platform.tenants WHERE id = $1",
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    let used = sqlx::query!(
        r#"SELECT coalesce(sum(input_tokens + output_tokens + cache_read_tokens
                               + cache_write_tokens), 0)::bigint AS "tokens!",
                  coalesce(sum(cost_micros), 0)::bigint AS "cost!"
           FROM ai_usage WHERE at >= $1"#,
        month_start(Utc::now())
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok((
        used.tokens,
        ai.quota_for(&tenant.plan, tenant.ai_monthly_tokens),
        used.cost,
    ))
}

/// `402 ai_quota_exceeded` when this month's usage reached the allowance.
/// ponytail: a soft limit; two calls started at the same moment may both pass. Reserve tokens
/// up front if overshoot by one call ever matters.
pub async fn ensure_quota(tx: &mut TenantTx, ai: &Ai) -> Result<(), Error> {
    if ai.client.is_none() {
        return Err(unavailable());
    }
    let (used, quota, _) = quota_state(tx, ai).await?;
    if used >= quota {
        return Err(quota_exceeded());
    }
    Ok(())
}

pub async fn usage(tx: &mut TenantTx, ai: &Ai) -> Result<UsageSummary, Error> {
    let (tokens_used, tokens_quota, cost_micros) = quota_state(tx, ai).await?;
    let start = month_start(Utc::now());
    let by_feature = sqlx::query_as!(
        FeatureUsage,
        r#"SELECT feature, count(*) AS "calls!",
                  sum(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens)::bigint AS "tokens!",
                  sum(cost_micros)::bigint AS "cost_micros!"
           FROM ai_usage WHERE at >= $1 GROUP BY feature ORDER BY feature"#,
        start
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(UsageSummary {
        provider: ai.provider().into(),
        model: ai.helper_model.clone(),
        month_start: start,
        tokens_used,
        tokens_quota,
        cost_micros,
        by_feature,
    })
}

/// Why a metered call produced nothing usable. `code()` is what proposals and plans record.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("the monthly AI allowance is used up")]
    Quota,
    /// Worth retrying later (provider unavailable).
    #[error("AI provider unavailable: {0}")]
    Unavailable(String),
    /// Retrying the same request cannot help.
    #[error("{code}: {detail}")]
    Failed { code: &'static str, detail: String },
    #[error(transparent)]
    Platform(#[from] Error),
}

impl From<sqlx::Error> for CallError {
    fn from(e: sqlx::Error) -> Self {
        Self::Platform(e.into())
    }
}

impl CallError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Quota => "ai_quota_exceeded",
            Self::Unavailable(_) => "ai_unavailable",
            Self::Failed { code, .. } => code,
            Self::Platform(e) => e.code(),
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Unavailable(_) => true,
            Self::Platform(e) => e.status().is_server_error(),
            Self::Quota | Self::Failed { .. } => false,
        }
    }

    pub(crate) fn output(detail: impl Into<String>) -> Self {
        Self::Failed {
            code: "ai_invalid_output",
            detail: detail.into(),
        }
    }
}

/// One model call for a helper.
pub(crate) struct Call<'a> {
    pub feature: &'static str,
    pub system: &'a str,
    pub task: &'a str,
    pub data: &'a Value,
    pub schema: &'a Value,
    pub max_tokens: u32,
}

/// Quota check, the call, then the `ai_usage` row (also for failures that consumed tokens).
/// Returns the untrusted JSON output and the model that produced it.
pub(crate) async fn call(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    actor: &str,
    c: &Call<'_>,
) -> Result<(Value, String), CallError> {
    let client = ai.client.as_ref().ok_or(CallError::Unavailable(
        "AI helpers are not configured".into(),
    ))?;
    {
        let mut tx = tenant_tx(db, tenant).await?;
        let (used, quota, _) = quota_state(&mut tx, ai).await?;
        tx.commit().await?;
        if used >= quota {
            return Err(CallError::Quota);
        }
    }
    let result = client
        .complete(&Request {
            feature: c.feature,
            model: &ai.helper_model,
            system: c.system,
            task: c.task,
            data: c.data,
            schema: c.schema,
            max_tokens: c.max_tokens,
        })
        .await;
    let (usage, model) = match &result {
        Ok(done) => (done.usage, done.model.clone()),
        Err(Failure { usage, model, .. }) => (*usage, model.clone()),
    };
    if usage.total() > 0 {
        let model = if model.is_empty() {
            ai.helper_model.clone()
        } else {
            model
        };
        let cost = ai.prices.cost_micros(&model, &usage);
        let n = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        let mut tx = tenant_tx(db, tenant).await?;
        sqlx::query!(
            "INSERT INTO ai_usage (tenant_id, feature, model, input_tokens, output_tokens,
                                   cache_read_tokens, cache_write_tokens, cost_micros, actor)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            tenant,
            c.feature,
            model,
            n(usage.input_tokens),
            n(usage.output_tokens),
            n(usage.cache_read_input_tokens),
            n(usage.cache_creation_input_tokens),
            n(cost),
            actor
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    match result {
        Ok(done) => Ok((done.output, done.model)),
        Err(f) => Err(match f.error {
            AiError::Unavailable(detail) => CallError::Unavailable(detail),
            AiError::Rejected(detail) => CallError::Failed {
                code: "ai_unavailable",
                detail,
            },
            AiError::Refused => CallError::Failed {
                code: "ai_refused",
                detail: "the model declined the request".into(),
            },
            AiError::Truncated => CallError::Failed {
                code: "ai_invalid_output",
                detail: "the output was cut off".into(),
            },
            AiError::InvalidOutput(detail) => CallError::output(detail),
        }),
    }
}

/// Deserializes the model output into `T` (unknown fields rejected).
pub(crate) fn parse<T: serde::de::DeserializeOwned>(output: Value) -> Result<T, CallError> {
    serde_json::from_value(output).map_err(|e| CallError::output(e.to_string()))
}

/// What a job runner should do after a step.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Try again later (the record stays pending).
    Retry(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_start_is_first_of_month_utc() {
        let now = Utc.with_ymd_and_hms(2026, 9, 25, 13, 7, 1).unwrap();
        assert_eq!(
            month_start(now),
            Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn quota_prefers_override_then_plan_then_standard() {
        let mut ai = Ai::fake();
        ai.plan_quotas = Arc::new(BTreeMap::from([
            ("standard".to_owned(), 100),
            ("pro".to_owned(), 1000),
        ]));
        assert_eq!(ai.quota_for("pro", None), 1000);
        assert_eq!(ai.quota_for("unknown", None), 100);
        assert_eq!(ai.quota_for("pro", Some(5)), 5);
    }
}
