//! Transactional outbox and leased job queue (spec §13, A8, A14). Thin wrappers over the
//! `queue.*` SECURITY DEFINER functions (migrations/..._queue.sql); the runtime role has no
//! direct access to the queue tables.
//!
//! Every function takes any Postgres executor, so it can join the caller's transaction:
//! `publish` inside the business transaction is what makes the outbox transactional.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgExecutor;
use uuid::Uuid;

/// Lease length for claimed jobs (A14). Workers heartbeat well within it.
pub const DEFAULT_LEASE: Duration = Duration::from_secs(60);

/// A job leased to this worker. `lease_token` fences its heartbeat/complete/fail calls.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub tenant_id: Option<Uuid>,
    pub queue: String,
    pub kind: String,
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub lease_token: Uuid,
}

#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub kind: &'a str,
    pub payload: Value,
    pub tenant_id: Option<Uuid>,
    pub queue: &'a str,
    /// `None` = now.
    pub run_at: Option<DateTime<Utc>>,
    pub max_attempts: i32,
    /// Enqueueing the same key twice returns the first job instead of a duplicate.
    pub idempotency_key: Option<String>,
}

impl<'a> NewJob<'a> {
    pub fn new(kind: &'a str, payload: Value) -> Self {
        Self {
            kind,
            payload,
            tenant_id: None,
            queue: "default",
            run_at: None,
            max_attempts: 10,
            idempotency_key: None,
        }
    }
}

/// An outbox event (`queue.outbox`).
#[derive(Debug, Clone)]
pub struct Event {
    pub id: i64,
    pub tenant_id: Option<Uuid>,
    pub event_type: String,
    pub payload: Value,
}

/// Records an event in the caller's transaction. The tenant is taken from the transaction's
/// tenant context (none for platform events).
pub async fn publish<'c>(
    db: impl PgExecutor<'c>,
    event_type: &str,
    payload: &Value,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT queue.publish($1, $2) AS "id!""#,
        event_type,
        payload
    )
    .fetch_one(db)
    .await
}

/// Adds a job and returns its id (the existing job's id for a repeated idempotency key).
pub async fn enqueue<'c>(db: impl PgExecutor<'c>, job: &NewJob<'_>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT queue.enqueue($1, $2, $3, $4, $5, $6, $7) AS "id!""#,
        job.kind,
        job.payload,
        job.tenant_id,
        job.queue,
        job.run_at,
        job.max_attempts,
        job.idempotency_key,
    )
    .fetch_one(db)
    .await
}

/// Leases up to `limit` ready jobs from `queues`, including jobs whose previous lease expired.
pub async fn claim<'c>(
    db: impl PgExecutor<'c>,
    owner: &str,
    queues: &[String],
    limit: i32,
    lease: Duration,
) -> Result<Vec<Job>, sqlx::Error> {
    sqlx::query_as!(
        Job,
        r#"SELECT id AS "id!", tenant_id, queue AS "queue!", kind AS "kind!",
                  payload AS "payload!", attempts AS "attempts!",
                  max_attempts AS "max_attempts!", lease_token AS "lease_token!"
           FROM queue.claim($1, $2, $3, $4)"#,
        owner,
        queues,
        limit,
        lease_secs(lease),
    )
    .fetch_all(db)
    .await
}

/// Extends the lease. `false` means the lease was lost (the job was reclaimed): stop working.
pub async fn heartbeat<'c>(
    db: impl PgExecutor<'c>,
    job: &Job,
    lease: Duration,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT queue.heartbeat($1, $2, $3) AS "ok!""#,
        job.id,
        job.lease_token,
        lease_secs(lease),
    )
    .fetch_one(db)
    .await
}

/// Marks the job done. `false` means a stale lease: another worker owns the job now.
pub async fn complete<'c>(db: impl PgExecutor<'c>, job: &Job) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT queue.complete($1, $2) AS "ok!""#,
        job.id,
        job.lease_token
    )
    .fetch_one(db)
    .await
}

/// Outcome of [`fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    /// Queued again for a later attempt.
    Retrying,
    /// No attempts left, or a permanent failure.
    Dead,
    /// The lease was stale; nothing was recorded.
    LeaseLost,
}

/// Records a failed attempt. `retry_in: None` marks a permanent failure (dead immediately).
pub async fn fail<'c>(
    db: impl PgExecutor<'c>,
    job: &Job,
    error: &str,
    retry_in: Option<Duration>,
) -> Result<Failed, sqlx::Error> {
    let retry_ms = retry_in.map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let status = sqlx::query_scalar!(
        r#"SELECT queue.fail($1, $2, $3, $4) AS status"#,
        job.id,
        job.lease_token,
        error,
        retry_ms,
    )
    .fetch_one(db)
    .await?;
    Ok(match status.as_deref() {
        Some("queued") => Failed::Retrying,
        Some(_) => Failed::Dead,
        None => Failed::LeaseLost,
    })
}

/// Locks up to `limit` undispatched events until the caller's transaction ends.
pub async fn claim_outbox<'c>(
    db: impl PgExecutor<'c>,
    limit: i32,
) -> Result<Vec<Event>, sqlx::Error> {
    sqlx::query_as!(
        Event,
        r#"SELECT id AS "id!", tenant_id, type AS "event_type!", payload AS "payload!"
           FROM queue.claim_outbox($1)"#,
        limit
    )
    .fetch_all(db)
    .await
}

pub async fn mark_dispatched<'c>(db: impl PgExecutor<'c>, ids: &[i64]) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT queue.mark_dispatched($1)", ids)
        .fetch_one(db)
        .await
        .map(|_| ())
}

/// Deletes finished jobs and dispatched events older than `older_than`; returns the count.
pub async fn purge<'c>(db: impl PgExecutor<'c>, older_than: Duration) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT queue.purge(make_interval(secs => $1)) AS "n!""#,
        older_than.as_secs_f64()
    )
    .fetch_one(db)
    .await
}

fn lease_secs(lease: Duration) -> i32 {
    i32::try_from(lease.as_secs().max(1)).unwrap_or(i32::MAX)
}

/// Retry delay after `attempt` failed attempts: exponential from `base`, capped at `cap`,
/// with "equal jitter" (half fixed, half random) so retries of a burst spread out.
pub fn backoff(attempt: i32, base: Duration, cap: Duration) -> Duration {
    let exp = u32::try_from(attempt.saturating_sub(1).clamp(0, 30)).unwrap_or(30);
    let ceiling = base.saturating_mul(2u32.saturating_pow(exp)).min(cap);
    let half = ceiling / 2;
    half + half.mul_f64(rand::random::<f64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_within_jitter_bounds() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(3600);
        for attempt in 1..=8 {
            let ceiling = base * 2u32.pow(u32::try_from(attempt - 1).unwrap());
            for _ in 0..50 {
                let d = backoff(attempt, base, cap);
                assert!(d >= ceiling / 2 && d <= ceiling, "attempt {attempt}: {d:?}");
            }
        }
    }

    #[test]
    fn backoff_is_capped() {
        let d = backoff(40, Duration::from_secs(1), Duration::from_secs(60));
        assert!(d <= Duration::from_secs(60) && d >= Duration::from_secs(30));
    }
}
