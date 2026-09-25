//! Outbox dispatcher (spec §13, A14): turns committed events into one job per subscriber.
//! Claiming the events, enqueueing the jobs and setting `dispatched_at` happen in one
//! transaction, so an event is dispatched completely or not at all.

use std::time::Duration;

use platform::queue::{self, NewJob};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::watch;

use crate::handlers;

const BATCH: i32 = 100;

/// The job kinds subscribed to an event type. Later WPs add webhooks, emails, analytics and
/// cache purges here. Search jobs are debounced per product (`commerce::search::job_for_event`).
pub fn subscribers(_event_type: &str) -> &'static [&'static str] {
    &[handlers::EVENTS_LOG]
}

/// Dispatches one batch; returns the number of events dispatched.
pub async fn dispatch_batch(db: &PgPool) -> Result<usize, sqlx::Error> {
    let mut tx = db.begin().await?;
    let events = queue::claim_outbox(&mut *tx, BATCH).await?;
    // After the claim: every event's source transaction committed before this instant, which
    // the debounced search jobs rely on.
    let now = chrono::Utc::now();
    for event in &events {
        if let Some(job) =
            commerce::search::job_for_event(event.tenant_id, &event.event_type, &event.payload, now)
        {
            queue::enqueue(&mut *tx, &job).await?;
        }
        for kind in subscribers(&event.event_type) {
            let mut job = NewJob::new(
                kind,
                json!({
                    "event_id": event.id,
                    "type": event.event_type,
                    "payload": event.payload,
                }),
            );
            job.tenant_id = event.tenant_id;
            job.idempotency_key = Some(format!("outbox:{}:{kind}", event.id));
            queue::enqueue(&mut *tx, &job).await?;
        }
    }
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    queue::mark_dispatched(&mut *tx, &ids).await?;
    tx.commit().await?;
    Ok(events.len())
}

/// Polls until `shutdown`. ponytail: polling only; add LISTEN/NOTIFY wake-ups if the poll
/// latency matters.
pub async fn run(db: PgPool, poll: Duration, mut shutdown: watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        match dispatch_batch(&db).await {
            Ok(n) if n > 0 => continue,
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "outbox dispatch failed"),
        }
        tokio::select! {
            _ = shutdown.changed() => {}
            () = tokio::time::sleep(poll) => {}
        }
    }
}
