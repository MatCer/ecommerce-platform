//! Product search (spec §11.1, D6, A23, A27): Meilisearch documents per sellable variant, the
//! cs/sk normalizer, indexing jobs and the storefront query service.
//!
//! - [`lang`]: the normalizer applied at index and query time.
//! - [`documents`]: Postgres → Meilisearch documents (one per sellable variant and locale).
//! - [`index`]: index lifecycle, incremental indexing with stale-version dropping, rebuilds
//!   with an index swap.
//! - [`query`]: the storefront query service (variant-correct filters, facet availability,
//!   rehydration from Postgres) and the zero-result log.
//!
//! Browsers never talk to Meilisearch; the API holds a search-only key, the worker the admin
//! key (A27).

pub mod documents;
pub mod index;
pub mod lang;
pub mod meili;
pub mod query;

use std::time::Duration;

use chrono::Utc;
use platform::queue::NewJob;
use serde_json::{Value, json};
use uuid::Uuid;

pub use meili::{Meili, MeiliError};

/// (Re)indexes one product: `{"product_id", "version"}`.
pub const INDEX_PRODUCT_JOB: &str = "search.index_product";
/// Reindexes the products of a changed category: `{"category_id", "version"}`.
pub const REINDEX_CATEGORY_JOB: &str = "search.reindex_category";
/// Rebuilds all indexes of a tenant into new indexes and swaps them in: `{"version"}`.
pub const REBUILD_JOB: &str = "search.rebuild";

/// Bumped whenever [`index::settings`] changes; indexes with an older version get the new
/// settings applied by the next indexing job.
pub const SETTINGS_VERSION: i32 = 2;

/// Meilisearch outages can last a while; indexing jobs keep retrying (capped backoff).
const JOB_ATTEMPTS: i32 = 25;
/// Indexing jobs wait this long, so a burst of changes to one product is indexed once: the
/// first job to run reads everything, the others are dropped as stale.
const DELAY: Duration = Duration::from_secs(2);
/// Rebuild requests from catalog changes wait longer (they tend to come in bursts too).
const REBUILD_DELAY: Duration = Duration::from_secs(10);

/// The live index of a tenant + locale (A27: full tenant UUID).
pub fn index_uid(tenant_id: Uuid, locale: &str) -> String {
    format!("t_{tenant_id}_{locale}")
}

/// Market code as a document key (`price.<key>`). Codes are `[a-z0-9-]`, so mapping `-` to
/// `_` is injective and yields a plain Meilisearch attribute name.
pub fn market_key(code: &str) -> String {
    code.replace('-', "_")
}

/// Draws the next search version (spec A27). One sequence orders both job dispatch and
/// catalog reads, so "this run read the catalog after that job was dispatched" is a plain
/// comparison; a sequence cannot step backwards like a clock.
pub async fn next_version<'c>(db: impl sqlx::PgExecutor<'c>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT nextval('search_versions') AS "v!""#)
        .fetch_one(db)
        .await
}

/// A search job carrying its `version`: drawn after the triggering change committed. The job
/// is stale, and dropped, when a completed indexing run drew a higher read version. No
/// idempotency key: coalescing into an existing job could hide a change behind a job that
/// already read the catalog.
fn versioned(
    kind: &'static str,
    mut payload: Value,
    tenant_id: Uuid,
    version: i64,
    delay: Duration,
) -> NewJob<'static> {
    payload["version"] = json!(version);
    let mut job = NewJob::new(kind, payload);
    job.tenant_id = Some(tenant_id);
    // Scheduling only (never compared for ordering).
    job.run_at = chrono::Duration::from_std(delay)
        .ok()
        .map(|d| Utc::now() + d);
    job.max_attempts = JOB_ATTEMPTS;
    job
}

/// The version of a search job (`None`: unversioned, never considered stale).
pub fn job_version(payload: &Value) -> Option<i64> {
    payload.get("version").and_then(Value::as_i64)
}

pub fn index_product_job(tenant_id: Uuid, product_id: Uuid, version: i64) -> NewJob<'static> {
    versioned(
        INDEX_PRODUCT_JOB,
        json!({ "product_id": product_id }),
        tenant_id,
        version,
        DELAY,
    )
}

pub fn rebuild_job(tenant_id: Uuid, version: i64) -> NewJob<'static> {
    versioned(REBUILD_JOB, json!({}), tenant_id, version, REBUILD_DELAY)
}

/// A rebuild requested by staff: runs right away.
pub fn manual_rebuild_job(tenant_id: Uuid, version: i64) -> NewJob<'static> {
    versioned(REBUILD_JOB, json!({}), tenant_id, version, Duration::ZERO)
}

/// The search job an outbox event triggers, if any (the dispatcher enqueues it next to the
/// generic subscribers). `version`: drawn after the events were claimed.
pub fn job_for_event(
    tenant_id: Option<Uuid>,
    event_type: &str,
    payload: &Value,
    version: i64,
) -> Option<NewJob<'static>> {
    let tenant_id = tenant_id?;
    let id = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    match event_type {
        "product.created" | "product.updated" | "product.deleted" | "price.changed"
        | "inventory.changed" => Some(index_product_job(tenant_id, id("product_id")?, version)),
        // Memberships still exist in Postgres, so the affected products can be found.
        "category.updated" | "category.moved" => Some(versioned(
            REINDEX_CATEGORY_JOB,
            json!({ "category_id": id("category_id")? }),
            tenant_id,
            version,
            DELAY,
        )),
        // A deleted category's memberships are gone; filterable parameters, markets and market
        // price lists shape every document: rebuild.
        "category.deleted" | "parameter.updated" | "parameter.deleted" | "market.created"
        | "market.updated" | "price_list.created" | "price_list.updated" => {
            Some(rebuild_job(tenant_id, version))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_names_use_the_full_tenant_uuid() {
        let t = Uuid::parse_str("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b").unwrap_or_default();
        assert_eq!(
            index_uid(t, "cs"),
            "t_0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b_cs"
        );
        assert_eq!(market_key("cz-b2b"), "cz_b2b");
    }

    #[test]
    fn jobs_carry_their_version_and_run_after_the_delay() {
        let (t, p) = (Uuid::now_v7(), Uuid::now_v7());
        let before = Utc::now();
        let job = index_product_job(t, p, 123);
        assert_eq!(job_version(&job.payload), Some(123));
        assert_eq!(job.payload["product_id"], json!(p));
        assert!(
            job.run_at
                .is_some_and(|r| r >= before + chrono::Duration::seconds(2))
        );
        assert_eq!(job.tenant_id, Some(t));
        assert!(job.idempotency_key.is_none());
        assert!(
            manual_rebuild_job(t, 5)
                .run_at
                .is_some_and(|r| r <= Utc::now())
        );
        assert_eq!(job_version(&json!({})), None);
    }

    #[test]
    fn events_map_to_search_jobs() {
        let (t, p) = (Uuid::now_v7(), Uuid::now_v7());
        let payload = json!({ "product_id": p, "variant_id": Uuid::now_v7() });
        for ty in ["product.updated", "price.changed", "inventory.changed"] {
            let job = job_for_event(Some(t), ty, &payload, 1);
            assert_eq!(job.map(|j| j.kind), Some(INDEX_PRODUCT_JOB), "{ty}");
        }
        let cat = json!({ "category_id": Uuid::now_v7() });
        assert_eq!(
            job_for_event(Some(t), "category.moved", &cat, 1).map(|j| j.kind),
            Some(REINDEX_CATEGORY_JOB)
        );
        for ty in ["category.deleted", "market.created", "parameter.updated"] {
            assert_eq!(
                job_for_event(Some(t), ty, &cat, 1).map(|j| j.kind),
                Some(REBUILD_JOB),
                "{ty}"
            );
        }
        assert!(job_for_event(Some(t), "coupon.created", &json!({}), 1).is_none());
        assert!(job_for_event(None, "product.updated", &payload, 1).is_none());
        assert!(job_for_event(Some(t), "product.updated", &json!({}), 1).is_none());
    }
}
