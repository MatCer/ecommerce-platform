//! Housekeeping sweeps (spec §13, WP14), run by the worker cron every 15 minutes. Each step
//! is idempotent and bounded per run; tenant data is only touched inside `tenant_tx`.
//! - media assets stuck in `processing` whose job died (e.g. the lease ran out on the final
//!   attempt) become `failed` with a reason the merchant can act on;
//! - abandoned uploads: `pending` assets older than a day are deleted (objects purged by a
//!   job); completed assets lose their `uploads/` object once the presigned URL expired;
//! - expired carts (open, 30 days inactive, never ordered) and spent checkout handoffs;
//! - search indexes of locales no market uses any more.

use object_store::ObjectStoreExt;
use platform::Error;
use platform::db::tenant_tx;
use platform::queue;
use platform::storage::Storage;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::media;
use crate::search::{self, Meili};

pub const SWEEP_JOB: &str = "ops.sweep";
/// Actor recorded in the audit log for sweeper deletions.
pub const ACTOR: &str = "system:sweeper";
const STUCK_REASON: &str =
    "processing did not finish (the worker stopped or timed out); upload the image again";
const BATCH: i64 = 500;

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct SweepReport {
    pub assets_failed: u64,
    pub uploads_deleted: u64,
    pub upload_objects_purged: u64,
    pub carts_deleted: u64,
    pub handoffs_deleted: u64,
    pub indexes_dropped: u64,
}

/// Runs every sweep. Meilisearch is optional (search is a degraded component, A27): without
/// it stale indexes wait for the next run.
pub async fn sweep(
    db: &PgPool,
    storage: &Storage,
    meili: Option<&Meili>,
) -> Result<SweepReport, Error> {
    let mut report = SweepReport {
        assets_failed: fail_stuck_assets(db).await?,
        ..SweepReport::default()
    };
    let tenants: Vec<Uuid> = sqlx::query_scalar!("SELECT id FROM platform.tenants ORDER BY id")
        .fetch_all(db)
        .await?;
    for tenant in tenants {
        let (deleted, purged) = abandoned_uploads(db, storage, tenant).await?;
        report.uploads_deleted += deleted;
        report.upload_objects_purged += purged;
        let (carts, handoffs) = expired_carts(db, tenant).await?;
        report.carts_deleted += carts;
        report.handoffs_deleted += handoffs;
        if let Some(meili) = meili {
            match stale_indexes(db, meili, tenant).await {
                Ok(n) => report.indexes_dropped += n,
                Err(e) => tracing::warn!(%tenant, error = %e, "stale index cleanup skipped"),
            }
        }
    }
    Ok(report)
}

/// Dead `media.process` jobs whose asset is still `processing` (newest 200 dead jobs per run;
/// dead jobs are kept 30 days). Only when the job died after the asset last changed, so an
/// old dead job never fails a later processing run of the same asset; a requeued job is no
/// longer dead and not considered.
pub async fn fail_stuck_assets(db: &PgPool) -> Result<u64, Error> {
    let dead = queue::list(db, Some("dead"), Some(media::PROCESS_JOB), None, 200).await?;
    let mut failed = 0;
    for job in dead {
        let (Some(tenant), Some(finished), Some(asset)) = (
            job.tenant_id,
            job.finished_at,
            job.payload
                .get("asset_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()),
        ) else {
            continue;
        };
        let mut tx = tenant_tx(db, tenant).await?;
        failed += sqlx::query!(
            "UPDATE assets SET status = 'failed', error = $2, updated_at = now()
             WHERE id = $1 AND status = 'processing' AND updated_at <= $3",
            asset,
            STUCK_REASON,
            finished
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
    }
    Ok(failed)
}

/// `(pending assets deleted, upload objects purged)` for one tenant.
pub async fn abandoned_uploads(
    db: &PgPool,
    storage: &Storage,
    tenant: Uuid,
) -> Result<(u64, u64), Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let stale = sqlx::query_scalar!(
        "SELECT id FROM assets WHERE status = 'pending' AND created_at < now() - interval '1 day'
         ORDER BY id LIMIT $1",
        BATCH
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut deleted = 0;
    for id in stale {
        let mut tx = tenant_tx(db, tenant).await?;
        // Re-checked under the row lock: a completion that raced the listing wins.
        let still = sqlx::query_scalar!(
            "SELECT id FROM assets WHERE id = $1 AND status = 'pending'
               AND created_at < now() - interval '1 day' FOR UPDATE",
            id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if still.is_none() {
            continue;
        }
        match media::delete(&mut tx, storage, ACTOR, id).await {
            Ok(()) => {
                tx.commit().await?;
                deleted += 1;
            }
            // Referenced by a product: leave it to the merchant.
            Err(Error::Conflict { .. } | Error::NotFound) => {}
            Err(e) => return Err(e),
        }
    }

    // The presigned URL lives 15 minutes; an hour later nothing can be uploaded any more.
    let mut tx = tenant_tx(db, tenant).await?;
    let done = sqlx::query_scalar!(
        "SELECT id FROM assets
         WHERE status <> 'pending' AND upload_purged_at IS NULL
           AND created_at < now() - interval '1 hour'
         ORDER BY id LIMIT $1",
        BATCH
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut purged = 0;
    for id in done {
        match storage.private.delete(&media::upload_key(tenant, id)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        let mut tx = tenant_tx(db, tenant).await?;
        purged += sqlx::query!(
            "UPDATE assets SET upload_purged_at = now() WHERE id = $1",
            id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
    }
    Ok((deleted, purged))
}

/// `(carts, handoffs)` deleted for one tenant: open or abandoned carts without activity for 30
/// days and never ordered (lines, coupon and handoffs cascade), and handoffs that were used or
/// expired over an hour ago.
pub async fn expired_carts(db: &PgPool, tenant: Uuid) -> Result<(u64, u64), Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let carts = sqlx::query!(
        "DELETE FROM carts WHERE id IN (
             SELECT c.id FROM carts c
             WHERE c.status IN ('open', 'abandoned')
               AND c.last_activity_at < now() - interval '30 days'
               AND NOT EXISTS (SELECT 1 FROM orders o WHERE o.cart_id = c.id)
             LIMIT $1)",
        BATCH * 4
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let handoffs = sqlx::query!(
        "DELETE FROM checkout_handoffs
         WHERE (used_at IS NOT NULL OR expires_at < now()) AND created_at < now() - interval '1 hour'"
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok((carts, handoffs))
}

/// Drops the indexes (live and half-built) of locales no market of the tenant has any more.
pub async fn stale_indexes(db: &PgPool, meili: &Meili, tenant: Uuid) -> Result<u64, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let stale = sqlx::query!(
        "SELECT locale, building_uid FROM search_indexes
         WHERE locale NOT IN (SELECT DISTINCT unnest(locales) FROM markets)"
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut dropped = 0;
    for row in stale {
        for uid in std::iter::once(search::index_uid(tenant, &row.locale)).chain(row.building_uid) {
            meili.delete_index(&uid).await?;
        }
        let mut tx = tenant_tx(db, tenant).await?;
        dropped += sqlx::query!("DELETE FROM search_indexes WHERE locale = $1", row.locale)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
    }
    Ok(dropped)
}
