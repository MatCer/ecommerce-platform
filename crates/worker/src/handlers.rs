//! Job handlers. Every handler is idempotent (spec §13).

use std::time::Duration;

use platform::queue::{self, Job};

use crate::runner::{Ctx, Handlers, JobError};

/// Structured log line per outbox event: the default subscriber of every event type.
pub const EVENTS_LOG: &str = "events.log";
/// Hourly retention: finished jobs, dispatched events, expired idempotency keys (A12).
pub const MAINTENANCE_CLEANUP: &str = "maintenance.cleanup";

const JOB_RETENTION: Duration = Duration::from_secs(7 * 86_400);

pub fn all() -> Handlers {
    Handlers::default()
        .register(EVENTS_LOG, events_log)
        .register(MAINTENANCE_CLEANUP, maintenance_cleanup)
}

async fn events_log(_ctx: Ctx, job: Job) -> Result<(), JobError> {
    let event_type = job
        .payload
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| JobError::Permanent("payload has no event type".into()))?;
    // Ids only: payloads may carry personal data.
    tracing::info!(
        event_type,
        event_id = job
            .payload
            .get("event_id")
            .and_then(serde_json::Value::as_i64),
        tenant_id = job.tenant_id.map(tracing::field::display),
        "event"
    );
    Ok(())
}

async fn maintenance_cleanup(ctx: Ctx, _job: Job) -> Result<(), JobError> {
    let queue_rows = queue::purge(&ctx.db, JOB_RETENTION).await?;
    let keys = sqlx::query_scalar!(r#"SELECT platform.purge_idempotency_keys() AS "n!""#)
        .fetch_one(&ctx.db)
        .await?;
    tracing::info!(queue_rows, idempotency_keys = keys, "cleanup done");
    Ok(())
}
