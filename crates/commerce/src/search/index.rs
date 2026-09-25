//! Index lifecycle and indexing (spec §11.1, A23, A27).
//!
//! Ordering guarantees:
//! - Every write for a product happens while its `search_product_state` row is locked, and the
//!   lock is held until Meilisearch applied the writes (tasks of an index apply in enqueue
//!   order). Documents therefore land in the order their Postgres state was read.
//! - Jobs are versioned by `dispatched_at`, a database-clock instant taken after the
//!   triggering change committed. A run records when it read the catalog (`read_at`); a job
//!   dispatched before the last `read_at` is stale and dropped, because that read already saw
//!   its change.
//! - During a rebuild, incremental jobs also write to the index being built. They hold a
//!   per-tenant advisory lock in shared mode while choosing their targets and writing; the
//!   swap takes it exclusively, so no write goes to an index that was just swapped out.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::documents::{self, Context};
use super::meili::{Meili, MeiliError, Task, quote};
use super::{SETTINGS_VERSION, index_uid};

/// Products loaded and written per rebuild batch.
const REBUILD_BATCH: i64 = 200;
const SETTINGS_WAIT: Duration = Duration::from_secs(300);
const REBUILD_WAIT: Duration = Duration::from_secs(1800);
/// Incremental writes are small; a slower engine fails the job, which retries.
const WRITE_WAIT: Duration = Duration::from_secs(60);

/// Index settings (versioned by [`SETTINGS_VERSION`]). Text fields hold normalized text
/// (`lang::analyze`), so Meilisearch's own language handling only sees folded ASCII stems.
pub fn settings(synonyms: &Value) -> Value {
    let equality = |patterns: &[&str], comparison: bool| {
        json!({
            "attributePatterns": patterns,
            "features": { "facetSearch": false, "filter": { "equality": true, "comparison": comparison } }
        })
    };
    json!({
        "searchableAttributes": [
            "skus", "eans", "name_stems", "name_folded", "variant_stems", "brand", "text_stems"
        ],
        "displayedAttributes": ["id", "product_id"],
        "filterableAttributes": [
            equality(&["price.*"], true),
            equality(&["opt.*", "param.*", "brand", "category_ids", "in_stock",
                       "active_in_markets", "id", "product_id", "skus", "eans"], false),
        ],
        "sortableAttributes": ["price", "popularity", "created_at"],
        "rankingRules": ["words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"],
        "distinctAttribute": "product_id",
        "typoTolerance": { "disableOnAttributes": ["skus", "eans"], "disableOnNumbers": true },
        // Per-tenant synonyms (normalized form, e.g. {"mikin": ["hoodi"]}); a placeholder
        // until tenants can edit them.
        "synonyms": synonyms,
        "stopWords": [],
        "pagination": { "maxTotalHits": 1000 },
        "faceting": { "maxValuesPerFacet": 100 },
    })
}

/// Per-tenant lock key shared by incremental jobs and the swap.
async fn lock_tenant(tx: &mut TenantTx, exclusive: bool) -> Result<(), Error> {
    let key = format!("search:{}", tx.tenant_id());
    if exclusive {
        sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", key)
            .fetch_one(&mut **tx)
            .await?;
    } else {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock_shared(hashtextextended($1, 0))",
            key
        )
        .fetch_one(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Creates the index if missing (a concurrent creator is fine) and applies the settings.
async fn create_with_settings(meili: &Meili, uid: &str) -> Result<(), MeiliError> {
    if !meili.index_exists(uid).await? {
        match meili
            .wait(meili.create_index(uid).await?, SETTINGS_WAIT)
            .await
        {
            Err(MeiliError::Task { error, .. }) if error == "index_already_exists" => {}
            other => other?,
        }
    }
    let task = meili.update_settings(uid, &settings(&json!({}))).await?;
    meili.wait(task, SETTINGS_WAIT).await
}

/// Makes sure every locale of the tenant's markets has a live index with current settings.
pub async fn ensure_indexes(db: &PgPool, meili: &Meili, tenant_id: Uuid) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let ctx = documents::load_context(&mut tx).await?;
    let current: BTreeMap<String, i32> =
        sqlx::query!("SELECT locale, settings_version FROM search_indexes")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| (r.locale, r.settings_version))
            .collect();
    tx.commit().await?;
    for locale in ctx.locales() {
        if current.get(&locale).is_some_and(|v| *v >= SETTINGS_VERSION) {
            continue;
        }
        create_with_settings(meili, &index_uid(tenant_id, &locale)).await?;
        let mut tx = tenant_tx(db, tenant_id).await?;
        sqlx::query!(
            "INSERT INTO search_indexes (tenant_id, locale, settings_version) VALUES ($1, $2, $3)
             ON CONFLICT (tenant_id, locale) DO UPDATE
             SET settings_version = GREATEST(search_indexes.settings_version, EXCLUDED.settings_version),
                 updated_at = now()",
            tenant_id,
            locale,
            SETTINGS_VERSION
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Locks the state rows of `products` (in id order, so concurrent lockers cannot deadlock)
/// and returns when each was last read for indexing.
async fn lock_products(
    tx: &mut TenantTx,
    products: &[Uuid],
) -> Result<BTreeMap<Uuid, Option<DateTime<Utc>>>, Error> {
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "INSERT INTO search_product_state (tenant_id, product_id)
         SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING",
        tenant_id,
        products
    )
    .execute(&mut **tx)
    .await?;
    Ok(sqlx::query!(
        "SELECT product_id, read_at FROM search_product_state
         WHERE product_id = ANY($1) ORDER BY product_id FOR UPDATE",
        products
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.product_id, r.read_at))
    .collect())
}

/// Index uids to write, per locale: the live index plus the one a rebuild is filling.
async fn targets(tx: &mut TenantTx) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let tenant_id = tx.tenant_id();
    Ok(
        sqlx::query!("SELECT locale, building_uid FROM search_indexes")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| {
                let mut uids = vec![index_uid(tenant_id, &r.locale)];
                uids.extend(r.building_uid);
                (r.locale, uids)
            })
            .collect(),
    )
}

fn id_list(ids: impl IntoIterator<Item = String>) -> String {
    let quoted: Vec<String> = ids.into_iter().map(|id| quote(&id)).collect();
    format!("[{}]", quoted.join(", "))
}

/// Replaces the documents of `products` in `uid` with `docs`: adds the new documents, then
/// deletes the products' documents that are no longer produced (removed variants, products
/// that became invisible). Both tasks are enqueued in this order; returns them.
async fn replace_products(
    meili: &Meili,
    uid: &str,
    products: &[Uuid],
    docs: &[Value],
) -> Result<Vec<Task>, MeiliError> {
    let products = id_list(products.iter().map(Uuid::to_string));
    let mut tasks = vec![];
    let filter = if docs.is_empty() {
        format!("product_id IN {products}")
    } else {
        tasks.push(meili.add_documents(uid, docs).await?);
        let keep = id_list(
            docs.iter()
                .filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_owned)),
        );
        format!("product_id IN {products} AND NOT id IN {keep}")
    };
    tasks.push(meili.delete_by_filter(uid, &filter).await?);
    Ok(tasks)
}

/// Waits until every task succeeded (any failure is an error: the job retries).
async fn wait_all(meili: &Meili, tasks: &[Task], limit: Duration) -> Result<(), Error> {
    for task in tasks {
        meili.wait(*task, limit).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexed {
    Done,
    /// A run that read the catalog after `dispatched_at` already indexed the product.
    Stale,
}

/// (Re)indexes one product. `dispatched_at` is the job's version (a database-clock instant
/// after the triggering change committed; `None`: always index). The product's state row
/// stays locked until Meilisearch applied every write, and `read_at` (when this run read the
/// catalog) is recorded only then, so a failed write is retried and never hides a change.
pub async fn index_product(
    db: &PgPool,
    meili: &Meili,
    tenant_id: Uuid,
    product_id: Uuid,
    dispatched_at: Option<DateTime<Utc>>,
) -> Result<Indexed, Error> {
    ensure_indexes(db, meili, tenant_id).await?;
    let mut tx = tenant_tx(db, tenant_id).await?;
    lock_tenant(&mut tx, false).await?;
    let read = lock_products(&mut tx, &[product_id]).await?;
    let last_read = read.get(&product_id).copied().flatten();
    if let (Some(last), Some(version)) = (last_read, dispatched_at)
        && last > version
    {
        return Ok(Indexed::Stale);
    }
    // Everything committed before this instant is visible to the reads below.
    let read_at = super::db_clock(&mut *tx).await?;
    let ctx = documents::load_context(&mut tx).await?;
    let docs = build(&mut tx, &ctx, &[product_id]).await?;
    let mut tasks = vec![];
    for (locale, uids) in targets(&mut tx).await? {
        let docs = docs.get(&locale).map(Vec::as_slice).unwrap_or_default();
        for uid in uids {
            tasks.extend(replace_products(meili, &uid, &[product_id], docs).await?);
        }
    }
    wait_all(meili, &tasks, WRITE_WAIT).await?;
    sqlx::query!(
        "UPDATE search_product_state SET read_at = $2, indexed_at = now() WHERE product_id = $1",
        product_id,
        read_at
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Indexed::Done)
}

async fn build(
    tx: &mut TenantTx,
    ctx: &Context,
    products: &[Uuid],
) -> Result<BTreeMap<String, Vec<Value>>, Error> {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for p in documents::load_products(tx, products, Utc::now()).await? {
        for (locale, docs) in documents::build_product(ctx, &p) {
            out.entry(locale).or_default().extend(docs);
        }
    }
    Ok(out)
}

/// Products in `category_id` or its subtree: a renamed or moved category changes their
/// category names and ancestor ids. (Deleted categories trigger a rebuild instead: their
/// memberships are gone by then.)
pub async fn category_products(tx: &mut TenantTx, category_id: Uuid) -> Result<Vec<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE sub AS (
               SELECT id FROM categories WHERE id = $1
               UNION SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id
           )
           SELECT DISTINCT product_id AS "id!" FROM product_categories
           WHERE category_id IN (SELECT id FROM sub)"#,
        category_id
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// Current state of a tenant's search indexes (admin status endpoint).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct IndexStatus {
    pub locale: String,
    /// Meilisearch index uid.
    pub index: String,
    pub settings_version: i32,
    /// A rebuild is filling a new index.
    pub rebuilding: bool,
    pub rebuilt_at: Option<DateTime<Utc>>,
    /// Documents (sellable variants) written by the last rebuild.
    pub documents: Option<i64>,
}

pub async fn status(tx: &mut TenantTx) -> Result<Vec<IndexStatus>, Error> {
    let tenant_id = tx.tenant_id();
    Ok(sqlx::query!(
        "SELECT locale, settings_version, building_uid, rebuilt_at, documents
         FROM search_indexes ORDER BY locale"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| IndexStatus {
        index: index_uid(tenant_id, &r.locale),
        locale: r.locale,
        settings_version: r.settings_version,
        rebuilding: r.building_uid.is_some(),
        rebuilt_at: r.rebuilt_at,
        documents: r.documents,
    })
    .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuilt {
    Done,
    /// A completed rebuild started after `dispatched_at`: nothing to do.
    Stale,
    /// Another rebuild of this tenant is running; try again later.
    Busy,
}

/// Rebuilds every locale index of the tenant into fresh indexes (`<live>__r<job_id>`) and
/// swaps them in atomically.
pub async fn rebuild(
    db: &PgPool,
    meili: &Meili,
    tenant_id: Uuid,
    job_id: i64,
    dispatched_at: Option<DateTime<Utc>>,
) -> Result<Rebuilt, Error> {
    // One rebuild per tenant at a time, via a session lock on a connection taken out of the
    // pool: if this future is dropped (lease lost, shutdown) the connection closes and the
    // lock goes with it, instead of lingering on a pooled connection.
    let mut conn = db.acquire().await?.detach();
    let key = format!("search-rebuild:{tenant_id}");
    let locked = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock(hashtextextended($1, 0)) AS "ok!""#,
        key
    )
    .fetch_one(&mut conn)
    .await?;
    if !locked {
        return Ok(Rebuilt::Busy);
    }
    let result = rebuild_locked(db, meili, tenant_id, job_id, dispatched_at).await;
    // Closing the session releases the lock.
    let _ = sqlx::Connection::close(conn).await;
    result
}

async fn rebuild_locked(
    db: &PgPool,
    meili: &Meili,
    tenant_id: Uuid,
    job_id: i64,
    dispatched_at: Option<DateTime<Utc>>,
) -> Result<Rebuilt, Error> {
    ensure_indexes(db, meili, tenant_id).await?;

    // 1. Fresh indexes, registered as build targets (incremental jobs start writing to them).
    //    An index left over by an interrupted rebuild is unregistered first (under the
    //    exclusive lock, so no job still targets it) and then deleted.
    let mut tx = tenant_tx(db, tenant_id).await?;
    let last_start: Option<DateTime<Utc>> =
        sqlx::query_scalar!("SELECT min(rebuild_started_at) FROM search_indexes")
            .fetch_one(&mut *tx)
            .await?;
    if let (Some(last), Some(version)) = (last_start, dispatched_at)
        && last > version
    {
        return Ok(Rebuilt::Stale);
    }
    lock_tenant(&mut tx, true).await?;
    let previous: Vec<String> = sqlx::query_scalar!(
        r#"SELECT building_uid AS "uid!" FROM search_indexes WHERE building_uid IS NOT NULL"#
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("UPDATE search_indexes SET building_uid = NULL WHERE building_uid IS NOT NULL")
        .execute(&mut *tx)
        .await?;
    let locales: Vec<String> =
        sqlx::query_scalar!("SELECT locale FROM search_indexes ORDER BY locale")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    for uid in previous {
        drop_index(meili, &uid).await?;
    }
    let mut building = BTreeMap::new();
    for locale in &locales {
        let uid = format!("{}__r{job_id}", index_uid(tenant_id, locale));
        // A retry of this job may find its own half-filled index: start from scratch.
        drop_index(meili, &uid).await?;
        create_with_settings(meili, &uid).await?;
        building.insert(locale.clone(), uid);
    }
    let mut tx = tenant_tx(db, tenant_id).await?;
    for (locale, uid) in &building {
        sqlx::query!(
            "UPDATE search_indexes SET building_uid = $2, updated_at = now() WHERE locale = $1",
            locale,
            uid
        )
        .execute(&mut *tx)
        .await?;
    }
    // Catalog reads below start after this instant (and after the targets are registered).
    let started_at = super::db_clock(&mut *tx).await?;
    tx.commit().await?;

    // 2. Fill them, a batch of products at a time, under the products' row locks.
    let mut tasks: Vec<Task> = vec![];
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut cursor = Uuid::nil();
    loop {
        let mut tx = tenant_tx(db, tenant_id).await?;
        let ids: Vec<Uuid> = sqlx::query_scalar!(
            "SELECT id FROM products WHERE status = 'active' AND id > $1 ORDER BY id LIMIT $2",
            cursor,
            REBUILD_BATCH
        )
        .fetch_all(&mut *tx)
        .await?;
        let Some(max) = ids.last().copied() else {
            break;
        };
        cursor = max;
        lock_products(&mut tx, &ids).await?;
        let ctx = documents::load_context(&mut tx).await?;
        for (locale, docs) in build(&mut tx, &ctx, &ids).await? {
            if let Some(uid) = building.get(&locale)
                && !docs.is_empty()
            {
                tasks.push(meili.add_documents(uid, &docs).await?);
                *counts.entry(locale).or_default() += i64::try_from(docs.len()).unwrap_or(i64::MAX);
            }
        }
        tx.commit().await?;
    }
    // Every batch must have landed: a failed one would swap in an incomplete index.
    wait_all(meili, &tasks, REBUILD_WAIT).await?;

    // 3. Swap all locales at once, under the exclusive tenant lock.
    let mut tx = tenant_tx(db, tenant_id).await?;
    lock_tenant(&mut tx, true).await?;
    let pairs: Vec<(String, String)> = building
        .iter()
        .map(|(locale, uid)| (index_uid(tenant_id, locale), uid.clone()))
        .collect();
    if !pairs.is_empty() {
        let task = meili.swap(&pairs).await?;
        meili.wait(task, SETTINGS_WAIT).await?;
    }
    for locale in building.keys() {
        sqlx::query!(
            "UPDATE search_indexes SET building_uid = NULL, rebuild_started_at = $2,
                    rebuilt_at = now(), documents = $3, updated_at = now()
             WHERE locale = $1",
            locale,
            started_at,
            counts.get(locale).copied().unwrap_or(0)
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // 4. The swapped-out indexes now hold the old documents.
    for uid in building.values() {
        if let Err(e) = drop_index(meili, uid).await {
            tracing::warn!(index = %uid, error = %e, "old search index not deleted");
        }
    }
    tracing::info!(%tenant_id, ?counts, "search indexes rebuilt");
    Ok(Rebuilt::Done)
}

/// Deletes an index and waits for it (a missing index is fine).
async fn drop_index(meili: &Meili, uid: &str) -> Result<(), Error> {
    match meili
        .wait(meili.delete_index(uid).await?, SETTINGS_WAIT)
        .await
    {
        Err(MeiliError::Task { error, .. }) if error == "index_not_found" => Ok(()),
        other => other.map_err(Error::from),
    }
}
