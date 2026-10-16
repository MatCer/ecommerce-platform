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
use crate::reviews;
use crate::storefront::Context;

pub const JOB: &str = "ai.review_summary";
pub const FEATURE: &str = "review_summary";
pub const FIELD: &str = "review_summary";
pub const MIN_REVIEWS: i64 = 5;
pub const MAX_INPUT_REVIEWS: i64 = 40;
pub const BODY_CHARS: usize = 1000;
pub const SUMMARY_MAX: usize = 600;
const LOCALES_PER_CALL: usize = 4;
pub const DEBOUNCE_SECS: i64 = 300;
pub const CHANGED_EVENT: &str = "review_summary.changed";
const ACTOR: &str = "system";

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

pub(super) async fn enqueue_all(tx: &mut TenantTx) -> Result<(), Error> {
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
    Ok(())
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
    let Some(product) = super::product_name(tx, product_id).await? else {
        return Ok(None);
    };
    let locales = super::market_locales(tx).await?;
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
        let text = super::safe_text(&item.summary, 1..=SUMMARY_MAX, "a summary")?;
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
    let (input, stored) = {
        let mut tx = tenant_tx(db, tenant).await?;
        if !super::settings::get(&mut tx).await?.review_summaries {
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
    if ai.client.is_none() || stored.as_deref() == Some(input.sha256.as_str()) {
        return Ok(Outcome::Done);
    }
    match summarize(db, ai, tenant, &input).await {
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

async fn summarize(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    input: &Input,
) -> Result<(BTreeMap<String, String>, String), CallError> {
    let mut summaries = BTreeMap::new();
    let mut model = String::new();
    for locales in input.locales.chunks(LOCALES_PER_CALL) {
        let mut data = input.data.clone();
        data["locales"] = json!(locales);
        let task = format!(
            "Summarize the reviews of this product in these languages: {}.",
            locales.join(", ")
        );
        let call = Call {
            feature: FEATURE,
            system: prompts::REVIEW_SUMMARY_SYSTEM,
            task: &task,
            data: &data,
            schema: &prompts::review_summary_schema(),
            max_tokens: 2000,
            image: None,
        };
        let (output, m) = super::call(db, ai, tenant, ACTOR, &call).await?;
        summaries.extend(validate(output, locales)?);
        model = m;
    }
    Ok((summaries, model))
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
    fn a_hash_in_plain_prose_passes() {
        let got = validate(
            out(&[("cs", "Customers prefer shade #2."), ("sk", "B")]),
            &locales(),
        )
        .unwrap();
        assert_eq!(got["cs"], "Customers prefer shade #2.");
    }

    #[test]
    fn rejection_errors_do_not_echo_model_output() {
        for bad in [
            out(&[("cs", "A"), ("PRIVATE-OUTPUT", "B")]),
            out(&[("cs", "Viz https://PRIVATE-OUTPUT.example"), ("sk", "B")]),
            out(&[("cs", &"PRIVATE-OUTPUT".repeat(60)), ("sk", "B")]),
            json!({ "PRIVATE-OUTPUT-SENTINEL": 1, "summaries": [] }),
            json!({ "summaries": "PRIVATE-OUTPUT-SENTINEL" }),
            json!({ "summaries": [{ "locale": "cs", "summary": "A", "PRIVATE-OUTPUT": 1 }] }),
        ] {
            let e = validate(bad, &locales()).unwrap_err();
            assert_eq!(e.code(), "ai_invalid_output");
            let (display, debug) = (e.to_string(), format!("{e:?}"));
            assert!(!display.contains("PRIVATE-OUTPUT"), "{display}");
            assert!(!debug.contains("PRIVATE-OUTPUT"), "{debug}");
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
