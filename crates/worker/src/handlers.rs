//! Job handlers. Every handler is idempotent (spec §13).

use std::sync::Arc;
use std::time::Duration;

use commerce::media::{self, Processed};
use platform::queue::{self, Job};
use platform::storage::Storage;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::runner::{Ctx, Handlers, JobError};

/// Structured log line per outbox event: the default subscriber of every event type.
pub const EVENTS_LOG: &str = "events.log";
/// Hourly retention: finished jobs, dispatched events, expired idempotency keys (A12).
pub const MAINTENANCE_CLEANUP: &str = "maintenance.cleanup";

const JOB_RETENTION: Duration = Duration::from_secs(7 * 86_400);

/// Image encoding is CPU-heavy (each encode is single-threaded): one at a time per worker
/// process keeps the other job loops responsive. ponytail: fixed at 1; make it configurable
/// when a dedicated media worker gets more cores.
const MEDIA_CONCURRENCY: usize = 1;

pub fn all(storage: Storage) -> Handlers {
    let encode_slots = Arc::new(Semaphore::new(MEDIA_CONCURRENCY));
    let purge_storage = storage.clone();
    Handlers::default()
        .register(EVENTS_LOG, events_log)
        .register(MAINTENANCE_CLEANUP, maintenance_cleanup)
        .register(media::PROCESS_JOB, move |ctx, job| {
            media_process(ctx, job, storage.clone(), encode_slots.clone())
        })
        .register(media::PURGE_JOB, move |_ctx, job| {
            media_purge(job, purge_storage.clone())
        })
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

fn tenant_and_asset(job: &Job) -> Result<(Uuid, Uuid), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("media job without tenant".into()))?;
    let asset = job
        .payload
        .get("asset_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no asset_id".into()))?;
    Ok((tenant, asset))
}

async fn media_process(
    ctx: Ctx,
    job: Job,
    storage: Storage,
    slots: Arc<Semaphore>,
) -> Result<(), JobError> {
    let (tenant, asset) = tenant_and_asset(&job)?;
    let _slot = slots
        .acquire()
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    let outcome = media::process(&ctx.db, &storage, tenant, asset)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::info!(%asset, ?outcome, "media processed");
    if outcome == Processed::Failed {
        tracing::warn!(%asset, "image could not be decoded; asset marked failed");
    }
    Ok(())
}

async fn media_purge(job: Job, storage: Storage) -> Result<(), JobError> {
    let keys = |field: &str| -> Result<Vec<String>, JobError> {
        serde_json::from_value(job.payload.get(field).cloned().unwrap_or_default())
            .map_err(|e| JobError::Permanent(format!("{field}: {e}")))
    };
    media::purge(&storage, &keys("private")?, &keys("public")?)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))
}
