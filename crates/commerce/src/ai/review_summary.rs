use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::fields::EntityType;
use super::marks::{self, value_hash};
use super::{Ai, Call, CallError, Outcome, prompts};
use crate::audit;
use crate::reviews::{self, plain_text};
use crate::storefront::Context;

pub const JOB: &str = "ai.review_summary";
pub const FEATURE: &str = "review_summary";
pub const FIELD: &str = "review_summary";
pub const MIN_REVIEWS: i64 = 5;
pub const MAX_INPUT_REVIEWS: i64 = 40;
pub const BODY_CHARS: usize = 1000;
pub const SUMMARY_MAX: usize = 600;
pub const DEBOUNCE_SECS: i64 = 300;
pub const CHANGED_EVENT: &str = "review_summary.changed";
pub const SETTINGS_CHANGED_EVENT: &str = "ai_settings.changed";
const ACTOR: &str = "system";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AiSettings {
    /// AI summaries of verified reviews on product pages, labelled as AI-generated. Off by
    /// default.
    pub review_summaries: bool,
}

/// An AI summary as the shop shows it (plain text; escape on output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReviewSummary {
    pub text: String,
    /// Language of the text (`lang` attribute).
    pub locale: String,
    /// Published verified reviews it was written from.
    pub review_count: i32,
    pub generated_at: DateTime<Utc>,
}

pub async fn settings(tx: &mut TenantTx) -> Result<AiSettings, Error> {
    Ok(
        sqlx::query_scalar!("SELECT review_summaries FROM ai_settings")
            .fetch_optional(&mut **tx)
            .await?
            .map(|review_summaries| AiSettings { review_summaries })
            .unwrap_or_default(),
    )
}

pub async fn put_settings(
    tx: &mut TenantTx,
    actor: &str,
    input: &AiSettings,
) -> Result<AiSettings, Error> {
    sqlx::query!(
        "INSERT INTO ai_settings (tenant_id) VALUES ($1) ON CONFLICT (tenant_id) DO NOTHING",
        tx.tenant_id()
    )
    .execute(&mut **tx)
    .await?;
    let before = AiSettings {
        review_summaries: sqlx::query_scalar!(
            "SELECT review_summaries FROM ai_settings WHERE tenant_id = $1 FOR UPDATE",
            tx.tenant_id()
        )
        .fetch_one(&mut **tx)
        .await?,
    };
    sqlx::query!(
        "UPDATE ai_settings SET review_summaries = $2, updated_at = now() WHERE tenant_id = $1",
        tx.tenant_id(),
        input.review_summaries
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
    if before != *input {
        queue::publish(&mut **tx, SETTINGS_CHANGED_EVENT, &json!({})).await?;
    }
    if input.review_summaries && !before.review_summaries {
        let products = sqlx::query_scalar!(
            r#"SELECT product_id AS "product_id!" FROM reviews
               WHERE status = 'published' AND verified
               GROUP BY product_id HAVING count(*) >= $1
               UNION SELECT product_id FROM review_summaries"#,
            MIN_REVIEWS
        )
        .fetch_all(&mut **tx)
        .await?;
        let tenant_id = tx.tenant_id();
        for product_id in products {
            queue::enqueue(&mut **tx, &new_job(tenant_id, product_id)).await?;
        }
    }
    Ok(*input)
}

fn new_job(tenant_id: Uuid, product_id: Uuid) -> NewJob<'static> {
    let mut j = NewJob::new(JOB, json!({ "product_id": product_id }));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 3;
    j
}

pub fn debounced_job(tenant_id: Uuid, product_id: Uuid, now: DateTime<Utc>) -> NewJob<'static> {
    let slot = now.timestamp().div_euclid(DEBOUNCE_SECS);
    let mut j = new_job(tenant_id, product_id);
    j.idempotency_key = Some(format!("{JOB}:{tenant_id}:{product_id}:{slot}"));
    j.run_at = DateTime::from_timestamp((slot + 1) * DEBOUNCE_SECS, 0);
    j
}

pub fn job_for_event(
    tenant_id: Option<Uuid>,
    event_type: &str,
    payload: &Value,
    now: DateTime<Utc>,
) -> Option<NewJob<'static>> {
    matches!(event_type, reviews::CHANGED_EVENT | "product.updated").then_some(())?;
    let product_id = payload
        .get("product_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())?;
    Some(debounced_job(tenant_id?, product_id, now))
}

struct Input {
    data: Value,
    sha256: String,
    review_count: i64,
    locales: Vec<String>,
}

async fn load(tx: &mut TenantTx, product_id: Uuid) -> Result<Option<Input>, Error> {
    let Some(product) = sqlx::query_scalar!(
        "SELECT name FROM product_translations WHERE product_id = $1
         ORDER BY (locale = (SELECT default_locale FROM markets WHERE is_default)) DESC NULLS LAST,
                  locale
         LIMIT 1",
        product_id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let mut locales =
        sqlx::query_scalar!(r#"SELECT DISTINCT unnest(locales) AS "locale!" FROM markets"#)
            .fetch_all(&mut **tx)
            .await?;
    locales.sort();
    let counts = sqlx::query!(
        r#"SELECT rating, count(*) AS "n!" FROM reviews
           WHERE product_id = $1 AND status = 'published' AND verified GROUP BY rating"#,
        product_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let rating = reviews::rating_summary(counts.iter().map(|c| (c.rating, c.n)));
    let rows = sqlx::query!(
        "SELECT id, rating, title, body, locale FROM reviews
         WHERE product_id = $1 AND status = 'published' AND verified
         ORDER BY published_at DESC, id DESC LIMIT $2",
        product_id,
        MAX_INPUT_REVIEWS
    )
    .fetch_all(&mut **tx)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "rating": r.rating,
                "title": r.title,
                "body": r.body.chars().take(BODY_CHARS).collect::<String>(),
                "locale": r.locale,
            })
        })
        .collect();
    let data = json!({
        "product": product,
        "locales": locales,
        "rating": rating,
        "reviews": items,
    });
    let sha256 = value_hash(&json!({
        "system": prompts::REVIEW_SUMMARY_SYSTEM,
        "schema": prompts::review_summary_schema(),
        "ids": ids,
        "data": data,
    }));
    Ok(Some(Input {
        data,
        sha256,
        review_count: rating.as_ref().map_or(0, |r| r.count),
        locales,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    summaries: Vec<OutputItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputItem {
    locale: String,
    summary: String,
}

fn validate(output: Value, locales: &[String]) -> Result<BTreeMap<String, String>, CallError> {
    let out: Output = super::parse(output)?;
    let mut by_locale = BTreeMap::new();
    for item in out.summaries {
        let text = plain_text(&item.summary, false);
        if !(1..=SUMMARY_MAX).contains(&text.chars().count()) {
            return Err(CallError::output(format!(
                "a summary must be 1-{SUMMARY_MAX} characters"
            )));
        }
        let lower = text.to_lowercase();
        if ["//", "www.", "@", "<"].iter().any(|p| lower.contains(p)) {
            return Err(CallError::output(
                "a summary contains a link, an email address or markup",
            ));
        }
        if by_locale.insert(item.locale, text).is_some() {
            return Err(CallError::output("a locale is summarized twice"));
        }
    }
    if !by_locale.keys().eq(locales.iter()) {
        return Err(CallError::output(format!(
            "the summaries do not cover exactly the requested locales {locales:?}"
        )));
    }
    Ok(by_locale)
}

pub(crate) async fn lock_inputs(tx: &mut TenantTx, product_id: Uuid) -> Result<(), Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('review_summary:' || $1::text, 0))",
        product_id.to_string()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn stored_hash(tx: &mut TenantTx, product_id: Uuid) -> Result<Option<String>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT input_sha256 FROM review_summaries WHERE product_id = $1 LIMIT 1",
        product_id
    )
    .fetch_optional(&mut **tx)
    .await?)
}

pub async fn run(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    product_id: Uuid,
    last_attempt: bool,
) -> Result<Outcome, Error> {
    if ai.client.is_none() {
        return Ok(Outcome::Done);
    }
    let (input, stored) = {
        let mut tx = tenant_tx(db, tenant).await?;
        if !settings(&mut tx).await?.review_summaries {
            return Ok(Outcome::Done);
        }
        let Some(input) = load(&mut tx, product_id).await? else {
            return Ok(Outcome::Done);
        };
        let stored = stored_hash(&mut tx, product_id).await?;
        tx.commit().await?;
        (input, stored)
    };
    if input.review_count < MIN_REVIEWS || input.locales.is_empty() {
        if stored.is_some() {
            store_if_input_unchanged(db, tenant, product_id, &input.sha256, None).await?;
        }
        return Ok(Outcome::Done);
    }
    if stored.as_deref() == Some(input.sha256.as_str()) {
        return Ok(Outcome::Done);
    }
    let task = format!(
        "Summarize the reviews of this product in these languages: {}.",
        input.locales.join(", ")
    );
    let call = super::call(
        db,
        ai,
        tenant,
        ACTOR,
        &Call {
            feature: FEATURE,
            system: prompts::REVIEW_SUMMARY_SYSTEM,
            task: &task,
            data: &input.data,
            schema: &prompts::review_summary_schema(),
            max_tokens: 2000,
        },
    )
    .await;
    let result =
        call.and_then(|(output, model)| validate(output, &input.locales).map(|s| (s, model)));
    match result {
        Ok((summaries, model)) => {
            let fresh = Fresh {
                summaries: &summaries,
                model: &model,
                review_count: input.review_count,
            };
            store_if_input_unchanged(db, tenant, product_id, &input.sha256, Some(&fresh)).await?;
            Ok(Outcome::Done)
        }
        Err(CallError::Platform(e)) => Err(e),
        Err(e) if e.retryable() && !last_attempt => Ok(Outcome::Retry(e.to_string())),
        Err(e) => {
            tracing::warn!(%product_id, code = e.code(), error = %e, "review summary failed");
            Ok(Outcome::Done)
        }
    }
}

struct Fresh<'a> {
    summaries: &'a BTreeMap<String, String>,
    model: &'a str,
    review_count: i64,
}

async fn store_if_input_unchanged(
    db: &PgPool,
    tenant: Uuid,
    product_id: Uuid,
    sha256: &str,
    fresh: Option<&Fresh<'_>>,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    lock_inputs(&mut tx, product_id).await?;
    if stored_hash(&mut tx, product_id).await?.as_deref() == Some(sha256) {
        return Ok(());
    }
    if load(&mut tx, product_id)
        .await?
        .map(|i| i.sha256)
        .as_deref()
        != Some(sha256)
    {
        return Ok(());
    }
    let entity_id = product_id.to_string();
    let deleted = sqlx::query!(
        "DELETE FROM review_summaries WHERE product_id = $1",
        product_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query!(
        "DELETE FROM ai_marks WHERE entity_type = $1 AND entity_id = $2 AND field = $3",
        EntityType::Product.as_str(),
        entity_id,
        FIELD
    )
    .execute(&mut *tx)
    .await?;
    if let Some(f) = fresh {
        let review_count = i32::try_from(f.review_count).unwrap_or(i32::MAX);
        for (locale, text) in f.summaries {
            sqlx::query!(
                "INSERT INTO review_summaries (tenant_id, product_id, locale, summary,
                                               review_count, input_sha256, model)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                tenant,
                product_id,
                locale,
                text,
                review_count,
                sha256,
                f.model
            )
            .execute(&mut *tx)
            .await?;
            marks::record(
                &mut tx,
                EntityType::Product,
                &entity_id,
                &marks::NewMark {
                    locale,
                    field: FIELD,
                    value: &json!(text),
                    feature: FEATURE,
                    model: f.model,
                    proposal_id: None,
                    accepted_by: ACTOR,
                },
            )
            .await?;
        }
    }
    if fresh.is_some() || deleted > 0 {
        queue::publish(
            &mut *tx,
            CHANGED_EVENT,
            &json!({ "product_id": product_id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn for_storefront(
    tx: &mut TenantTx,
    ctx: &Context,
    product_id: Uuid,
) -> Result<Option<ReviewSummary>, Error> {
    let row = sqlx::query!(
        "SELECT s.summary, s.locale, s.review_count, s.generated_at, m.value_sha256
         FROM review_summaries s
         JOIN ai_settings a ON a.review_summaries
         JOIN ai_marks m ON m.entity_type = $3 AND m.entity_id = s.product_id::text
                        AND m.locale = s.locale AND m.field = $4
         WHERE s.product_id = $1 AND s.locale = $2",
        product_id,
        ctx.locale,
        EntityType::Product.as_str(),
        FIELD
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row
        .filter(|r| value_hash(&json!(r.summary)) == r.value_sha256)
        .map(|r| ReviewSummary {
            text: r.summary,
            locale: r.locale,
            review_count: r.review_count,
            generated_at: r.generated_at,
        }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use chrono::TimeZone;

    use super::*;

    fn locales() -> Vec<String> {
        vec!["cs".into(), "sk".into()]
    }

    fn out(items: &[(&str, &str)]) -> Value {
        json!({ "summaries": items.iter().map(|(l, s)| json!({ "locale": l, "summary": s }))
            .collect::<Vec<_>>() })
    }

    #[test]
    fn valid_output_is_plain_text_per_requested_locale() {
        let got = validate(
            out(&[
                ("sk", "  Zákazníci\tchvália strih. "),
                ("cs", "Chválí střih."),
            ]),
            &locales(),
        )
        .unwrap();
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            vec![
                ("cs".to_owned(), "Chválí střih.".to_owned()),
                ("sk".to_owned(), "Zákazníci chvália strih.".to_owned())
            ]
        );
    }

    #[test]
    fn invalid_output_is_rejected() {
        let long = "a".repeat(SUMMARY_MAX + 1);
        for bad in [
            out(&[("cs", "Jen česky.")]),
            out(&[("cs", "A"), ("sk", "B"), ("en", "C")]),
            out(&[("cs", "A"), ("cs", "B")]),
            out(&[("cs", "Viz https://x.example"), ("sk", "B")]),
            out(&[("cs", "Viz WWW.example.cz"), ("sk", "B")]),
            out(&[("cs", "Pište na info@example.cz"), ("sk", "B")]),
            out(&[("cs", "Viz //evil.example/x"), ("sk", "B")]),
            out(&[("cs", "<b>Super</b>"), ("sk", "B")]),
            out(&[("cs", &long), ("sk", "B")]),
            out(&[("cs", "   "), ("sk", "B")]),
            json!({ "summaries": [], "extra": 1 }),
        ] {
            let e = validate(bad.clone(), &locales()).unwrap_err();
            assert_eq!(e.code(), "ai_invalid_output", "{bad}");
        }
    }

    #[test]
    fn plain_prose_with_decimals_and_abbreviations_passes() {
        let ok =
            "Zákazníci hodnotí tričko s.Oliver průměrně 4,6 z 5. Chválí střih atd. Vel. M sedí.";
        assert!(validate(out(&[("cs", ok), ("sk", "B")]), &locales()).is_ok());
    }

    #[test]
    fn rejection_errors_do_not_echo_model_output() {
        for bad in [
            out(&[("cs", "A"), ("PRIVATE-OUTPUT", "B")]),
            out(&[("cs", "Viz https://PRIVATE-OUTPUT.example"), ("sk", "B")]),
            out(&[("cs", &"PRIVATE-OUTPUT".repeat(60)), ("sk", "B")]),
        ] {
            let e = validate(bad, &locales()).unwrap_err().to_string();
            assert!(!e.contains("PRIVATE-OUTPUT"), "{e}");
        }
    }

    #[test]
    fn review_changes_debounce_per_product() {
        let (t, p) = (Uuid::now_v7(), Uuid::now_v7());
        let now = Utc.with_ymd_and_hms(2026, 10, 16, 12, 1, 0).unwrap();
        let payload = json!({ "product_id": p });
        let a = job_for_event(Some(t), "review.changed", &payload, now).unwrap();
        let b = job_for_event(
            Some(t),
            "review.changed",
            &payload,
            now + chrono::Duration::seconds(200),
        )
        .unwrap();
        assert_eq!(a.kind, "ai.review_summary");
        assert_eq!(a.idempotency_key, b.idempotency_key);
        assert_eq!(
            a.run_at,
            Some(Utc.with_ymd_and_hms(2026, 10, 16, 12, 5, 0).unwrap())
        );
        assert_eq!(a.payload, payload);
        assert_eq!((a.tenant_id, a.max_attempts), (Some(t), 3));
        let other = job_for_event(Some(Uuid::now_v7()), "review.changed", &payload, now).unwrap();
        assert_ne!(other.idempotency_key, a.idempotency_key);
        assert!(job_for_event(Some(t), "review_summary.changed", &payload, now).is_none());
        assert_eq!(
            job_for_event(Some(t), "product.updated", &payload, now).map(|j| j.idempotency_key),
            Some(a.idempotency_key.clone()),
            "a rename can change the input"
        );
        assert!(job_for_event(Some(t), "price.changed", &payload, now).is_none());
        assert!(job_for_event(None, "review.changed", &payload, now).is_none());
        assert!(job_for_event(Some(t), "review.changed", &json!({}), now).is_none());
    }
}
