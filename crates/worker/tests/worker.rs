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
        queue::publish(&mut *tx, "demo.happened", &json!({ "n": n }))
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

/// Catalog, price and stock events become one debounced search job per product (§11.1).
#[sqlx::test(migrations = "../../migrations")]
async fn outbox_versions_search_indexing_per_product(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "alpha").await;
    let product = testkit::catalog::product(&runtime, tenant, "TS", 2).await;
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    let adj = commerce::inventory::Adjustment {
        delta: Some(3),
        on_hand: None,
        note: None,
    };
    commerce::inventory::adjust(&mut tx, "t", product.variants[1].id, "r1", &adj)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM queue.outbox WHERE type = 'inventory.changed'")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(payload["product_id"], json!(product.id));

    // product.created + inventory.changed in one dispatch batch → one versioned job.
    while outbox::dispatch_batch(&runtime).await.unwrap() > 0 {}
    let jobs: Vec<(serde_json::Value, Option<uuid::Uuid>, bool)> = sqlx::query_as(
        "SELECT payload, tenant_id, run_at > now() FROM queue.jobs
         WHERE kind = 'search.index_product'",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].0["product_id"], json!(product.id));
    assert!(commerce::search::job_version(&jobs[0].0).is_some());
    assert_eq!(jobs[0].1, Some(tenant));
    assert!(jobs[0].2, "runs after the delay");
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
    let (stop, task) = start(
        &runtime,
        worker::handlers::all(testkit::memory_storage(), testkit::dead_meili()),
        fast_config(),
    );
    let (attempts, _) = wait_for_status(&db, id, "done").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(attempts, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn media_jobs_process_and_purge_assets(db: PgPool) {
    use commerce::media::{self, AssetStatus, NewUpload};
    use image::ImageEncoder;
    use object_store::{ObjectStoreExt, PutPayload, path::Path};

    let runtime = testkit::runtime_pool(&db, 4).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &[200u8; 3 * 50 * 40],
            50,
            40,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();

    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    let input = NewUpload {
        filename: None,
        content_type: "image/png".into(),
        size: png.len() as u64,
    };
    let up = media::create_upload(&mut tx, &storage, "u1", &input)
        .await
        .unwrap();
    let key = Path::from(format!("uploads/{tenant}/{}", up.asset.id));
    storage
        .private
        .put(&key, PutPayload::from(png))
        .await
        .unwrap();
    media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let job_id = |kind: &'static str| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT max(id) FROM queue.jobs WHERE kind = $1")
                .bind(kind)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let (stop, task) = start(
        &runtime,
        worker::handlers::all(storage.clone(), testkit::dead_meili()),
        fast_config(),
    );
    wait_for_status(&db, job_id(media::PROCESS_JOB).await, "done").await;

    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    let asset = media::get(&mut tx, &storage, up.asset.id).await.unwrap();
    assert_eq!(asset.status, AssetStatus::Ready);
    assert_eq!(asset.variants.len(), 3);
    media::delete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    wait_for_status(&db, job_id(media::PURGE_JOB).await, "done").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert!(storage.private.head(&key).await.is_err());
    let original = Path::from(format!("originals/{tenant}/{}", up.asset.id));
    assert!(storage.private.head(&original).await.is_err());
    for v in &asset.variants {
        assert!(
            storage
                .public
                .head(&Path::from(v.key.as_str()))
                .await
                .is_err()
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn media_job_failing_every_attempt_marks_the_asset_failed(db: PgPool) {
    use commerce::media::{self, AssetStatus, NewUpload};
    use image::ImageEncoder;
    use object_store::{ObjectStoreExt, PutPayload, path::Path};

    let runtime = testkit::runtime_pool(&db, 4).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[10u8; 3 * 8 * 8], 8, 8, image::ExtendedColorType::Rgb8)
        .unwrap();
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    let input = NewUpload {
        filename: None,
        content_type: "image/png".into(),
        size: png.len() as u64,
    };
    let up = media::create_upload(&mut tx, &storage, "u1", &input)
        .await
        .unwrap();
    let key = Path::from(format!("uploads/{tenant}/{}", up.asset.id));
    storage
        .private
        .put(&key, PutPayload::from(png))
        .await
        .unwrap();
    media::complete(&mut tx, &storage, "u1", up.asset.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // The original vanishes: every attempt fails with a storage error.
    let original = Path::from(format!("originals/{tenant}/{}", up.asset.id));
    storage.private.delete(&original).await.unwrap();

    let id: i64 = sqlx::query_scalar("SELECT id FROM queue.jobs WHERE kind = $1")
        .bind(media::PROCESS_JOB)
        .fetch_one(&db)
        .await
        .unwrap();
    let (stop, task) = start(
        &runtime,
        worker::handlers::all(storage.clone(), testkit::dead_meili()),
        fast_config(),
    );
    let (attempts, _) = wait_for_status(&db, id, "dead").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(attempts, 5);
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    let asset = media::get(&mut tx, &storage, up.asset.id).await.unwrap();
    assert_eq!(asset.status, AssetStatus::Failed);
    assert!(asset.error.unwrap().contains("upload the image again"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn scheduled_sale_start_publishes_price_changed(db: PgPool) {
    use commerce::money::Currency;
    use commerce::promotions::sales::{self, SaleDiscount, SaleInput, SaleTargets};

    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let product = testkit::catalog::product(&runtime, tenant, "T", 1).await;
    let list = testkit::pricing::price_list(&runtime, tenant, "cz", Currency::Czk).await;
    let variant = product.variants[0].id;
    testkit::pricing::set_prices(&runtime, tenant, list.id, &[(variant, 10_000)]).await;
    let mut tx = platform::db::tenant_tx(&runtime, tenant).await.unwrap();
    sales::create(
        &mut tx,
        "u",
        &SaleInput {
            name: "Soon".into(),
            discount: SaleDiscount::Percent { basis_points: 1000 },
            starts_at: Some(Utc::now() + chrono::Duration::milliseconds(500)),
            ends_at: None,
            targets: SaleTargets {
                all: true,
                ..SaleTargets::default()
            },
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let (stop, task) = start(
        &runtime,
        worker::handlers::all(testkit::memory_storage(), testkit::dead_meili()),
        fast_config(),
    );
    let mut found = None;
    for _ in 0..250 {
        found = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT payload FROM queue.outbox
             WHERE tenant_id = $1 AND type = 'price.changed' AND payload->>'cause' = 'sale'",
        )
        .bind(tenant)
        .fetch_optional(&db)
        .await
        .unwrap();
        if found.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop.send(true).unwrap();
    task.await.unwrap();
    let event = found.expect("no price.changed for the sale start");
    assert_eq!(event["before_minor"], 10_000);
    assert_eq!(event["after_minor"], 9_000);
    assert_eq!(event["variant_id"], variant.to_string());
}
