//! Cron scheduler (spec §13). One leader per cluster: the worker that holds a session-level
//! `pg_try_advisory_lock` on a dedicated connection enqueues due jobs. If that connection
//! dies the lock is released and another worker takes over. Each run is enqueued with the key
//! `cron:<name>:<slot>`, so even two overlapping leaders cannot run a slot twice.

use std::time::Duration;

use chrono::{DateTime, Utc};
use platform::queue::{self, NewJob};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;

use crate::handlers;

/// Advisory lock key for the cron leader ("cron" in ASCII).
const LEADER_LOCK: i64 = 0x6372_6f6e;

pub struct Schedule {
    pub name: &'static str,
    pub kind: &'static str,
    /// Runs once per `every`, aligned to the Unix epoch (hourly = on the hour, UTC).
    pub every: Duration,
}

pub const SCHEDULES: &[Schedule] = &[
    Schedule {
        name: "maintenance.cleanup",
        kind: handlers::MAINTENANCE_CLEANUP,
        every: Duration::from_secs(3600),
    },
    Schedule {
        name: "feeds.export_all",
        kind: commerce::feeds::export::ALL_JOB,
        every: Duration::from_secs(3600),
    },
    // ponytail: one global scan per minute; an order expires up to ~1.5 min late (cron tick
    // 30 s + slot). Schedule per order (`run_at`) if payment windows need to be exact.
    Schedule {
        name: "payments.expire",
        kind: handlers::PAYMENTS_EXPIRE,
        every: Duration::from_secs(60),
    },
    Schedule {
        name: "payments.remind",
        kind: handlers::PAYMENTS_REMIND,
        every: Duration::from_secs(3600),
    },
    // Fio allows one request per token every 30 s; ten minutes keeps well clear of it.
    Schedule {
        name: "payments.fio_poll",
        kind: handlers::PAYMENTS_FIO_POLL,
        every: Duration::from_secs(600),
    },
    Schedule {
        name: "analytics.rollup",
        kind: handlers::ROLLUP_JOB,
        every: Duration::from_secs(3600),
    },
    Schedule {
        name: "recommendations.rollup",
        kind: handlers::RECOMMENDATIONS_ROLLUP,
        every: Duration::from_secs(3600),
    },
    Schedule {
        name: "analytics.partitions",
        kind: handlers::PARTITIONS_JOB,
        every: Duration::from_secs(86_400),
    },
    Schedule {
        name: "themes.maintenance",
        kind: commerce::themes::MAINTENANCE_JOB,
        every: Duration::from_secs(3600),
    },
    Schedule {
        name: "ops.sweep",
        kind: handlers::SWEEP_JOB,
        every: Duration::from_secs(900),
    },
];

/// A dedicated connection holding the leader lock, or `None` if another worker leads.
pub async fn try_lead(db: &PgPool) -> Result<Option<PgConnection>, sqlx::Error> {
    let mut conn = db.acquire().await?.detach();
    let leader = sqlx::query_scalar!(r#"SELECT pg_try_advisory_lock($1) AS "ok!""#, LEADER_LOCK)
        .fetch_one(&mut conn)
        .await?;
    Ok(leader.then_some(conn))
}

/// Enqueues the current slot of every schedule (idempotent per slot).
pub async fn enqueue_due(
    conn: &mut PgConnection,
    schedules: &[Schedule],
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    for s in schedules {
        let every = i64::try_from(s.every.as_secs().max(1)).unwrap_or(i64::MAX);
        let slot = now.timestamp().div_euclid(every);
        let mut job = NewJob::new(s.kind, json!({ "slot": slot }));
        job.max_attempts = 3;
        job.idempotency_key = Some(format!("cron:{}:{slot}", s.name));
        queue::enqueue(&mut *conn, &job).await?;
    }
    Ok(())
}

/// Tries to lead every `tick`; while leading, enqueues due jobs every `tick`.
pub async fn run(db: PgPool, tick: Duration, mut shutdown: watch::Receiver<bool>) {
    let mut leader: Option<PgConnection> = None;
    while !*shutdown.borrow() {
        if leader.is_none() {
            match try_lead(&db).await {
                Ok(Some(conn)) => {
                    tracing::info!("cron leadership acquired");
                    leader = Some(conn);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "cron leader election failed"),
            }
        }
        if let Some(conn) = leader.as_mut()
            && let Err(e) = enqueue_due(conn, SCHEDULES, Utc::now()).await
        {
            // Drop the connection (and with it the lock); re-elect on the next tick.
            tracing::warn!(error = %e, "cron enqueue failed, giving up leadership");
            leader = None;
        }
        tokio::select! {
            _ = shutdown.changed() => {}
            () = tokio::time::sleep(tick) => {}
        }
    }
}
