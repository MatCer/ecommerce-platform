pub mod detect;
mod load;
mod readout;

use chrono::{DateTime, NaiveDate, Utc};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::NewJob;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ai::{self, Ai, Call, CallError, Outcome, prompts, settings};
use crate::{analytics, audit};
use detect::Detection;
use detect::rules::{
    FUNNEL_BASELINE_DAYS, FUNNEL_RECENT_DAYS, PRODUCT_DAYS, VITALS_DAYS, ZERO_RESULT_DAYS,
};
pub use readout::{Measure, ThemeChange, Window};

pub const JOB: &str = "insights.run";
pub const DAILY_MODEL_CALLS: i64 = 3;
pub const EARLIEST_RUN_HOUR_UTC: i64 = 4;
pub const LIST_LIMIT: i64 = 50;
pub const HYPOTHESIS_MAX: usize = 500;
pub const THEME_PROMPT_MAX: usize = 1500;
pub const MOVE_FACTOR: f64 = 1.5;
const ACTOR: &str = "system";

/// The run of one tenant over `day` (the payload carries it, so a late or retried job still
/// evaluates the same day).
pub fn job(tenant: Uuid, day: NaiveDate) -> NewJob<'static> {
    let mut j = NewJob::new(JOB, json!({ "day": day }));
    j.tenant_id = Some(tenant);
    j.max_attempts = 3;
    j.idempotency_key = Some(format!("{JOB}:{tenant}:{day}"));
    j
}

/// A ranked signal as staff see it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Insight {
    pub id: Uuid,
    /// `resolved` once the detector stopped firing; such insights are listed only with a
    /// `theme_change`.
    pub status: Status,
    pub detection: Detection,
    /// Affected visits per week; the list is ordered by it.
    pub score: f64,
    /// Low add-to-cart: the product's name in the default market's language (`null` once the
    /// product is gone).
    pub product_name: Option<String>,
    pub first_detected_on: NaiveDate,
    pub last_detected_on: NaiveDate,
    /// AI-generated; label it as such.
    pub hypothesis: Option<Hypothesis>,
    /// The latest theme change published from this insight's draft (`null` when none).
    pub theme_change: Option<ThemeChange>,
    /// Set when a dismissed signal came back with at least 1.5 times its score at dismissal.
    pub reopened: Option<Reopened>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
#[schema(as = InsightReopened)]
pub struct Reopened {
    /// The run day that reopened it.
    pub on: NaiveDate,
    /// The score when staff dismissed it.
    pub dismissed_score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = InsightStatus)]
pub enum Status {
    Open,
    Dismissed,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Hypothesis {
    pub text: String,
    /// Draft request for the AI theme editor; staff edit and start it themselves.
    pub theme_prompt: String,
    pub model: String,
    pub generated_at: DateTime<Utc>,
}

struct Row {
    id: Uuid,
    status: String,
    detection: Value,
    score: f64,
    product_name: Option<String>,
    first_detected_on: NaiveDate,
    last_detected_on: NaiveDate,
    hypothesis: Option<String>,
    theme_prompt: Option<String>,
    hypothesis_model: Option<String>,
    hypothesis_at: Option<DateTime<Utc>>,
    dismissed_score: Option<f64>,
    reopened_on: Option<NaiveDate>,
    revision_id: Option<Uuid>,
    revision_number: Option<i32>,
    published_at: Option<DateTime<Utc>>,
}

fn parse_detection(v: Value) -> Result<Detection, Error> {
    serde_json::from_value(v).map_err(|e| Error::Internal(format!("stored detection: {e}")))
}

fn parse_status(s: &str) -> Result<Status, Error> {
    match s {
        "open" => Ok(Status::Open),
        "dismissed" => Ok(Status::Dismissed),
        "resolved" => Ok(Status::Resolved),
        other => Err(Error::Internal(format!("stored insight status: {other}"))),
    }
}

impl TryFrom<Row> for Insight {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self, Error> {
        let hypothesis = match (
            r.hypothesis,
            r.theme_prompt,
            r.hypothesis_model,
            r.hypothesis_at,
        ) {
            (Some(text), Some(theme_prompt), Some(model), Some(generated_at)) => Some(Hypothesis {
                text,
                theme_prompt,
                model,
                generated_at,
            }),
            _ => None,
        };
        let reopened = match (r.reopened_on, r.dismissed_score) {
            (Some(on), Some(dismissed_score)) => Some(Reopened {
                on,
                dismissed_score,
            }),
            (Some(_), None) => {
                return Err(Error::Internal(
                    "reopened insight without a dismissal score".into(),
                ));
            }
            (None, _) => None,
        };
        Ok(Self {
            id: r.id,
            status: parse_status(&r.status)?,
            detection: parse_detection(r.detection)?,
            score: r.score,
            product_name: r.product_name,
            first_detected_on: r.first_detected_on,
            last_detected_on: r.last_detected_on,
            hypothesis,
            theme_change: None,
            reopened,
        })
    }
}

pub async fn list(tx: &mut TenantTx, today: NaiveDate) -> Result<Vec<Insight>, Error> {
    select(tx, None, today).await
}

async fn select(
    tx: &mut TenantTx,
    id: Option<Uuid>,
    today: NaiveDate,
) -> Result<Vec<Insight>, Error> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT s.id, s.status, s.detection, s.score, s.first_detected_on, s.last_detected_on,
                  s.hypothesis, s.theme_prompt, s.hypothesis_model, s.hypothesis_at,
                  s.dismissed_score, s.reopened_on,
                  CASE WHEN s.kind = 'low_add_to_cart'
                       THEN insight_product_name((s.detection->>'product_id')::uuid) END
                      AS product_name,
                  p.id AS "revision_id?", p.number AS "revision_number?",
                  p.published_at AS "published_at?"
           FROM insight_signals s
           LEFT JOIN LATERAL (
               SELECT v.id, v.number, v.published_at
               FROM ai_theme_runs r JOIN theme_revisions v ON v.id = r.revision_id
               WHERE r.insight_id = s.id AND v.published_at IS NOT NULL
               ORDER BY v.published_at DESC LIMIT 1
           ) p ON true
           WHERE CASE WHEN $1::uuid IS NULL
                      THEN s.status = 'open' OR (s.status = 'resolved' AND p.id IS NOT NULL)
                      ELSE s.id = $1 END
           ORDER BY s.score DESC, s.kind COLLATE "C", s.subject COLLATE "C" LIMIT $2"#,
        id,
        LIST_LIMIT
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let published = match (r.revision_id, r.revision_number, r.published_at) {
            (Some(revision_id), Some(revision_number), Some(published_at)) => {
                Some(readout::Published {
                    revision_id,
                    revision_number,
                    published_at,
                })
            }
            _ => None,
        };
        let mut insight = Insight::try_from(r)?;
        if let Some(p) = published {
            insight.theme_change =
                Some(readout::theme_change(tx, &insight.detection, p, today).await?);
        }
        items.push(insight);
    }
    Ok(items)
}

pub async fn dismiss(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    today: NaiveDate,
) -> Result<Insight, Error> {
    let changed = sqlx::query!(
        "UPDATE insight_signals SET status = 'dismissed', dismissed_at = now(), dismissed_by = $2,
                                    dismissed_score = score, reopened_on = NULL
         WHERE id = $1 AND status <> 'dismissed' RETURNING kind, subject",
        id,
        actor
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(c) = changed {
        audit::record(
            tx,
            actor,
            "insight.dismissed",
            "insight_signal",
            Some(&id.to_string()),
            &json!({ "kind": c.kind, "subject": c.subject }),
        )
        .await?;
    }
    select(tx, Some(id), today)
        .await?
        .pop()
        .ok_or(Error::NotFound)
}

async fn lock_tenant_run(db: &PgPool, tenant: Uuid) -> Result<Option<PgConnection>, Error> {
    let mut conn = db.acquire().await?.detach();
    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock(hashtextextended('insights:' || $1::text, 0)) AS "ok!""#,
        tenant.to_string()
    )
    .fetch_one(&mut conn)
    .await?;
    Ok(locked.then_some(conn))
}

async fn unlock_tenant_run(mut conn: PgConnection, tenant: Uuid) -> Result<(), Error> {
    sqlx::query_scalar!(
        r#"SELECT pg_advisory_unlock(hashtextextended('insights:' || $1::text, 0)) AS "ok!""#,
        tenant.to_string()
    )
    .fetch_one(&mut conn)
    .await?;
    Ok(())
}

async fn calls_today(db: &PgPool, tenant: Uuid) -> Result<i64, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let n = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM ai_usage
           WHERE feature = $1
             AND at >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'"#,
        ai::INSIGHT_HYPOTHESIS
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(n)
}

async fn store(tx: &mut TenantTx, day: NaiveDate, found: &[Detection]) -> Result<(), Error> {
    for d in found {
        let detection =
            serde_json::to_value(d).map_err(|e| Error::Internal(format!("detection: {e}")))?;
        let reopened = sqlx::query!(
            r#"UPDATE insight_signals SET status = 'open', reopened_on = $3
               WHERE kind = $1 AND subject = $2 AND status = 'dismissed'
                 AND $4 >= $5 * dismissed_score AND $3 >= last_detected_on
               RETURNING id, dismissed_score AS "dismissed_score!""#,
            d.kind(),
            d.subject(),
            day,
            d.score(),
            MOVE_FACTOR
        )
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(r) = reopened {
            audit::record(
                tx,
                ACTOR,
                "insight.reopened",
                "insight_signal",
                Some(&r.id.to_string()),
                &json!({ "kind": d.kind(), "subject": d.subject(), "score": d.score(),
                         "dismissed_score": r.dismissed_score }),
            )
            .await?;
        }
        sqlx::query!(
            "INSERT INTO insight_signals (tenant_id, id, kind, subject, detection, score,
                                          first_detected_on, last_detected_on)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
             ON CONFLICT (tenant_id, kind, subject) DO UPDATE
             SET detection = EXCLUDED.detection, score = EXCLUDED.score,
                 last_detected_on = EXCLUDED.last_detected_on,
                 status = CASE insight_signals.status WHEN 'resolved' THEN 'open'
                               ELSE insight_signals.status END
             WHERE EXCLUDED.last_detected_on >= insight_signals.last_detected_on",
            tx.tenant_id(),
            Uuid::now_v7(),
            d.kind(),
            d.subject(),
            detection,
            d.score(),
            day
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!(
            "INSERT INTO insight_scores (tenant_id, signal_id, day, score)
             SELECT tenant_id, id, $3, $4 FROM insight_signals WHERE kind = $1 AND subject = $2
             ON CONFLICT (tenant_id, signal_id, day) DO UPDATE SET score = EXCLUDED.score",
            d.kind(),
            d.subject(),
            day,
            d.score()
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE insight_signals SET status = 'resolved'
         WHERE status = 'open' AND last_detected_on < $1",
        day
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

struct Target {
    id: Uuid,
    detection: Value,
    product_name: Option<String>,
    score: f64,
}

fn facts(d: &Detection, product: Option<&str>, locale: &str) -> Value {
    let r4 = |x: f64| (x * 10_000.0).round() / 10_000.0;
    let mut v = match d {
        Detection::FunnelDrop {
            transition,
            recent,
            baseline,
        } => json!({
            "transition": transition.as_str(),
            "recent": { "days": FUNNEL_RECENT_DAYS, "sessions_at_step": recent.from,
                        "sessions_continuing": recent.to,
                        "rate": r4(detect::rate(recent.to, recent.from)) },
            "baseline": { "days": FUNNEL_BASELINE_DAYS, "sessions_at_step": baseline.from,
                          "sessions_continuing": baseline.to,
                          "rate": r4(detect::rate(baseline.to, baseline.from)) },
        }),
        Detection::LowAddToCart {
            views,
            adds,
            shop_views,
            shop_adds,
            ..
        } => json!({
            "product": product,
            "days": PRODUCT_DAYS,
            "views": views,
            "add_to_carts": adds,
            "rate": r4(detect::rate(*adds, *views)),
            "shop_rate": r4(detect::rate(*shop_adds, *shop_views)),
        }),
        Detection::SlowPage {
            template,
            metric,
            p75,
            samples,
            requests,
        } => json!({
            "template": template,
            "metric": metric,
            "p75": r4(*p75),
            "good_bound": metric.good(),
            "samples": samples,
            "page_requests": requests,
            "days": VITALS_DAYS,
        }),
        Detection::ZeroResultSearch { query, searches } => json!({
            "query": query,
            "searches_without_results": searches,
            "days": ZERO_RESULT_DAYS,
        }),
    };
    v["kind"] = json!(d.kind());
    v["affected_visits_per_week"] = json!(r4(d.score()));
    v["locale"] = json!(locale);
    v
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    hypothesis: String,
    theme_prompt: String,
}

fn has_markdown(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let inside_word = |i: usize| {
        i > 0
            && chars[i - 1].is_alphanumeric()
            && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric())
    };
    let after_number = text.trim_start_matches(|c: char| c.is_ascii_digit());
    let list = text.starts_with("- ")
        || text.starts_with("+ ")
        || (after_number.len() < text.len() && after_number.starts_with(". "));
    list || chars.iter().enumerate().any(|(i, c)| {
        matches!(c, '*' | '`' | '#' | '[' | ']' | '>') || (*c == '_' && !inside_word(i))
    })
}

fn plain(
    raw: &str,
    chars: std::ops::RangeInclusive<usize>,
    what: &str,
) -> Result<String, CallError> {
    let text = ai::safe_text(raw, chars, what)?;
    if has_markdown(&text) {
        return Err(CallError::output(format!("{what} contains Markdown")));
    }
    Ok(text)
}

fn validate(output: Value) -> Result<(String, String), CallError> {
    let out: Output = ai::parse(output)?;
    Ok((
        plain(&out.hypothesis, 1..=HYPOTHESIS_MAX, "the hypothesis")?,
        plain(&out.theme_prompt, 20..=THEME_PROMPT_MAX, "the theme prompt")?,
    ))
}

/// Detects, stores and explains one tenant's signals for `day`. While another run of the same
/// tenant is in progress it does nothing.
pub async fn run(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    day: NaiveDate,
    last_attempt: bool,
) -> Result<Outcome, Error> {
    let Some(lock) = lock_tenant_run(db, tenant).await? else {
        return Ok(Outcome::Done);
    };
    let outcome = run_locked(db, ai, tenant, day, last_attempt).await?;
    unlock_tenant_run(lock, tenant).await?;
    Ok(outcome)
}

async fn run_locked(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    day: NaiveDate,
    last_attempt: bool,
) -> Result<Outcome, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    analytics::rollup(&mut tx, day).await?;
    let mut found = detect::detect(&load::metrics(&mut tx, day).await?);
    detect::rank(&mut found);
    store(&mut tx, day, &found).await?;
    let on = settings::get(&mut tx).await?.insights;
    tx.commit().await?;
    if !on || ai.client.is_none() {
        return Ok(Outcome::Done);
    }

    let mut tx = tenant_tx(db, tenant).await?;
    let targets = sqlx::query_as!(
        Target,
        r#"SELECT id, detection,
                  CASE WHEN kind = 'low_add_to_cart'
                       THEN insight_product_name((detection->>'product_id')::uuid) END
                      AS product_name,
                  score
           FROM insight_signals WHERE status = 'open' AND hypothesis_at IS NULL
           ORDER BY score DESC, kind COLLATE "C", subject COLLATE "C" LIMIT $1"#,
        DAILY_MODEL_CALLS
    )
    .fetch_all(&mut *tx)
    .await?;
    let locale = sqlx::query_scalar!(
        r#"SELECT coalesce((SELECT default_locale FROM markets WHERE is_default), 'en') AS "l!""#
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    for t in targets {
        if calls_today(db, tenant).await? >= DAILY_MODEL_CALLS {
            break;
        }
        let detection = parse_detection(t.detection)?;
        let data = facts(&detection, t.product_name.as_deref(), &locale);
        let task = format!(
            "Explain this finding and draft a theme-editor request. Write both in the language \
             {locale}."
        );
        let result = ai::call(
            db,
            ai,
            tenant,
            ACTOR,
            &Call {
                feature: ai::INSIGHT_HYPOTHESIS,
                system: prompts::INSIGHT_SYSTEM,
                task: &task,
                data: &data,
                schema: &prompts::insight_schema(),
                max_tokens: 1500,
                image: None,
            },
        )
        .await
        .and_then(|(output, model)| validate(output).map(|v| (v, model)));
        let mut tx = tenant_tx(db, tenant).await?;
        match result {
            Ok(((text, prompt), model)) => {
                sqlx::query!(
                    "UPDATE insight_signals SET hypothesis = $2, theme_prompt = $3,
                            hypothesis_model = $4, hypothesis_at = now(), hypothesis_score = $5
                     WHERE id = $1 AND hypothesis_at IS NULL",
                    t.id,
                    text,
                    prompt,
                    model,
                    t.score
                )
                .execute(&mut *tx)
                .await?;
            }
            Err(CallError::Platform(e)) => return Err(e),
            Err(CallError::Quota) => return Ok(Outcome::Done),
            Err(e) if e.retryable() && !last_attempt => return Ok(Outcome::Retry(e.to_string())),
            Err(e) if e.retryable() => {
                tracing::warn!(%tenant, signal = %t.id, code = e.code(), "insight hypothesis gave up");
                return Ok(Outcome::Done);
            }
            Err(e) => {
                tracing::warn!(%tenant, signal = %t.id, code = e.code(), "insight hypothesis rejected");
                sqlx::query!(
                    "UPDATE insight_signals SET hypothesis_error = $2, hypothesis_at = now(),
                                                hypothesis_score = $3
                     WHERE id = $1 AND hypothesis_at IS NULL",
                    t.id,
                    e.code(),
                    t.score
                )
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
    }
    Ok(Outcome::Done)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn output_is_plain_text_within_bounds() {
        let ok = json!({ "hypothesis": " Fewer\tvisitors add it. ",
                         "theme_prompt": "Make the add to cart button larger." });
        assert_eq!(
            validate(ok).unwrap(),
            (
                "Fewer visitors add it.".to_owned(),
                "Make the add to cart button larger.".to_owned()
            )
        );
        for bad in [
            json!({ "hypothesis": "See https://x.example", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "Fine.", "theme_prompt": "**Bold** the add to cart button." }),
            json!({ "hypothesis": "Rating -> 5", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "Use `code`", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "See [the shop](x)", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "# Heading", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "Fine.", "theme_prompt": "Too short." }),
            json!({ "hypothesis": "", "theme_prompt": "Make the button larger." }),
            json!({ "hypothesis": "Fine.", "theme_prompt": "Make the button larger.", "x": 1 }),
        ] {
            assert_eq!(
                validate(bad.clone()).unwrap_err().code(),
                "ai_invalid_output",
                "{bad}"
            );
        }
    }

    #[test]
    fn markdown_is_rejected_and_underscores_inside_words_pass() {
        let with = |h: &str| json!({ "hypothesis": h, "theme_prompt": "Make the button larger." });
        for ok in ["add-to-cart rate fell", "view_item_to_add_to_cart"] {
            assert_eq!(validate(with(ok)).unwrap().0, ok);
        }
        for bad in [
            "The button is *hard to find*.",
            "Make the _add to cart_ button more prominent.",
            "Make the __primary action__ more prominent.",
            "- bigger photos",
            "+ bigger photos",
            "1. bigger photos",
            "Close the list ]",
        ] {
            assert_eq!(
                validate(with(bad)).unwrap_err().code(),
                "ai_invalid_output",
                "{bad}"
            );
        }
    }

    #[test]
    fn jobs_are_one_per_tenant_and_day() {
        let t = Uuid::from_u128(7);
        let day = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let j = job(t, day);
        assert_eq!(
            (
                j.kind,
                j.tenant_id,
                j.max_attempts,
                j.idempotency_key.as_deref(),
                j.payload
            ),
            (
                "insights.run",
                Some(t),
                3,
                Some("insights.run:00000000-0000-0000-0000-000000000007:2026-10-16"),
                json!({ "day": "2026-10-16" })
            )
        );
    }
}
