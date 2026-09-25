//! Outbound webhooks (spec §8.5, A21).
//!
//! A subscription names a URL and the events it wants. Every matching outbox event becomes
//! one delivery per subscription ([`fanout`]); a delivery is POSTed as JSON through the
//! SSRF-safe client with
//! `X-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`
//! (the key is the whole `whsec_…` secret string), `X-Webhook-Id: <delivery id>` (receivers
//! dedupe on it) and `X-Webhook-Event: <type>`. Any 2xx is success. Failures retry with
//! exponential backoff (30 s doubling, capped at 6 h) inside a 24 h window, the last attempt at
//! the window's end, then the delivery is `dead`; staff can redeliver, which opens a new
//! window. Secrets are encrypted at rest and shown only when created or rotated.

use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::crypto::SecretBox;
use platform::db::{TenantTx, tenant_tx};
use platform::http::SafeClient;
use platform::queue::{self, NewJob};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;

/// Event types a subscription can ask for (§8.5).
pub const EVENTS: [&str; 10] = [
    "order.created",
    "order.paid",
    "order.cancelled",
    "order.shipped",
    "order.refunded",
    "product.created",
    "product.updated",
    "product.deleted",
    "inventory.changed",
    "customer.created",
];
/// One outbox event → deliveries for the matching subscriptions.
pub const FANOUT_JOB: &str = "webhooks.fanout";
/// One delivery attempt.
pub const DELIVER_JOB: &str = "webhooks.deliver";
/// Retries stop this long after the first attempt (or a redelivery).
pub const RETRY_WINDOW: Duration = Duration::from_secs(24 * 3600);
const MAX_SUBSCRIPTIONS: i64 = 20;
const PAGE_MAX: i64 = 100;
/// A21: 10 s per attempt; the answer is read (at most 1 MB) and dropped.
const RESPONSE_LIMITS: platform::http::Limits = platform::http::Limits {
    max_bytes: 1024 * 1024,
    timeout: Duration::from_secs(10),
};

/// Delivery dependencies: the secret box and the SSRF-safe client (A21).
#[derive(Clone)]
pub struct Webhooks {
    pub secrets: SecretBox,
    pub http: SafeClient,
    /// Production refuses plain-http endpoints.
    pub require_https: bool,
}

pub fn is_event(t: &str) -> bool {
    EVENTS.contains(&t)
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Subscription {
    pub id: Uuid,
    pub url: String,
    pub events: Vec<String>,
    pub description: String,
    pub active: bool,
    /// The secret's last characters, to tell secrets apart.
    pub secret_hint: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SubscriptionList {
    pub items: Vec<Subscription>,
    /// Event types a subscription can ask for.
    pub event_types: Vec<String>,
}

/// Returned by create and rotate only: the signing secret is never shown again.
#[derive(Debug, Serialize, ToSchema)]
pub struct SubscriptionWithSecret {
    pub subscription: Subscription,
    pub secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSubscription {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionUpdate {
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub description: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Delivery {
    pub id: Uuid,
    pub subscription_id: Uuid,
    /// Outbox event id (the payload's `id` is `evt_<event_id>`).
    pub event_id: i64,
    pub event_type: String,
    /// `pending`, `retrying`, `succeeded` or `dead`.
    pub status: String,
    pub attempts: i32,
    pub response_code: Option<i32>,
    pub last_error: Option<String>,
    pub next_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeliveryPage {
    pub items: Vec<Delivery>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

fn invalid(code: &'static str, detail: impl Into<String>) -> Error {
    Error::Validation {
        code,
        detail: detail.into(),
    }
}

fn check_url(url: &str, require_https: bool) -> Result<String, Error> {
    let url = url.trim();
    if !(10..=2048).contains(&url.len()) {
        return Err(invalid("invalid_url", "the URL must be 10-2048 characters"));
    }
    let parsed = reqwest::Url::parse(url)
        .ok()
        .filter(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
                && u.host_str().is_some_and(|h| !h.is_empty())
        })
        .ok_or_else(|| {
            invalid(
                "invalid_url",
                "an http(s) URL without credentials is required",
            )
        })?;
    if require_https && parsed.scheme() != "https" {
        return Err(invalid("invalid_url", "webhook URLs must use https"));
    }
    // Literal private addresses are refused right away; host names are checked on every
    // delivery (their addresses can change).
    let host = parsed.host_str().unwrap_or_default();
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        && !platform::http::is_public(ip)
    {
        return Err(invalid(
            "invalid_url",
            "the URL points to a private address",
        ));
    }
    Ok(url.to_owned())
}

fn check_events(events: &[String]) -> Result<Vec<String>, Error> {
    let mut out: Vec<String> = Vec::new();
    for e in events {
        if !is_event(e) {
            return Err(invalid(
                "invalid_events",
                format!("unknown event type {e:?}"),
            ));
        }
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    if out.is_empty() {
        return Err(invalid("invalid_events", "choose at least one event type"));
    }
    Ok(out)
}

fn check_description(d: &str) -> Result<String, Error> {
    let d = d.trim();
    if d.chars().count() > 200 {
        return Err(invalid("invalid_description", "at most 200 characters"));
    }
    Ok(d.to_owned())
}

fn aad(id: Uuid) -> Vec<u8> {
    format!("webhook:{id}").into_bytes()
}

fn new_secret() -> String {
    format!("whsec_{}", hex::encode(rand::random::<[u8; 32]>()))
}

fn hint(secret: &str) -> String {
    secret[secret.len().saturating_sub(4)..].to_owned()
}

async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Subscription, Error> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, url, events, description, active, secret_hint, created_at, updated_at
         FROM webhook_subscriptions WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

pub async fn list(tx: &mut TenantTx) -> Result<SubscriptionList, Error> {
    let items = sqlx::query_as!(
        Subscription,
        "SELECT id, url, events, description, active, secret_hint, created_at, updated_at
         FROM webhook_subscriptions ORDER BY created_at, id"
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(SubscriptionList {
        items,
        event_types: EVENTS.iter().map(|e| (*e).to_owned()).collect(),
    })
}

pub async fn create(
    tx: &mut TenantTx,
    hooks: &Webhooks,
    actor: &str,
    input: &NewSubscription,
) -> Result<SubscriptionWithSecret, Error> {
    let url = check_url(&input.url, hooks.require_https)?;
    let events = check_events(&input.events)?;
    let description = check_description(&input.description)?;
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM webhook_subscriptions"#)
        .fetch_one(&mut **tx)
        .await?;
    if count >= MAX_SUBSCRIPTIONS {
        return Err(Error::Conflict {
            code: "too_many_subscriptions",
            detail: format!("at most {MAX_SUBSCRIPTIONS} webhook subscriptions"),
        });
    }
    let id = crate::id::new_id();
    let secret = new_secret();
    sqlx::query!(
        "INSERT INTO webhook_subscriptions
             (id, tenant_id, url, events, description, secret_ciphertext, secret_hint, active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        tx.tenant_id(),
        url,
        &events,
        description,
        hooks.secrets.seal(secret.as_bytes(), &aad(id)),
        hint(&secret),
        input.active
    )
    .execute(&mut **tx)
    .await?;
    let subscription = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "webhook.created",
        "webhook_subscription",
        Some(&id.to_string()),
        &json!({ "after": subscription }),
    )
    .await?;
    Ok(SubscriptionWithSecret {
        subscription,
        secret,
    })
}

pub async fn update(
    tx: &mut TenantTx,
    hooks: &Webhooks,
    actor: &str,
    id: Uuid,
    input: &SubscriptionUpdate,
) -> Result<Subscription, Error> {
    let before = get(tx, id).await?;
    let url = match &input.url {
        Some(u) => check_url(u, hooks.require_https)?,
        None => before.url.clone(),
    };
    let events = match &input.events {
        Some(e) => check_events(e)?,
        None => before.events.clone(),
    };
    let description = match &input.description {
        Some(d) => check_description(d)?,
        None => before.description.clone(),
    };
    sqlx::query!(
        "UPDATE webhook_subscriptions
         SET url = $2, events = $3, description = $4, active = $5, updated_at = now()
         WHERE id = $1",
        id,
        url,
        &events,
        description,
        input.active.unwrap_or(before.active)
    )
    .execute(&mut **tx)
    .await?;
    let after = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "webhook.updated",
        "webhook_subscription",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM webhook_subscriptions WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "webhook.deleted",
        "webhook_subscription",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    Ok(())
}

/// A new secret; the old one stops working at once.
pub async fn rotate_secret(
    tx: &mut TenantTx,
    hooks: &Webhooks,
    actor: &str,
    id: Uuid,
) -> Result<SubscriptionWithSecret, Error> {
    get(tx, id).await?;
    let secret = new_secret();
    sqlx::query!(
        "UPDATE webhook_subscriptions
         SET secret_ciphertext = $2, secret_hint = $3, updated_at = now() WHERE id = $1",
        id,
        hooks.secrets.seal(secret.as_bytes(), &aad(id)),
        hint(&secret)
    )
    .execute(&mut **tx)
    .await?;
    let subscription = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "webhook.secret_rotated",
        "webhook_subscription",
        Some(&id.to_string()),
        &json!({ "secret_hint": subscription.secret_hint }),
    )
    .await?;
    Ok(SubscriptionWithSecret {
        subscription,
        secret,
    })
}

/// Deliveries newest first, optionally of one subscription.
pub async fn deliveries(
    tx: &mut TenantTx,
    subscription: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<DeliveryPage, Error> {
    let limit = limit.clamp(1, PAGE_MAX);
    let mut items = sqlx::query_as!(
        Delivery,
        "SELECT id, subscription_id, event_id, event_type, status, attempts, response_code,
                last_error, next_at, delivered_at, created_at, payload
         FROM webhook_deliveries
         WHERE ($1::uuid IS NULL OR subscription_id = $1) AND ($2::uuid IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3",
        subscription,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let more = items.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = if more {
        items.last().map(|d| d.id)
    } else {
        None
    };
    Ok(DeliveryPage { items, next_cursor })
}

fn deliver_job(
    tenant: Uuid,
    delivery: Uuid,
    attempt: i32,
    run_at: Option<DateTime<Utc>>,
) -> NewJob<'static> {
    let mut job = NewJob::new(
        DELIVER_JOB,
        json!({ "delivery_id": delivery, "attempt": attempt }),
    );
    job.tenant_id = Some(tenant);
    job.run_at = run_at;
    job.max_attempts = 5;
    job.idempotency_key = Some(format!("webhook:{delivery}:{attempt}"));
    job
}

/// Sends a delivery again now, with a fresh 24 h retry window.
pub async fn redeliver(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<Delivery, Error> {
    let row = sqlx::query!(
        "UPDATE webhook_deliveries
         SET status = CASE WHEN attempts = 0 THEN 'pending' ELSE 'retrying' END,
             next_at = now(), window_started_at = now(), updated_at = now()
         WHERE id = $1 AND status IN ('succeeded', 'dead')
         RETURNING attempts",
        id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return match sqlx::query_scalar!("SELECT id FROM webhook_deliveries WHERE id = $1", id)
            .fetch_optional(&mut **tx)
            .await?
        {
            None => Err(Error::NotFound),
            Some(_) => Err(Error::Conflict {
                code: "delivery_in_progress",
                detail: "the delivery is still being retried".into(),
            }),
        };
    };
    let job = deliver_job(tx.tenant_id(), id, row.attempts + 1, None);
    queue::enqueue(&mut **tx, &job).await?;
    audit::record(
        tx,
        actor,
        "webhook.redelivered",
        "webhook_delivery",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    let page = sqlx::query_as!(
        Delivery,
        "SELECT id, subscription_id, event_id, event_type, status, attempts, response_code,
                last_error, next_at, delivered_at, created_at, payload
         FROM webhook_deliveries WHERE id = $1",
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(page)
}

/// Worker step for [`FANOUT_JOB`]: a delivery (+ its first job) per active subscription that
/// wants the event. Idempotent: one delivery per subscription and event.
pub async fn fanout(
    db: &PgPool,
    tenant: Uuid,
    event_id: i64,
    event_type: &str,
    data: &Value,
) -> Result<usize, Error> {
    if !is_event(event_type) {
        return Ok(0);
    }
    let mut tx = tenant_tx(db, tenant).await?;
    let body = json!({
        "id": format!("evt_{event_id}"),
        "type": event_type,
        "created_at": Utc::now(),
        "data": data,
    });
    let created = sqlx::query_scalar!(
        "INSERT INTO webhook_deliveries (tenant_id, subscription_id, event_id, event_type, payload,
                                         next_at)
         SELECT tenant_id, id, $1, $2, $3, now() FROM webhook_subscriptions
         WHERE active AND $2 = ANY (events)
         ON CONFLICT (tenant_id, subscription_id, event_id) DO NOTHING
         RETURNING id",
        event_id,
        event_type,
        body
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in &created {
        queue::enqueue(&mut *tx, &deliver_job(tenant, *id, 1, None)).await?;
    }
    tx.commit().await?;
    Ok(created.len())
}

/// `X-Signature` for `body` sent at `ts` (Unix seconds).
pub fn signature(secret: &str, ts: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
    mac.update(ts.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// Delay before attempt `attempt + 1`: 30 s doubling, capped at 6 h.
pub fn retry_delay(attempt: i32) -> Duration {
    let exp = u32::try_from(attempt.saturating_sub(1).clamp(0, 20)).unwrap_or(20);
    Duration::from_secs(30)
        .saturating_mul(2u32.saturating_pow(exp))
        .min(Duration::from_secs(6 * 3600))
}

/// What happened to one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Succeeded,
    /// Scheduled again at the given time.
    Retrying(DateTime<Utc>),
    Dead,
    /// The job is stale (another attempt was recorded, or the delivery finished).
    Skipped,
}

/// Next state after a failed attempt: retry inside the window (the last one at its end),
/// otherwise dead.
pub fn after_failure(
    attempt: i32,
    window_started: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let end = window_started + chrono::Duration::from_std(RETRY_WINDOW).unwrap_or_default();
    if now >= end {
        return None;
    }
    let delay = chrono::Duration::from_std(retry_delay(attempt)).unwrap_or_default();
    Some((now + delay).min(end))
}

/// Worker step for [`DELIVER_JOB`]: one HTTP attempt, recorded with a compare-and-set on
/// `attempts` so a duplicate job cannot record twice. A crash between sending and recording
/// re-sends the same attempt (at-least-once; receivers dedupe on `X-Webhook-Id`).
pub async fn deliver(
    db: &PgPool,
    hooks: &Webhooks,
    tenant: Uuid,
    id: Uuid,
    attempt: i32,
) -> Result<Attempt, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let row = sqlx::query!(
        "SELECT d.status, d.attempts, d.payload, d.event_type, d.window_started_at,
                s.id AS subscription_id, s.url, s.secret_ciphertext, s.active
         FROM webhook_deliveries d
         JOIN webhook_subscriptions s ON s.tenant_id = d.tenant_id AND s.id = d.subscription_id
         WHERE d.id = $1",
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(Attempt::Skipped); // subscription deleted
    };
    if !matches!(row.status.as_str(), "pending" | "retrying") || row.attempts + 1 != attempt {
        return Ok(Attempt::Skipped);
    }

    let outcome = if row.active {
        let secret = hooks
            .secrets
            .open(&row.secret_ciphertext, &aad(row.subscription_id))
            .map_err(|e| Error::Internal(e.to_string()))?;
        let secret = String::from_utf8(secret).map_err(|e| Error::Internal(e.to_string()))?;
        let body = serde_json::to_vec(&row.payload).map_err(|e| Error::Internal(e.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("commerce-platform-webhooks/1"),
        );
        let sig = signature(&secret, Utc::now().timestamp(), &body);
        for (k, v) in [
            ("x-signature", sig),
            ("x-webhook-id", id.to_string()),
            ("x-webhook-event", row.event_type.clone()),
        ] {
            headers.insert(
                k,
                HeaderValue::from_str(&v).map_err(|e| Error::Internal(e.to_string()))?,
            );
        }
        match hooks
            .http
            .post(&row.url, headers, body, RESPONSE_LIMITS)
            .await
        {
            Ok(code) if (200..300).contains(&code) => Ok(i32::from(code)),
            Ok(code) => Err((Some(i32::from(code)), format!("HTTP {code}"))),
            Err(e) => Err((None, e.to_string())),
        }
    } else {
        Err((None, "subscription is disabled".to_owned()))
    };

    let now = Utc::now();
    let mut tx = tenant_tx(db, tenant).await?;
    let result = match outcome {
        Ok(code) => sqlx::query!(
            "UPDATE webhook_deliveries
                 SET status = 'succeeded', attempts = $2, response_code = $3, last_error = NULL,
                     next_at = NULL, delivered_at = now(), updated_at = now()
                 WHERE id = $1 AND attempts = $2 - 1 AND status IN ('pending', 'retrying')",
            id,
            attempt,
            code
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
        .eq(&1)
        .then_some(Attempt::Succeeded),
        Err((code, error)) => {
            let next = after_failure(attempt, row.window_started_at, now);
            let status = if next.is_some() { "retrying" } else { "dead" };
            let error: String = error.chars().take(500).collect();
            let recorded = sqlx::query!(
                "UPDATE webhook_deliveries
                 SET status = $3, attempts = $2, response_code = $4, last_error = $5,
                     next_at = $6, updated_at = now()
                 WHERE id = $1 AND attempts = $2 - 1 AND status IN ('pending', 'retrying')",
                id,
                attempt,
                status,
                code,
                error,
                next
            )
            .execute(&mut *tx)
            .await?
            .rows_affected()
                == 1;
            if recorded && let Some(at) = next {
                queue::enqueue(&mut *tx, &deliver_job(tenant, id, attempt + 1, Some(at))).await?;
            }
            recorded.then_some(match next {
                Some(at) => Attempt::Retrying(at),
                None => Attempt::Dead,
            })
        }
    };
    tx.commit().await?;
    Ok(result.unwrap_or(Attempt::Skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_a_reference_vector() {
        // echo -n '1700000000.{"a":1}' | openssl dgst -sha256 -hmac 'whsec_test'
        assert_eq!(
            signature("whsec_test", 1_700_000_000, br#"{"a":1}"#),
            "t=1700000000,v1=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789"
        );
    }

    #[test]
    fn retries_back_off_and_stop_after_the_window() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(11), Duration::from_secs(6 * 3600));
        let start = Utc::now();
        let first = after_failure(1, start, start).unwrap();
        assert_eq!(first - start, chrono::Duration::seconds(30));
        let late = start + chrono::Duration::hours(23);
        assert_eq!(
            after_failure(12, start, late).unwrap(),
            start + chrono::Duration::hours(24),
            "the last retry lands at the end of the window"
        );
        assert_eq!(
            after_failure(13, start, start + chrono::Duration::hours(24)),
            None
        );
    }

    #[test]
    fn validates_urls_and_events() {
        assert!(check_url("https://hooks.example.com/x", true).is_ok());
        assert!(check_url("http://hooks.example.com/x", true).is_err());
        assert!(check_url("http://hooks.example.com/x", false).is_ok());
        assert!(check_url("https://10.0.0.1/x", false).is_err());
        assert!(check_url("https://[::1]/x", false).is_err());
        assert!(check_url("https://u:p@example.com/x", false).is_err());
        assert_eq!(
            check_events(&["order.paid".into(), "order.paid".into()]).unwrap(),
            ["order.paid"]
        );
        assert!(check_events(&[]).is_err());
        assert!(check_events(&["order.exception".into()]).is_err());
    }
}
