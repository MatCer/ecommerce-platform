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
/// cache purges here. Search jobs are versioned per product (`commerce::search::job_for_event`).
pub fn subscribers(event_type: &str) -> &'static [&'static str] {
    match event_type {
        commerce::staff::INVITED_EVENT => &[handlers::EVENTS_LOG, handlers::STAFF_INVITE_MAIL],
        commerce::customers::EMAIL_VERIFIED_EVENT => {
            &[handlers::EVENTS_LOG, handlers::LINK_GUEST_ORDERS]
        }
        commerce::orders::CREATED_EVENT => &[
            handlers::EVENTS_LOG,
            handlers::PURCHASE_JOB,
            handlers::FANOUT_JOB,
        ],
        // ponytail: a fan-out job per event even for tenants without subscriptions (it finds
        // none and finishes); filter here if event volume makes that noticeable.
        t if commerce::webhooks::is_event(t) => &[handlers::EVENTS_LOG, handlers::FANOUT_JOB],
        _ => &[handlers::EVENTS_LOG],
    }
}

/// Dispatches one batch; returns the number of events dispatched.
pub async fn dispatch_batch(db: &PgPool) -> Result<usize, sqlx::Error> {
    let mut tx = db.begin().await?;
    let events = queue::claim_outbox(&mut *tx, BATCH).await?;
    // Drawn after the claim: every claimed event's transaction committed before it.
    // It versions the search jobs (spec A27); one job per product and kind per batch.
    let version = commerce::search::next_version(&mut *tx).await?;
    let mut search_jobs = std::collections::HashSet::new();
    for event in &events {
        if let Some(job) = commerce::search::job_for_event(
            event.tenant_id,
            &event.event_type,
            &event.payload,
            version,
        ) && search_jobs.insert((job.tenant_id, job.kind, job.payload.to_string()))
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

/// Polls until `shutdown`. Polling every 500 ms keeps the dispatch lag under a second
/// (`outbox_lag_seconds` on `/metrics`); ponytail: add LISTEN/NOTIFY wake-ups if that
/// metric shows the lag matters.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_events_fan_out_and_orders_feed_analytics() {
        assert_eq!(
            subscribers("order.created"),
            [
                handlers::EVENTS_LOG,
                handlers::PURCHASE_JOB,
                handlers::FANOUT_JOB
            ]
        );
        for t in commerce::webhooks::EVENTS {
            assert!(subscribers(t).contains(&handlers::FANOUT_JOB), "{t}");
        }
        assert_eq!(subscribers("coupon.created"), [handlers::EVENTS_LOG]);
    }
}
