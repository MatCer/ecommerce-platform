//! Leased job queue and outbox semantics (spec §13, A14) against a real Postgres.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use platform::queue::{self, Failed, Job, NewJob};
use serde_json::json;
use sqlx::PgPool;

const LEASE: Duration = Duration::from_secs(60);

fn queues() -> Vec<String> {
    vec!["default".into()]
}

async fn claim_one(runtime: &PgPool, owner: &str) -> Option<Job> {
    queue::claim(runtime, owner, &queues(), 1, LEASE)
        .await
        .unwrap()
        .pop()
}

/// Simulates the passage of the lease without sleeping (owner connection, tests only).
async fn expire_lease(owner: &PgPool, id: i64) {
    sqlx::query("UPDATE queue.jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(owner)
        .await
        .unwrap();
}

async fn status(owner: &PgPool, id: i64) -> (String, i32) {
    sqlx::query_as("SELECT status, attempts FROM queue.jobs WHERE id = $1")
        .bind(id)
        .fetch_one(owner)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn crashed_worker_lease_is_reclaimed_and_stale_completion_is_fenced(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let id = queue::enqueue(&runtime, &NewJob::new("demo", json!({})))
        .await
        .unwrap();

    let first = claim_one(&runtime, "worker-a").await.unwrap();
    assert_eq!((first.id, first.attempts), (id, 1));
    // While the lease is live nobody else gets the job.
    assert!(claim_one(&runtime, "worker-b").await.is_none());

    // worker-a "crashes": no heartbeat, the lease runs out, worker-b reclaims the job.
    expire_lease(&db, id).await;
    let second = claim_one(&runtime, "worker-b").await.unwrap();
    assert_eq!((second.id, second.attempts), (id, 2));
    assert_ne!(first.lease_token, second.lease_token);

    // worker-a comes back: its token is stale, every write is refused.
    assert!(!queue::heartbeat(&runtime, &first, LEASE).await.unwrap());
    assert!(!queue::complete(&runtime, &first).await.unwrap());
    assert_eq!(
        queue::fail(&runtime, &first, "late", Some(Duration::ZERO))
            .await
            .unwrap(),
        Failed::LeaseLost
    );
    assert_eq!(status(&db, id).await, ("running".into(), 2));

    assert!(queue::heartbeat(&runtime, &second, LEASE).await.unwrap());
    assert!(queue::complete(&runtime, &second).await.unwrap());
    assert_eq!(status(&db, id).await, ("done".into(), 2));
    // Completing twice is refused as well.
    assert!(!queue::complete(&runtime, &second).await.unwrap());
}

#[sqlx::test(migrations = "../../migrations")]
async fn failures_retry_with_delay_then_die(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let mut job = NewJob::new("demo", json!({}));
    job.max_attempts = 2;
    let id = queue::enqueue(&runtime, &job).await.unwrap();

    let claimed = claim_one(&runtime, "w").await.unwrap();
    let outcome = queue::fail(&runtime, &claimed, "boom", Some(Duration::from_secs(300)))
        .await
        .unwrap();
    assert_eq!(outcome, Failed::Retrying);
    assert_eq!(status(&db, id).await, ("queued".into(), 1));
    // Not due yet.
    assert!(claim_one(&runtime, "w").await.is_none());

    sqlx::query("UPDATE queue.jobs SET run_at = now() WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    let claimed = claim_one(&runtime, "w").await.unwrap();
    let outcome = queue::fail(&runtime, &claimed, "boom again", Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(outcome, Failed::Dead);
    let last_error: String = sqlx::query_scalar("SELECT last_error FROM queue.jobs WHERE id = $1")
        .bind(id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(last_error, "boom again");
    assert!(claim_one(&runtime, "w").await.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn permanent_failure_is_dead_immediately(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let id = queue::enqueue(&runtime, &NewJob::new("demo", json!({})))
        .await
        .unwrap();
    let claimed = claim_one(&runtime, "w").await.unwrap();
    assert_eq!(
        queue::fail(&runtime, &claimed, "bad payload", None)
            .await
            .unwrap(),
        Failed::Dead
    );
    assert_eq!(status(&db, id).await, ("dead".into(), 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn lease_expiring_on_final_attempt_kills_the_job(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let mut job = NewJob::new("demo", json!({}));
    job.max_attempts = 1;
    let id = queue::enqueue(&runtime, &job).await.unwrap();
    claim_one(&runtime, "w").await.unwrap();
    expire_lease(&db, id).await;
    assert!(claim_one(&runtime, "w").await.is_none());
    assert_eq!(status(&db, id).await, ("dead".into(), 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn idempotency_key_prevents_duplicate_jobs(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let mut job = NewJob::new("demo", json!({"n": 1}));
    job.idempotency_key = Some("cron:demo:1".into());
    let a = queue::enqueue(&runtime, &job).await.unwrap();
    let b = queue::enqueue(&runtime, &job).await.unwrap();
    assert_eq!(a, b);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM queue.jobs")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_claims_never_share_a_job(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    for n in 0..20 {
        queue::enqueue(&runtime, &NewJob::new("demo", json!({ "n": n })))
            .await
            .unwrap();
    }
    let mut tasks = Vec::new();
    for w in 0..4 {
        let runtime = runtime.clone();
        tasks.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            loop {
                let batch = queue::claim(&runtime, &format!("w{w}"), &queues(), 3, LEASE)
                    .await
                    .unwrap();
                if batch.is_empty() {
                    return ids;
                }
                ids.extend(batch.iter().map(|j| j.id));
            }
        }));
    }
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    all.sort_unstable();
    let before = all.len();
    all.dedup();
    assert_eq!((before, all.len()), (20, 20));
}

#[sqlx::test(migrations = "../../migrations")]
async fn publish_takes_the_tenant_from_the_transaction(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (tenant, _) = testkit::tenant(&runtime, "alpha").await;

    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    queue::publish(&mut *tx, "market.created", &json!({"id": 1}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Rolled back events never become visible.
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    queue::publish(&mut *tx, "market.created", &json!({"id": 2}))
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    queue::publish(&runtime, "platform.thing", &json!({}))
        .await
        .unwrap();

    let rows: Vec<(Option<uuid::Uuid>, String)> =
        sqlx::query_as("SELECT tenant_id, type FROM queue.outbox ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            (Some(tenant), "market.created".into()),
            (None, "platform.thing".into())
        ]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn purge_removes_old_finished_work_only(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let done = queue::enqueue(&runtime, &NewJob::new("demo", json!({})))
        .await
        .unwrap();
    let claimed = claim_one(&runtime, "w").await.unwrap();
    queue::complete(&runtime, &claimed).await.unwrap();
    let dead = queue::enqueue(&runtime, &NewJob::new("demo", json!({})))
        .await
        .unwrap();
    let claimed = claim_one(&runtime, "w").await.unwrap();
    queue::fail(&runtime, &claimed, "x", None).await.unwrap();
    sqlx::query("UPDATE queue.jobs SET finished_at = now() - interval '8 days'")
        .execute(&db)
        .await
        .unwrap();

    let purged = queue::purge(&runtime, Duration::from_secs(7 * 86_400))
        .await
        .unwrap();
    assert_eq!(purged, 1);
    let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM queue.jobs")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(left, vec![dead]);
    assert_ne!(done, dead);
}
