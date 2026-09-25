//! WP19 lifecycle flows. Every transition runs under tenant RLS and a row lock. The cron
//! scanner is deliberately bounded; the next tick resumes where it left off.

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::consent::{self, ConsentPurpose, Subject};
use crate::notifications::{self, Brand, Email, Template};
use crate::storefront::{self, PublicUrls};

pub const EVENT_JOB: &str = "flows.event";
pub const TICK_JOB: &str = "flows.tick";
const BATCH: i64 = 100;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowConfig {
    /// Hours after the triggering cart activity; only abandoned carts use several steps.
    pub delays_hours: Vec<i64>,
    /// A private, one-use percent coupon on the final abandoned-cart step.
    pub coupon_percent: Option<i32>,
}

impl FlowConfig {
    pub fn for_kind(kind: &str) -> Self {
        Self {
            delays_hours: match kind {
                "abandoned_cart" => vec![1, 24, 72],
                "review_invite" => vec![7 * 24],
                _ => vec![0],
            },
            coupon_percent: None,
        }
    }

    pub fn validate(&self, kind: &str) -> Result<(), Error> {
        let valid_len = if kind == "abandoned_cart" {
            1..=3
        } else {
            1..=1
        };
        if !valid_len.contains(&self.delays_hours.len())
            || self.delays_hours.iter().any(|h| *h < 0 || *h > 24 * 90)
            || self.delays_hours.windows(2).any(|w| w[0] >= w[1])
            || (kind == "abandoned_cart" && self.delays_hours[0] < 1)
            || self.coupon_percent.is_some_and(|p| !(1..=50).contains(&p))
            || (kind != "abandoned_cart" && self.coupon_percent.is_some())
        {
            return Err(Error::Validation {
                code: "invalid_flow_config",
                detail: "invalid delay or coupon configuration".into(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowDefinition)]
pub struct Definition {
    pub id: Uuid,
    pub kind: String,
    pub enabled: bool,
    pub config: FlowConfig,
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = FlowDefinitionChange)]
#[serde(deny_unknown_fields)]
pub struct DefinitionChange {
    pub enabled: bool,
    pub config: FlowConfig,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowRun)]
pub struct Run {
    pub id: Uuid,
    pub kind: String,
    pub source_kind: String,
    pub source_id: Uuid,
    pub status: String,
    pub next_step: i32,
    pub due_at: DateTime<Utc>,
    pub exit_reason: Option<String>,
    pub attempts: i16,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowStep)]
pub struct StepRecord {
    pub step_number: i32,
    pub status: String,
    pub message_id: Option<Uuid>,
    pub reason: Option<String>,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = FlowRunDetail)]
pub struct RunDetail {
    pub run: Run,
    pub steps: Vec<StepRecord>,
}

pub async fn ensure_defaults(tx: &mut TenantTx) -> Result<(), Error> {
    for kind in ["abandoned_cart", "watchdog", "review_invite"] {
        let config = serde_json::to_value(FlowConfig::for_kind(kind))
            .map_err(|e| Error::Internal(e.to_string()))?;
        sqlx::query("INSERT INTO flow_definitions (tenant_id, kind, config) VALUES ($1,$2,$3) ON CONFLICT (tenant_id,kind) DO NOTHING")
            .bind(tx.tenant_id()).bind(kind).bind(config).execute(&mut **tx).await?;
    }
    Ok(())
}

pub async fn definitions(tx: &mut TenantTx) -> Result<Vec<Definition>, Error> {
    ensure_defaults(tx).await?;
    let rows = sqlx::query("SELECT id,kind,enabled,config FROM flow_definitions ORDER BY kind")
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|r| {
            let kind: String = r.try_get("kind")?;
            let config = serde_json::from_value(r.try_get("config")?)
                .map_err(|e| Error::Internal(format!("flow config: {e}")))?;
            Ok(Definition {
                id: r.try_get("id")?,
                kind,
                enabled: r.try_get("enabled")?,
                config,
            })
        })
        .collect()
}

pub async fn configure(
    tx: &mut TenantTx,
    actor: &str,
    kind: &str,
    change: &DefinitionChange,
) -> Result<Definition, Error> {
    if !["abandoned_cart", "watchdog", "review_invite"].contains(&kind) {
        return Err(Error::NotFound);
    }
    change.config.validate(kind)?;
    ensure_defaults(tx).await?;
    let value = serde_json::to_value(&change.config).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query("UPDATE flow_definitions SET enabled=$2,config=$3,updated_at=now() WHERE kind=$1")
        .bind(kind)
        .bind(change.enabled)
        .bind(value)
        .execute(&mut **tx)
        .await?;
    crate::audit::record(
        tx,
        actor,
        "flows.configured",
        "flow",
        Some(kind),
        &json!({"enabled":change.enabled,"config":change.config}),
    )
    .await?;
    definitions(tx)
        .await?
        .into_iter()
        .find(|d| d.kind == kind)
        .ok_or(Error::NotFound)
}

pub async fn runs(tx: &mut TenantTx, limit: i64) -> Result<Vec<Run>, Error> {
    let rows = sqlx::query("SELECT r.id,d.kind,r.source_kind,r.source_id,r.status,r.next_step,r.due_at,r.exit_reason,r.attempts,r.last_error FROM flow_runs r JOIN flow_definitions d ON d.id=r.definition_id ORDER BY r.created_at DESC LIMIT $1")
        .bind(limit.clamp(1,100)).fetch_all(&mut **tx).await?;
    rows.into_iter()
        .map(|r| {
            Ok(Run {
                id: r.try_get("id")?,
                kind: r.try_get("kind")?,
                source_kind: r.try_get("source_kind")?,
                source_id: r.try_get("source_id")?,
                status: r.try_get("status")?,
                next_step: r.try_get("next_step")?,
                due_at: r.try_get("due_at")?,
                exit_reason: r.try_get("exit_reason")?,
                attempts: r.try_get("attempts")?,
                last_error: r.try_get("last_error")?,
            })
        })
        .collect()
}

pub async fn run_detail(tx: &mut TenantTx, id: Uuid) -> Result<RunDetail, Error> {
    let r = sqlx::query(
        "SELECT r.id,d.kind,r.source_kind,r.source_id,r.status,r.next_step,r.due_at,r.exit_reason,r.attempts,r.last_error
        FROM flow_runs r JOIN flow_definitions d ON d.id=r.definition_id WHERE r.id=$1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let run = Run {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        source_kind: r.try_get("source_kind")?,
        source_id: r.try_get("source_id")?,
        status: r.try_get("status")?,
        next_step: r.try_get("next_step")?,
        due_at: r.try_get("due_at")?,
        exit_reason: r.try_get("exit_reason")?,
        attempts: r.try_get("attempts")?,
        last_error: r.try_get("last_error")?,
    };
    let rows = sqlx::query("SELECT step_number,status,message_id,reason,executed_at FROM flow_steps WHERE run_id=$1 ORDER BY step_number")
        .bind(id).fetch_all(&mut **tx).await?;
    let steps = rows
        .into_iter()
        .map(|r| {
            Ok(StepRecord {
                step_number: r.try_get("step_number")?,
                status: r.try_get("status")?,
                message_id: r.try_get("message_id")?,
                reason: r.try_get("reason")?,
                executed_at: r.try_get("executed_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(RunDetail { run, steps })
}

pub async fn cancel_run(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<RunDetail, Error> {
    let affected = sqlx::query(
        "UPDATE flow_runs SET status='cancelled',exit_reason='manual',updated_at=now()
        WHERE id=$1 AND status='active'",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(Error::NotFound);
    }
    crate::audit::record(
        tx,
        actor,
        "flows.run_cancelled",
        "flow_run",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    run_detail(tx, id).await
}

pub fn dev_clock_allowed() -> bool {
    matches!(std::env::var("APP_ENV").as_deref(), Ok("dev" | "test"))
}

pub async fn effective_now(tx: &mut TenantTx) -> Result<DateTime<Utc>, Error> {
    if !dev_clock_allowed() {
        return Ok(Utc::now());
    }
    let offset: Option<i64> = sqlx::query_scalar("SELECT offset_seconds FROM flow_test_clocks")
        .fetch_optional(&mut **tx)
        .await?;
    Ok(Utc::now() + Duration::seconds(offset.unwrap_or(0)))
}

pub async fn advance_clock(tx: &mut TenantTx, hours: i64) -> Result<DateTime<Utc>, Error> {
    if !dev_clock_allowed() {
        return Err(Error::NotFound);
    }
    if !(1..=24 * 90).contains(&hours) {
        return Err(Error::Validation {
            code: "invalid_clock_advance",
            detail: "hours must be 1..2160".into(),
        });
    }
    sqlx::query("INSERT INTO flow_test_clocks(tenant_id,offset_seconds) VALUES($1,$2) ON CONFLICT(tenant_id) DO UPDATE SET offset_seconds=flow_test_clocks.offset_seconds+$2,updated_at=now()")
        .bind(tx.tenant_id()).bind(hours*3600).execute(&mut **tx).await?;
    effective_now(tx).await
}

/// Create runs from currently eligible carts and delivered shipments. This scan runs at most
/// once a minute; unique keys make retries and concurrent leaders harmless.
pub async fn enroll_due(tx: &mut TenantTx, now: DateTime<Utc>) -> Result<(), Error> {
    ensure_defaults(tx).await?;
    sqlx::query("INSERT INTO flow_runs(tenant_id,definition_id,source_kind,source_id,due_at,config_snapshot)
        SELECT c.tenant_id,d.id,'cart',c.id,c.last_activity_at + make_interval(hours => (d.config->'delays_hours'->>0)::int),d.config
        FROM carts c JOIN flow_definitions d ON d.tenant_id=c.tenant_id AND d.kind='abandoned_cart' AND d.enabled
        WHERE c.status='open' AND c.email IS NOT NULL AND c.last_activity_at <= $1 - interval '1 hour'
          AND EXISTS(SELECT 1 FROM cart_lines l WHERE l.cart_id=c.id)
          AND NOT EXISTS(SELECT 1 FROM flow_runs r WHERE r.definition_id=d.id AND r.source_id=c.id)
          AND (SELECT granted FROM consent_records cr WHERE cr.purpose='email_marketing'
               AND ((cr.subject_type='email' AND cr.subject_id=c.email)
                 OR (cr.subject_type='customer' AND cr.subject_id=c.customer_id::text))
               ORDER BY cr.at DESC,cr.id DESC LIMIT 1)=true
        ORDER BY c.last_activity_at LIMIT $2 ON CONFLICT DO NOTHING")
        .bind(now).bind(BATCH).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO flow_runs(tenant_id,definition_id,source_kind,source_id,due_at,config_snapshot)
        SELECT o.tenant_id,d.id,'order',o.id,min(s.delivered_at) + make_interval(hours => (d.config->'delays_hours'->>0)::int),d.config
        FROM orders o JOIN shipments s ON s.order_id=o.id AND s.delivered_at IS NOT NULL
        JOIN flow_definitions d ON d.tenant_id=o.tenant_id AND d.kind='review_invite' AND d.enabled
        WHERE o.status='delivered'
          AND NOT EXISTS(SELECT 1 FROM flow_runs r WHERE r.definition_id=d.id AND r.source_id=o.id)
          AND (SELECT granted FROM consent_records cr WHERE cr.purpose='review_invites'
               AND ((cr.subject_type='email' AND cr.subject_id=o.email)
                 OR (cr.subject_type='customer' AND cr.subject_id=o.customer_id::text))
               ORDER BY cr.at DESC,cr.id DESC LIMIT 1)=true
        GROUP BY o.tenant_id,d.id,o.id,d.config
        ORDER BY min(s.delivered_at) LIMIT $1
        ON CONFLICT DO NOTHING")
        .bind(BATCH).execute(&mut **tx).await?;
    Ok(())
}

/// One bounded batch. Mail messages and step markers commit in the same transaction.
pub async fn execute_due(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    now: DateTime<Utc>,
) -> Result<u64, Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM flow_runs WHERE status='active' AND due_at <= $1 ORDER BY due_at,id LIMIT $2 FOR UPDATE SKIP LOCKED")
        .bind(now).bind(BATCH).fetch_all(&mut **tx).await?;
    for id in &ids {
        sqlx::query("SAVEPOINT flow_step")
            .execute(&mut **tx)
            .await?;
        match execute_one(tx, urls, *id, now).await {
            Ok(()) => {
                sqlx::query("RELEASE SAVEPOINT flow_step")
                    .execute(&mut **tx)
                    .await?;
            }
            Err(error) => {
                sqlx::query("ROLLBACK TO SAVEPOINT flow_step")
                    .execute(&mut **tx)
                    .await?;
                tracing::warn!(run_id=%id,error=%error,"flow step failed; retry bounded to three attempts");
                sqlx::query(
                    "UPDATE flow_runs SET attempts=attempts+1,
                    status=CASE WHEN attempts>=2 THEN 'failed' ELSE 'active' END,
                    exit_reason=CASE WHEN attempts>=2 THEN 'retry_exhausted' ELSE NULL END,
                    last_error='step_error',due_at=$2+interval '5 minutes',updated_at=now()
                    WHERE id=$1 AND status='active'",
                )
                .bind(id)
                .bind(now)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "INSERT INTO flow_steps(tenant_id,run_id,step_number,status,reason)
                    SELECT tenant_id,id,next_step,'failed','retry_exhausted' FROM flow_runs
                    WHERE id=$1 AND status='failed'
                    ON CONFLICT(tenant_id,run_id,step_number) DO NOTHING",
                )
                .bind(id)
                .execute(&mut **tx)
                .await?;
                sqlx::query("RELEASE SAVEPOINT flow_step")
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    Ok(ids.len() as u64)
}

async fn execute_one(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let row = sqlx::query("SELECT r.source_kind,r.source_id,r.next_step,d.kind,d.enabled,
        CASE WHEN r.config_snapshot='{}'::jsonb THEN d.config ELSE r.config_snapshot END AS config
        FROM flow_runs r JOIN flow_definitions d ON d.id=r.definition_id WHERE r.id=$1 AND r.status='active'")
        .bind(id).fetch_one(&mut **tx).await?;
    let kind: String = row.try_get("kind")?;
    let source: Uuid = row.try_get("source_id")?;
    let step: i32 = row.try_get("next_step")?;
    let config: FlowConfig = serde_json::from_value(row.try_get("config")?)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !row.try_get::<bool, _>("enabled")? {
        return finish(tx, id, "cancelled", "disabled").await;
    }
    match kind.as_str() {
        "abandoned_cart" => execute_cart(tx, urls, id, source, step, &config, now).await,
        "review_invite" => execute_review(tx, urls, id, source, step, now).await,
        _ => finish(tx, id, "completed", "unsupported").await,
    }
}

async fn finish(tx: &mut TenantTx, id: Uuid, status: &str, reason: &str) -> Result<(), Error> {
    sqlx::query("UPDATE flow_runs SET status=$2,exit_reason=$3,updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(status)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn execute_cart(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    id: Uuid,
    cart_id: Uuid,
    step: i32,
    config: &FlowConfig,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let Some(c) = sqlx::query("SELECT market_id,email,locale,status,last_activity_at,customer_id FROM carts WHERE id=$1 FOR UPDATE")
        .bind(cart_id).fetch_optional(&mut **tx).await? else { return finish(tx,id,"cancelled","cart_missing").await; };
    let status: String = c.try_get("status")?;
    let email: Option<String> = c.try_get("email")?;
    let active: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM cart_lines WHERE cart_id=$1 LIMIT 1")
            .bind(cart_id)
            .fetch_optional(&mut **tx)
            .await?;
    let ordered: Option<i32> = sqlx::query_scalar("SELECT 1 FROM orders WHERE cart_id=$1 LIMIT 1")
        .bind(cart_id)
        .fetch_optional(&mut **tx)
        .await?;
    if status != "open" || active.is_none() || ordered.is_some() || email.is_none() {
        return finish(tx, id, "cancelled", "cart_closed").await;
    }
    let email = email.ok_or(Error::NotFound)?;
    let customer: Option<Uuid> = c.try_get("customer_id")?;
    let mut subjects = vec![Subject::Email(email.clone())];
    if let Some(customer) = customer {
        subjects.push(Subject::Customer(customer));
    }
    if consent::latest_any(tx, &subjects, ConsentPurpose::EmailMarketing).await? != Some(true) {
        return finish(tx, id, "cancelled", "consent_withdrawn").await;
    }
    let activity: DateTime<Utc> = c.try_get("last_activity_at")?;
    let Some(&delay) = usize::try_from(step)
        .ok()
        .and_then(|step| config.delays_hours.get(step))
    else {
        return finish(tx, id, "cancelled", "schedule_exhausted").await;
    };
    let eligible_at = activity + Duration::hours(delay);
    if eligible_at > now {
        sqlx::query("UPDATE flow_runs SET due_at=$2,updated_at=now() WHERE id=$1")
            .bind(id)
            .bind(eligible_at)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    }
    let market: Uuid = c.try_get("market_id")?;
    let locale: String = c.try_get("locale")?;
    let ctx = storefront::context(tx, urls, market, Some(&locale), now).await?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    let minted = capability::mint();
    sqlx::query("INSERT INTO flow_restore_tokens(tenant_id,token_hash,cart_id,expires_at) VALUES($1,$2,$3,$4)")
        .bind(tx.tenant_id()).bind(&minted.hash).bind(cart_id).bind(now+Duration::days(30)).execute(&mut **tx).await?;
    let coupon = if step as usize == config.delays_hours.len() - 1 {
        if let Some(percent) = config.coupon_percent {
            Some(issue_coupon(tx, id, percent, now).await?)
        } else {
            None
        }
    } else {
        None
    };
    let restore = format!(
        "{}/restore-cart?token={}",
        storefront::checkout_base(&ctx.base_url),
        minted.token
    );
    let optout = capability::mint();
    sqlx::query("INSERT INTO flow_unsubscribe_tokens(tenant_id,token_hash,email) VALUES($1,$2,$3)")
        .bind(tx.tenant_id())
        .bind(&optout.hash)
        .bind(&email)
        .execute(&mut **tx)
        .await?;
    let unsub_url = format!(
        "{}/flows/unsubscribe?token={}",
        storefront::checkout_base(&ctx.base_url),
        optout.token
    );
    let unsub_post = format!(
        "{}/_p/flows/unsubscribe?token={}",
        storefront::checkout_base(&ctx.base_url),
        optout.token
    );
    let message_id = notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::AbandonedCart,
            stream: Stream::Marketing,
            to: &email,
            locale: &locale,
            vars: json!({"url":restore,"coupon":coupon,"step":step+1,"unsubscribe_url":unsub_url}),
            idempotency_key: format!("flow:{id}:{step}"),
            sensitive: true,
        },
    )
    .await?;
    sqlx::query("UPDATE email_messages SET list_unsubscribe=$2 WHERE id=$1 AND status='pending'")
        .bind(message_id)
        .bind(&unsub_post)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO flow_steps(tenant_id,run_id,step_number,status,message_id) VALUES($1,$2,$3,'sent',$4) ON CONFLICT(tenant_id,run_id,step_number) DO NOTHING")
        .bind(tx.tenant_id()).bind(id).bind(step).bind(message_id).execute(&mut **tx).await?;
    let next = step as usize + 1;
    if next == config.delays_hours.len() {
        finish(tx, id, "completed", "all_steps").await?;
    } else {
        let due = activity + Duration::hours(config.delays_hours[next]);
        sqlx::query("UPDATE flow_runs SET next_step=$2,due_at=$3,updated_at=now() WHERE id=$1")
            .bind(id)
            .bind(step + 1)
            .bind(due)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn unsubscribe_cart(tx: &mut TenantTx, token: &str) -> Result<bool, Error> {
    if !capability::well_formed(token) {
        return Ok(false);
    }
    let email: Option<String> = sqlx::query_scalar("UPDATE flow_unsubscribe_tokens SET used_at=now() WHERE token_hash=$1 AND used_at IS NULL RETURNING email")
        .bind(capability::hash(token)).fetch_optional(&mut **tx).await?;
    let Some(email) = email else {
        return Ok(false);
    };
    consent::record_server(
        tx,
        &Subject::Email(email.clone()),
        ConsentPurpose::EmailMarketing,
        false,
        consent::TEXT_VERSION,
        "unsubscribe",
        None,
    )
    .await?;
    sqlx::query("UPDATE flow_runs SET status='cancelled',exit_reason='unsubscribed',updated_at=now()
        WHERE status='active' AND source_kind='cart' AND source_id IN (SELECT id FROM carts WHERE email=$1)")
        .bind(email).execute(&mut **tx).await?;
    Ok(true)
}

/// Mailer-side check (A20): consent can be withdrawn after a step queued its message but
/// before SMTP. This also catches an order placed while the message waited in the queue.
pub async fn delivery_refusal(
    tx: &mut TenantTx,
    message_id: Uuid,
) -> Result<Option<&'static str>, Error> {
    let row = sqlx::query("SELECT idempotency_key,to_email FROM email_messages WHERE id=$1")
        .bind(message_id)
        .fetch_one(&mut **tx)
        .await?;
    let key: String = row.try_get("idempotency_key")?;
    if let Some(watch_text) = key.strip_prefix("watch:alert:") {
        let mut parts = watch_text.split(':');
        let Ok(watch) = Uuid::parse_str(parts.next().unwrap_or_default()) else {
            return Ok(Some("invalid_watch"));
        };
        let generation = parts
            .next()
            .and_then(|n| n.parse::<i32>().ok())
            .unwrap_or(1);
        let status: Option<String> = sqlx::query_scalar(
            "SELECT w.status FROM flow_watches w JOIN flow_definitions d
                ON d.tenant_id=w.tenant_id AND d.kind='watchdog' AND d.enabled
                WHERE w.id=$1 AND w.generation=$2",
        )
        .bind(watch)
        .bind(generation)
        .fetch_optional(&mut **tx)
        .await?;
        return Ok(if status.as_deref() == Some("fired") {
            None
        } else {
            Some("watch_unsubscribed")
        });
    }
    let Some(run_text) = key.strip_prefix("flow:").and_then(|s| s.split(':').next()) else {
        return Ok(None);
    };
    let Ok(run_id) = Uuid::parse_str(run_text) else {
        return Ok(Some("invalid_flow"));
    };
    let email: String = row.try_get("to_email")?;
    let run = sqlx::query(
        "SELECT r.source_id,r.source_kind,r.status,d.enabled FROM flow_runs r
        JOIN flow_definitions d ON d.id=r.definition_id WHERE r.id=$1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(run) = run else {
        return Ok(Some("flow_missing"));
    };
    if !run.try_get::<bool, _>("enabled")? {
        return Ok(Some("flow_disabled"));
    }
    if matches!(run.try_get::<&str, _>("status")?, "cancelled" | "failed") {
        return Ok(Some("flow_cancelled"));
    }
    let source: Uuid = run.try_get("source_id")?;
    let kind: String = run.try_get("source_kind")?;
    let row = if kind == "cart" {
        sqlx::query("SELECT customer_id,status FROM carts WHERE id=$1")
            .bind(source)
            .fetch_optional(&mut **tx)
            .await?
    } else {
        sqlx::query("SELECT customer_id,status FROM orders WHERE id=$1")
            .bind(source)
            .fetch_optional(&mut **tx)
            .await?
    };
    let Some(row) = row else {
        return Ok(Some("source_missing"));
    };
    let mut subjects = vec![Subject::Email(email)];
    if let Some(customer) = row.try_get::<Option<Uuid>, _>("customer_id")? {
        subjects.push(Subject::Customer(customer));
    }
    let purpose = if kind == "cart" {
        ConsentPurpose::EmailMarketing
    } else {
        ConsentPurpose::ReviewInvites
    };
    if consent::latest_any(tx, &subjects, purpose).await? != Some(true) {
        return Ok(Some("consent_withdrawn"));
    }
    let status: String = row.try_get("status")?;
    if kind == "cart" {
        let ordered: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM orders WHERE cart_id=$1 LIMIT 1")
                .bind(source)
                .fetch_optional(&mut **tx)
                .await?;
        let has_lines: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM cart_lines WHERE cart_id=$1 LIMIT 1")
                .bind(source)
                .fetch_optional(&mut **tx)
                .await?;
        if status != "open" || ordered.is_some() || has_lines.is_none() {
            return Ok(Some("cart_closed"));
        }
    } else if kind == "order" && status != "delivered" {
        return Ok(Some("order_closed"));
    }
    Ok(None)
}

async fn issue_coupon(
    tx: &mut TenantTx,
    run: Uuid,
    percent: i32,
    now: DateTime<Utc>,
) -> Result<String, Error> {
    if let Some(code) = sqlx::query_scalar("SELECT coupon_code FROM flow_runs WHERE id=$1")
        .bind(run)
        .fetch_one(&mut **tx)
        .await?
    {
        return Ok(code);
    }
    for _ in 0..5 {
        let code = format!(
            "FLOW{}",
            hex::encode(rand::random::<[u8; 14]>()).to_uppercase()
        );
        let inserted: Option<String> = sqlx::query_scalar("INSERT INTO coupons(tenant_id,code,kind,value,usage_limit,starts_at,ends_at,published)
            VALUES($1,$2,'percent',$3,1,$4,$5,false) ON CONFLICT(tenant_id,code) DO NOTHING RETURNING code")
            .bind(tx.tenant_id()).bind(&code).bind(i64::from(percent)*100)
            .bind(now).bind(now+Duration::days(14)).fetch_optional(&mut **tx).await?;
        if let Some(code) = inserted {
            sqlx::query("UPDATE flow_runs SET coupon_code=$2 WHERE id=$1")
                .bind(run)
                .bind(&code)
                .execute(&mut **tx)
                .await?;
            return Ok(code);
        }
    }
    Err(Error::Internal(
        "coupon code collision limit reached".into(),
    ))
}

async fn execute_review(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    id: Uuid,
    order_id: Uuid,
    step: i32,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let Some(o) = sqlx::query(
        "SELECT market_id,email,locale,customer_id,status FROM orders WHERE id=$1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return finish(tx, id, "cancelled", "order_missing").await;
    };
    if o.try_get::<String, _>("status")? != "delivered" {
        return finish(tx, id, "cancelled", "order_not_delivered").await;
    }
    let email: String = o.try_get("email")?;
    let mut subjects = vec![Subject::Email(email.clone())];
    if let Some(customer) = o.try_get::<Option<Uuid>, _>("customer_id")? {
        subjects.push(Subject::Customer(customer));
    }
    if consent::latest_any(tx, &subjects, ConsentPurpose::ReviewInvites).await? != Some(true) {
        return finish(tx, id, "cancelled", "consent_withdrawn").await;
    }
    let market: Uuid = o.try_get("market_id")?;
    let locale: String = o.try_get("locale")?;
    let ctx = storefront::context(tx, urls, market, Some(&locale), now).await?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    let tokens = crate::reviews::issue_tokens(tx, order_id, now).await?;
    for token in tokens {
        let url = crate::reviews::review_url(&ctx, &token.token);
        notifications::enqueue(
            tx,
            &brand,
            Email {
                template: Template::ReviewInvite,
                stream: Stream::Transactional,
                to: &email,
                locale: &locale,
                vars: json!({"url":url,"product":token.product_name}),
                idempotency_key: format!("flow:{id}:{step}:{}", token.product_id),
                sensitive: true,
            },
        )
        .await?;
    }
    sqlx::query("INSERT INTO flow_steps(tenant_id,run_id,step_number,status,reason) VALUES($1,$2,$3,'sent','review_invites') ON CONFLICT(tenant_id,run_id,step_number) DO NOTHING")
        .bind(tx.tenant_id()).bind(id).bind(step).execute(&mut **tx).await?;
    finish(tx, id, "completed", "all_steps").await
}

/// A restore capability is consumed atomically; the returned checkout-cart capability is freshly
/// minted and only valid for the market in which the cart was created.
pub async fn restore_cart(
    tx: &mut TenantTx,
    market_id: Uuid,
    token: &str,
    now: DateTime<Utc>,
) -> Result<Option<String>, Error> {
    if !capability::well_formed(token) {
        return Ok(None);
    }
    let cart_id: Option<Uuid> = sqlx::query_scalar("UPDATE flow_restore_tokens SET used_at=$3 WHERE token_hash=$1 AND used_at IS NULL AND expires_at>$3 AND cart_id IN (SELECT id FROM carts WHERE market_id=$2 AND status='open') RETURNING cart_id")
        .bind(capability::hash(token)).bind(market_id).bind(now).fetch_optional(&mut **tx).await?;
    let Some(cart_id) = cart_id else {
        return Ok(None);
    };
    let fresh = capability::mint();
    sqlx::query("UPDATE carts SET checkout_token_hash=$2,last_activity_at=now(),updated_at=now() WHERE id=$1")
        .bind(cart_id).bind(&fresh.hash).execute(&mut **tx).await?;
    Ok(Some(fresh.token))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchInput {
    pub variant_id: Uuid,
    pub kind: String,
    pub target_minor: Option<i64>,
    pub email: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WatchStatus {
    pub status: &'static str,
}

pub async fn subscribe_watch(
    tx: &mut TenantTx,
    ctx: &storefront::Context,
    input: &WatchInput,
) -> Result<(), Error> {
    subscribe_watch_with_ip(tx, ctx, input, None).await
}

pub async fn subscribe_watch_with_ip(
    tx: &mut TenantTx,
    ctx: &storefront::Context,
    input: &WatchInput,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    if !["back_in_stock", "price_drop"].contains(&input.kind.as_str())
        || input
            .target_minor
            .is_some_and(|n| n <= 0 || n > 100_000_000_000)
        || (input.kind == "back_in_stock" && input.target_minor.is_some())
    {
        return Err(Error::Validation {
            code: "invalid_watch",
            detail: "invalid watch kind or target".into(),
        });
    }
    let email = crate::staff::normalize_email(&input.email)?;
    ensure_defaults(tx).await?;
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM flow_definitions WHERE kind='watchdog'")
            .fetch_one(&mut **tx)
            .await?;
    if !enabled {
        return Err(Error::NotFound);
    }
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM variants v JOIN products p ON p.id=v.product_id WHERE v.id=$1 AND p.status='active' LIMIT 1")
        .bind(input.variant_id).fetch_optional(&mut **tx).await?;
    if exists.is_none() {
        return Err(Error::NotFound);
    }
    let confirm = capability::mint();
    let unsub = capability::mint();
    sqlx::query("SAVEPOINT watch_subscription")
        .execute(&mut **tx)
        .await?;
    let watch_id: Option<Uuid> = sqlx::query_scalar("INSERT INTO flow_watches(tenant_id,market_id,variant_id,kind,target_minor,email,locale,confirm_hash,unsubscribe_hash,confirm_expires_at)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,now()+interval '48 hours')
        ON CONFLICT(tenant_id,market_id,variant_id,kind,email) DO UPDATE SET
          target_minor=excluded.target_minor,status='pending',confirm_hash=excluded.confirm_hash,
          unsubscribe_hash=excluded.unsubscribe_hash,confirm_expires_at=excluded.confirm_expires_at,
          generation=flow_watches.generation+1,updated_at=now() WHERE flow_watches.status='unsubscribed'
            OR (flow_watches.status='pending' AND flow_watches.updated_at < now()-interval '10 minutes')
        RETURNING id")
        .bind(tx.tenant_id()).bind(ctx.market.id).bind(input.variant_id).bind(&input.kind)
        .bind(input.target_minor).bind(&email).bind(&ctx.locale).bind(&confirm.hash).bind(&unsub.hash)
        .fetch_optional(&mut **tx).await?;
    if let Some(watch_id) = watch_id {
        let ip = ip_hash.map(hex::encode).unwrap_or_else(|| "missing".into());
        if !watch_mail_quota(tx, "recipient", &email, 2).await?
            || !watch_mail_quota(tx, "ip", &ip, 20).await?
        {
            sqlx::query("ROLLBACK TO SAVEPOINT watch_subscription")
                .execute(&mut **tx)
                .await?;
            sqlx::query("RELEASE SAVEPOINT watch_subscription")
                .execute(&mut **tx)
                .await?;
            return Ok(());
        }
        let brand = Brand::load(tx, ctx.base_url.clone()).await?;
        let url = format!(
            "{}/watch/confirm?token={}",
            storefront::checkout_base(&ctx.base_url),
            confirm.token
        );
        notifications::enqueue(
            tx,
            &brand,
            Email {
                template: Template::WatchConfirm,
                stream: Stream::Transactional,
                to: &email,
                locale: &ctx.locale,
                vars: json!({"url":url}),
                idempotency_key: format!(
                    "watch:confirm:{watch_id}:{}",
                    hex::encode(&confirm.hash[..8])
                ),
                sensitive: true,
            },
        )
        .await?;
    }
    sqlx::query("RELEASE SAVEPOINT watch_subscription")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn watch_mail_quota(
    tx: &mut TenantTx,
    scope: &str,
    identifier: &str,
    limit: i32,
) -> Result<bool, Error> {
    let granted: Option<i32> = sqlx::query_scalar("INSERT INTO flow_watch_mail_quotas(tenant_id,scope,identifier)
        VALUES($1,$2,$3) ON CONFLICT(tenant_id,scope,identifier) DO UPDATE SET
          window_started_at=CASE WHEN flow_watch_mail_quotas.window_started_at < now()-interval '1 day'
                                  THEN now() ELSE flow_watch_mail_quotas.window_started_at END,
          sent_count=CASE WHEN flow_watch_mail_quotas.window_started_at < now()-interval '1 day'
                          THEN 1 ELSE flow_watch_mail_quotas.sent_count+1 END
        WHERE flow_watch_mail_quotas.window_started_at < now()-interval '1 day'
           OR flow_watch_mail_quotas.sent_count < $4 RETURNING sent_count")
        .bind(tx.tenant_id()).bind(scope).bind(identifier).bind(limit)
        .fetch_optional(&mut **tx).await?;
    Ok(granted.is_some())
}

pub async fn confirm_watch(
    tx: &mut TenantTx,
    token: &str,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    if !capability::well_formed(token) {
        return Ok(false);
    }
    let updated = sqlx::query("UPDATE flow_watches SET status='confirmed',confirm_hash=NULL,confirm_expires_at=NULL,confirmed_at=$2,updated_at=now()
        WHERE confirm_hash=$1 AND status='pending' AND confirm_expires_at>$2")
        .bind(capability::hash(token)).bind(now).execute(&mut **tx).await?.rows_affected();
    Ok(updated == 1)
}

pub async fn unsubscribe_watch(tx: &mut TenantTx, token: &str) -> Result<bool, Error> {
    if !capability::well_formed(token) {
        return Ok(false);
    }
    let updated = sqlx::query("UPDATE flow_watches SET status='unsubscribed',confirm_hash=NULL,confirm_expires_at=NULL,updated_at=now()
        WHERE unsubscribe_hash=$1 AND status <> 'unsubscribed'")
        .bind(capability::hash(token)).execute(&mut **tx).await?.rows_affected();
    Ok(updated == 1)
}

/// Consume a committed inventory or price transition. The before/after values come from the
/// outbox, never from the subscriber. The status update and email enqueue are atomic.
pub async fn watch_event(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    kind: &str,
    payload: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<u64, Error> {
    if kind != "inventory.changed" && kind != "price.changed" {
        return Ok(0);
    }
    ensure_defaults(tx).await?;
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM flow_definitions WHERE kind='watchdog'")
            .fetch_one(&mut **tx)
            .await?;
    if !enabled {
        return Ok(0);
    }
    let Some(variant) = payload
        .get("variant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(0);
    };
    let (watch_kind, before, after) = if kind == "inventory.changed" {
        (
            "back_in_stock",
            payload
                .pointer("/before/available")
                .and_then(|v| v.as_i64()),
            payload.pointer("/after/available").and_then(|v| v.as_i64()),
        )
    } else {
        (
            "price_drop",
            payload.get("before_minor").and_then(|v| v.as_i64()),
            payload.get("after_minor").and_then(|v| v.as_i64()),
        )
    };
    let (Some(before), Some(after)) = (before, after) else {
        return Ok(0);
    };
    if !(before <= 0 && after > 0 && watch_kind == "back_in_stock"
        || after < before && watch_kind == "price_drop")
    {
        return Ok(0);
    }
    let event_list = payload
        .get("price_list_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    if kind == "price.changed" && event_list.is_none() {
        return Ok(0);
    }
    let cursor = payload
        .get("_watch_cursor")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let rows = sqlx::query(
        "SELECT w.id,w.market_id,w.email,w.locale,w.target_minor,w.generation
        FROM flow_watches w JOIN markets m ON m.id=w.market_id
        WHERE w.variant_id=$1 AND w.kind=$2 AND w.status='confirmed'
          AND ($3::uuid IS NULL OR w.id>$3)
          AND ($4::uuid IS NULL OR m.price_list_id=$4)
          AND (w.target_minor IS NULL OR w.target_minor>$5)
        ORDER BY w.id LIMIT $6 FOR UPDATE OF w",
    )
    .bind(variant)
    .bind(watch_kind)
    .bind(cursor)
    .bind(event_list)
    .bind(after)
    .bind(BATCH)
    .fetch_all(&mut **tx)
    .await?;
    let next_cursor = if rows.len() == BATCH as usize {
        rows.last()
            .map(|row| row.try_get::<Uuid, _>("id"))
            .transpose()?
    } else {
        None
    };
    let mut fired = 0;
    for row in rows {
        let target: Option<i64> = row.try_get("target_minor")?;
        if target.is_some_and(|target| after >= target) {
            continue;
        }
        let id: Uuid = row.try_get("id")?;
        let generation: i32 = row.try_get("generation")?;
        let market: Uuid = row.try_get("market_id")?;
        let locale: String = row.try_get("locale")?;
        let email: String = row.try_get("email")?;
        let ctx = storefront::context(tx, urls, market, Some(&locale), now).await?;
        if watch_kind == "back_in_stock" {
            if crate::inventory::get(tx, variant).await?.available <= 0 {
                continue;
            }
        } else {
            let Some(price_list) = ctx.market.price_list_id else {
                continue;
            };
            let prices =
                crate::pricing::shelf_prices(tx, price_list, ctx.market.currency, &[variant], now)
                    .await?;
            if !prices.get(&variant).is_some_and(|price| {
                price.amount_minor <= after
                    && target.is_none_or(|threshold| price.amount_minor < threshold)
            }) {
                continue;
            }
        }
        let brand = Brand::load(tx, ctx.base_url.clone()).await?;
        let slug: Option<String> = sqlx::query_scalar("SELECT t.slug FROM variants v JOIN product_translations t ON t.product_id=v.product_id AND t.locale=$2 WHERE v.id=$1")
            .bind(variant).bind(&locale).fetch_optional(&mut **tx).await?;
        let product_url = slug.map_or_else(
            || ctx.base_url.clone(),
            |slug| ctx.page_url(&format!("/p/{slug}")),
        );
        // Unsubscribe URL needs the raw capability, which is intentionally never stored. Mint
        // a fresh one for this mail and rotate the hash while holding the watch row lock.
        let unsub = capability::mint();
        sqlx::query("UPDATE flow_watches SET unsubscribe_hash=$2,status='fired',fired_at=$3,updated_at=now() WHERE id=$1 AND status='confirmed'")
            .bind(id).bind(&unsub.hash).bind(now).execute(&mut **tx).await?;
        let unsubscribe_url = format!(
            "{}/watch/unsubscribe?token={}",
            storefront::checkout_base(&ctx.base_url),
            unsub.token
        );
        let message_id = notifications::enqueue(
            tx,
            &brand,
            Email {
                template: Template::WatchAlert,
                stream: Stream::Transactional,
                to: &email,
                locale: &locale,
                vars: json!({"url":product_url,"unsubscribe_url":unsubscribe_url}),
                idempotency_key: format!("watch:alert:{id}:{generation}"),
                sensitive: true,
            },
        )
        .await?;
        let run: Uuid = sqlx::query_scalar("INSERT INTO flow_runs(tenant_id,definition_id,source_kind,source_id,source_generation,status,next_step,due_at,exit_reason,config_snapshot)
            SELECT $1,id,'watch',$2,$4,'completed',1,$3,'fired',config FROM flow_definitions WHERE kind='watchdog'
            ON CONFLICT(tenant_id,definition_id,source_id,source_generation) DO UPDATE SET status='completed',exit_reason='fired'
            RETURNING id")
            .bind(tx.tenant_id()).bind(id).bind(now).bind(generation).fetch_one(&mut **tx).await?;
        sqlx::query(
            "INSERT INTO flow_steps(tenant_id,run_id,step_number,status,message_id)
            VALUES($1,$2,0,'sent',$3) ON CONFLICT(tenant_id,run_id,step_number) DO NOTHING",
        )
        .bind(tx.tenant_id())
        .bind(run)
        .bind(message_id)
        .execute(&mut **tx)
        .await?;
        fired += 1;
    }
    if let Some(cursor) = next_cursor {
        let mut continuation = payload.clone();
        continuation["_watch_cursor"] = json!(cursor);
        let mut job = NewJob::new(EVENT_JOB, json!({"type": kind, "payload": continuation}));
        job.tenant_id = Some(tx.tenant_id());
        queue::enqueue(&mut **tx, &job).await?;
    }
    Ok(fired)
}

pub async fn on_event(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    kind: &str,
    payload: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if kind == "order.created" {
        if let Some(order_id) = payload
            .get("order_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            sqlx::query("UPDATE flow_runs SET status='cancelled',exit_reason='order_placed',updated_at=now()
                WHERE source_kind='cart' AND status='active' AND source_id IN (SELECT cart_id FROM orders WHERE id=$1)")
                .bind(order_id).execute(&mut **tx).await?;
        }
    } else if kind == "cart.changed" {
        if let Some(cart) = payload
            .get("cart_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            enroll_cart(tx, cart).await?;
        }
    } else if kind == "inventory.changed" || kind == "price.changed" {
        watch_event(tx, urls, kind, payload, now).await?;
    } else if kind == "order.delivered" {
        enroll_due(tx, now).await?;
    }
    Ok(())
}

async fn enroll_cart(tx: &mut TenantTx, cart: Uuid) -> Result<(), Error> {
    ensure_defaults(tx).await?;
    sqlx::query("INSERT INTO flow_runs(tenant_id,definition_id,source_kind,source_id,due_at,config_snapshot)
        SELECT c.tenant_id,d.id,'cart',c.id,c.last_activity_at + make_interval(hours => (d.config->'delays_hours'->>0)::int),d.config
        FROM carts c JOIN flow_definitions d ON d.tenant_id=c.tenant_id AND d.kind='abandoned_cart' AND d.enabled
        WHERE c.id=$1 AND c.status='open' AND c.email IS NOT NULL
          AND EXISTS(SELECT 1 FROM cart_lines l WHERE l.cart_id=c.id)
          AND (SELECT granted FROM consent_records cr WHERE cr.purpose='email_marketing'
               AND ((cr.subject_type='email' AND cr.subject_id=c.email)
                 OR (cr.subject_type='customer' AND cr.subject_id=c.customer_id::text))
               ORDER BY cr.at DESC,cr.id DESC LIMIT 1)=true
        ON CONFLICT(tenant_id,definition_id,source_id,source_generation) DO UPDATE SET
          due_at=excluded.due_at,updated_at=now() WHERE flow_runs.status='active' AND flow_runs.next_step=0")
        .bind(cart).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_rejects_unbounded_or_misordered_steps() {
        for delays in [
            vec![],
            vec![0],
            vec![24, 1],
            vec![1, 24, 72, 96],
            vec![1, 1],
        ] {
            assert!(
                FlowConfig {
                    delays_hours: delays,
                    coupon_percent: None
                }
                .validate("abandoned_cart")
                .is_err()
            );
        }
        assert!(
            FlowConfig::for_kind("abandoned_cart")
                .validate("abandoned_cart")
                .is_ok()
        );
    }
}
