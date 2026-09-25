//! Queue gauges for `/metrics` (spec §15), refreshed every 15 s from `queue.stats()`:
//! `jobs_queue_depth{kind,status}` (unfinished jobs), `jobs_lag_seconds{kind}` (age of the
//! oldest due job still waiting) and `outbox_lag_seconds`. Series that disappear drop to 0.

use std::collections::HashSet;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;

pub async fn refresh(db: PgPool, mut shutdown: watch::Receiver<bool>) {
    let mut depth_seen: HashSet<(String, String)> = HashSet::new();
    let mut lag_seen: HashSet<String> = HashSet::new();
    while !*shutdown.borrow() {
        match platform::queue::stats(&db).await {
            Ok(stats) => {
                let mut depth_now = HashSet::new();
                let mut lag_now = HashSet::new();
                for s in stats {
                    #[allow(clippy::cast_precision_loss)]
                    metrics::gauge!("jobs_queue_depth", "kind" => s.kind.clone(), "status" => s.status.clone())
                        .set(s.jobs as f64);
                    if s.status == "queued" {
                        metrics::gauge!("jobs_lag_seconds", "kind" => s.kind.clone())
                            .set(s.oldest_due_seconds.unwrap_or(0.0));
                        lag_now.insert(s.kind.clone());
                    }
                    depth_now.insert((s.kind, s.status));
                }
                for (kind, status) in depth_seen.difference(&depth_now) {
                    metrics::gauge!("jobs_queue_depth", "kind" => kind.clone(), "status" => status.clone())
                        .set(0.0);
                }
                for kind in lag_seen.difference(&lag_now) {
                    metrics::gauge!("jobs_lag_seconds", "kind" => kind.clone()).set(0.0);
                }
                depth_seen.extend(depth_now);
                lag_seen.extend(lag_now);
            }
            Err(e) => tracing::warn!(error = %e, "queue stats failed"),
        }
        match platform::queue::outbox_lag(&db).await {
            Ok(lag) => metrics::gauge!("outbox_lag_seconds").set(lag),
            Err(e) => tracing::warn!(error = %e, "outbox lag failed"),
        }
        tokio::select! {
            _ = shutdown.changed() => {}
            () = tokio::time::sleep(Duration::from_secs(15)) => {}
        }
    }
}
