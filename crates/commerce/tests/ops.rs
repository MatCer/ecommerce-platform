//! Housekeeping sweeps: stuck assets, abandoned uploads, expired carts and handoffs.
#![allow(clippy::unwrap_used)]

use commerce::media::{self, NewUpload};
use commerce::ops;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::queue::{self, NewJob};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn asset(runtime: &PgPool, storage: &platform::storage::Storage, tenant: Uuid) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let up = media::create_upload(
        &mut tx,
        storage,
        "staff",
        &NewUpload {
            filename: Some("a.jpg".into()),
            content_type: "image/jpeg".into(),
            size: 10,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    up.asset.id
}

async fn exec(runtime: &PgPool, tenant: Uuid, sql: &'static str, id: Uuid) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(sql).bind(id).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

async fn status(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Option<(String, Option<String>)> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_as("SELECT status, error FROM assets WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn sweeps_stuck_assets_uploads_and_carts(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let storage = testkit::memory_storage();
    let shop = testkit::storefront::shop(&runtime, "ops").await;
    let tenant = shop.tenant;

    // An asset whose processing job died while it was `processing`.
    let stuck = asset(&runtime, &storage, tenant).await;
    exec(
        &runtime,
        tenant,
        "UPDATE assets SET status = 'processing' WHERE id = $1",
        stuck,
    )
    .await;
    let mut job = NewJob::new(media::PROCESS_JOB, json!({ "asset_id": stuck }));
    job.tenant_id = Some(tenant);
    let job_id = queue::enqueue(&runtime, &job).await.unwrap();
    sqlx::query("UPDATE queue.jobs SET status = 'dead', finished_at = now() WHERE id = $1")
        .bind(job_id)
        .execute(&db)
        .await
        .unwrap();

    // A pending upload from two days ago; a ready asset whose upload object is still there.
    let abandoned = asset(&runtime, &storage, tenant).await;
    exec(
        &runtime,
        tenant,
        "UPDATE assets SET created_at = now() - interval '2 days' WHERE id = $1",
        abandoned,
    )
    .await;
    let ready = asset(&runtime, &storage, tenant).await;
    exec(
        &runtime,
        tenant,
        "UPDATE assets SET status = 'ready', created_at = now() - interval '2 hours' WHERE id = $1",
        ready,
    )
    .await;
    let upload = media::upload_key(tenant, ready);
    storage
        .private
        .put(&upload, PutPayload::from_static(b"x"))
        .await
        .unwrap();
    let fresh = asset(&runtime, &storage, tenant).await;

    // Carts: an old open one, an old one with an order, a recent one; a used handoff.
    let old_order =
        testkit::storefront::raw_order(&runtime, &shop, shop.cz, "CZK", 100, 1, "confirmed").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let old: Uuid = sqlx::query_scalar(
        "INSERT INTO carts (tenant_id, market_id, locale, currency, last_activity_at)
         VALUES ($1, $2, 'cs', 'CZK', now() - interval '31 days') RETURNING id",
    )
    .bind(tenant)
    .bind(shop.cz)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let recent: Uuid = sqlx::query_scalar(
        "INSERT INTO carts (tenant_id, market_id, locale, currency) VALUES ($1, $2, 'cs', 'CZK')
         RETURNING id",
    )
    .bind(tenant)
    .bind(shop.cz)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE carts SET status = 'open', last_activity_at = now() - interval '40 days'
         WHERE id = (SELECT cart_id FROM orders WHERE id = $1)",
    )
    .bind(old_order)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO checkout_handoffs (token_hash, tenant_id, cart_id, market_id, expires_at, used_at, created_at)
         VALUES (sha256('h'::bytea), $1, $2, $3, now() - interval '2 hours', now() - interval '2 hours',
                 now() - interval '2 hours')",
    )
    .bind(tenant)
    .bind(recent)
    .bind(shop.cz)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let report = ops::sweep(&runtime, &storage, None).await.unwrap();
    assert_eq!(
        report,
        ops::SweepReport {
            assets_failed: 1,
            uploads_deleted: 1,
            upload_objects_purged: 1, // the ready asset (the others are under an hour old)
            carts_deleted: 1,
            handoffs_deleted: 1,
            indexes_dropped: 0,
        }
    );
    let (s, error) = status(&runtime, tenant, stuck).await.unwrap();
    assert_eq!(s, "failed");
    assert!(error.unwrap().contains("upload the image again"));
    assert!(status(&runtime, tenant, abandoned).await.is_none());
    assert!(
        status(&runtime, tenant, fresh).await.is_some(),
        "recent uploads stay"
    );
    assert!(
        storage.private.head(&upload).await.is_err(),
        "upload object purged"
    );

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let carts: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM carts ORDER BY id")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!carts.contains(&old));
    assert!(carts.contains(&recent));
    assert_eq!(carts.len(), 2, "the ordered cart stays");

    // Idempotent.
    assert_eq!(
        ops::sweep(&runtime, &storage, None).await.unwrap(),
        ops::SweepReport::default()
    );
}
