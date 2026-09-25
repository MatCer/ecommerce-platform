//! Runner, outbox dispatcher and cron leader against a real Postgres (spec §13, A14).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use platform::queue::{self, NewJob};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::watch;
use worker::runner::{self, Handlers, JobError, RunnerConfig};
use worker::{cron, outbox};

fn fast_config() -> RunnerConfig {
    let mut cfg = RunnerConfig::new("test".into(), 2);
    cfg.poll_interval = Duration::from_millis(20);
    cfg.heartbeat_every = Duration::from_millis(50);
    cfg.backoff_base = Duration::from_millis(10);
    cfg.backoff_cap = Duration::from_millis(20);
    cfg
}

/// Starts the runner in the background; the returned sender stops it.
fn start(
    runtime: &PgPool,
    handlers: Handlers,
    cfg: RunnerConfig,
) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    let (stop, shutdown) = watch::channel(false);
    let task = tokio::spawn(runner::run(runtime.clone(), handlers, cfg, shutdown));
    (stop, task)
}

async fn wait_for_status(owner: &PgPool, id: i64, want: &str) -> (i32, Option<String>) {
    for _ in 0..500 {
        let (status, attempts, err): (String, i32, Option<String>) =
            sqlx::query_as("SELECT status, attempts, last_error FROM queue.jobs WHERE id = $1")
                .bind(id)
                .fetch_one(owner)
                .await
                .unwrap();
        if status == want {
            return (attempts, err);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("job {id} never reached {want}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn flaky_handler_is_retried_until_it_succeeds(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let calls = Arc::new(AtomicU32::new(0));
    let seen = calls.clone();
    let handlers = Handlers::default().register("demo.flaky", move |_ctx, _job| {
        let calls = seen.clone();
        async move {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(JobError::Retry("transient".into()))
            } else {
                Ok(())
            }
        }
    });
    let id = queue::enqueue(&runtime, &NewJob::new("demo.flaky", json!({})))
        .await
        .unwrap();

    let (stop, task) = start(&runtime, handlers, fast_config());
    let (attempts, err) = wait_for_status(&db, id, "done").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!((attempts, err), (3, None));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn permanent_errors_panics_and_unknown_kinds(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let handlers = Handlers::default()
        .register("demo.bad", |_ctx, _job| async {
            Err(JobError::Permanent("invalid payload".into()))
        })
        .register("demo.panic", |_ctx, _job| async {
            panic!("handler bug");
        });
    let mut bad = NewJob::new("demo.bad", json!({}));
    bad.max_attempts = 5;
    let bad = queue::enqueue(&runtime, &bad).await.unwrap();
    let mut panics = NewJob::new("demo.panic", json!({}));
    panics.max_attempts = 2;
    let panics = queue::enqueue(&runtime, &panics).await.unwrap();
    let mut unknown = NewJob::new("demo.unknown", json!({}));
    unknown.max_attempts = 2;
    let unknown = queue::enqueue(&runtime, &unknown).await.unwrap();

    let (stop, task) = start(&runtime, handlers, fast_config());
    let bad = wait_for_status(&db, bad, "dead").await;
    let panics = wait_for_status(&db, panics, "dead").await;
    let unknown = wait_for_status(&db, unknown, "dead").await;
    stop.send(true).unwrap();
    task.await.unwrap();

    assert_eq!(bad, (1, Some("invalid payload".into())));
    assert_eq!(panics, (2, Some("handler panicked".into())));
    assert_eq!(
        unknown,
        (2, Some("no handler for job kind demo.unknown".into()))
    );
}

/// A worker that stalls past its lease loses the job to another worker and must not
/// complete it afterwards (fencing), even though its handler eventually returns Ok.
#[sqlx::test(migrations = "../../migrations")]
async fn runner_abandons_a_job_whose_lease_was_taken_over(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let notify = started.clone();
    let handlers = Handlers::default().register("demo.slow", move |_ctx, _job| {
        let started = notify.clone();
        async move {
            started.notify_one();
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(())
        }
    });
    let id = queue::enqueue(&runtime, &NewJob::new("demo.slow", json!({})))
        .await
        .unwrap();
    let mut cfg = fast_config();
    cfg.concurrency = 1;
    let (stop, task) = start(&runtime, handlers, cfg);
    started.notified().await;

    // The lease runs out and another worker reclaims the job.
    sqlx::query("UPDATE queue.jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    let other = queue::claim(
        &runtime,
        "other",
        &["default".into()],
        1,
        Duration::from_secs(60),
    )
    .await
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(other.id, id);

    // The runner's next heartbeat notices, aborts the handler and leaves the job alone.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT lease_owner FROM queue.jobs WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(owner.as_deref(), Some("other"));
    assert!(queue::complete(&runtime, &other).await.unwrap());
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("runner stops promptly")
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn outbox_fans_out_once_and_atomically(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "alpha").await;
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    for n in 0..3 {
        queue::publish(&mut *tx, "market.created", &json!({ "n": n }))
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();

    // A dispatch that rolls back leaves no jobs and no dispatched events.
    let mut tx = runtime.begin().await.unwrap();
    let events = queue::claim_outbox(&mut *tx, 10).await.unwrap();
    assert_eq!(events.len(), 3);
    queue::enqueue(&mut *tx, &NewJob::new("events.log", json!({})))
        .await
        .unwrap();
    queue::mark_dispatched(&mut *tx, &[events[0].id])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let (jobs, pending): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM queue.jobs), (SELECT count(*) FROM queue.outbox WHERE dispatched_at IS NULL)",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!((jobs, pending), (0, 3));

    // Two dispatchers racing: every event is dispatched exactly once.
    let (a, b) = tokio::join!(
        outbox::dispatch_batch(&runtime),
        outbox::dispatch_batch(&runtime)
    );
    assert_eq!(a.unwrap() + b.unwrap(), 3);
    assert_eq!(outbox::dispatch_batch(&runtime).await.unwrap(), 0);

    let rows: Vec<(String, Option<uuid::Uuid>, String)> = sqlx::query_as(
        "SELECT kind, tenant_id, idempotency_key FROM queue.jobs ORDER BY idempotency_key",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    for (kind, job_tenant, key) in &rows {
        assert_eq!(kind, "events.log");
        assert_eq!(*job_tenant, Some(tenant));
        assert!(
            key.starts_with("outbox:") && key.ends_with(":events.log"),
            "{key}"
        );
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM queue.outbox WHERE dispatched_at IS NULL")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(pending, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_one_cron_leader_and_one_job_per_slot(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let mut leader = cron::try_lead(&runtime)
        .await
        .unwrap()
        .expect("first leads");
    assert!(cron::try_lead(&runtime).await.unwrap().is_none());

    let t = Utc.with_ymd_and_hms(2026, 9, 25, 10, 15, 0).unwrap();
    cron::enqueue_due(&mut leader, cron::SCHEDULES, t)
        .await
        .unwrap();
    // Same hour again (e.g. a second leader after a network split): no duplicate.
    cron::enqueue_due(
        &mut leader,
        cron::SCHEDULES,
        t + chrono::Duration::minutes(30),
    )
    .await
    .unwrap();
    cron::enqueue_due(&mut leader, cron::SCHEDULES, t + chrono::Duration::hours(1))
        .await
        .unwrap();
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT idempotency_key FROM queue.jobs ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
    let slot = t.timestamp() / 3600;
    assert_eq!(
        keys,
        vec![
            format!("cron:maintenance.cleanup:{slot}"),
            format!("cron:maintenance.cleanup:{}", slot + 1)
        ]
    );

    // When the leader's connection goes away, another worker takes over.
    sqlx::Connection::close(leader).await.unwrap();
    assert!(cron::try_lead(&runtime).await.unwrap().is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn cleanup_job_runs_end_to_end(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let id = queue::enqueue(&runtime, &NewJob::new("maintenance.cleanup", json!({})))
        .await
        .unwrap();
    let (stop, task) = start(&runtime, worker::handlers::all(), fast_config());
    let (attempts, _) = wait_for_status(&db, id, "done").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(attempts, 1);
}
