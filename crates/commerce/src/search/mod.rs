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

use chrono::{DateTime, Utc};
use platform::queue::NewJob;
use serde_json::{Value, json};
use uuid::Uuid;

pub use meili::{Meili, MeiliError};

/// (Re)indexes one product: `{"product_id"}`. The job id is the indexing version.
pub const INDEX_PRODUCT_JOB: &str = "search.index_product";
/// Reindexes the products of a changed category: `{"category_id"}`.
pub const REINDEX_CATEGORY_JOB: &str = "search.reindex_category";
/// Rebuilds all indexes of a tenant into new indexes and swaps them in: `{}`.
pub const REBUILD_JOB: &str = "search.rebuild";

/// Bumped whenever [`index::settings`] changes; indexes with an older version get the new
/// settings applied by the next indexing job.
pub const SETTINGS_VERSION: i32 = 1;

/// Meilisearch outages can last a while; indexing jobs keep retrying (capped backoff).
const JOB_ATTEMPTS: i32 = 25;
/// Changes to one product within this window are indexed by one job.
const DEBOUNCE: Duration = Duration::from_secs(2);
/// Rebuild requests (settings-relevant catalog changes) are coalesced over this window.
const REBUILD_DEBOUNCE: Duration = Duration::from_secs(10);

/// The live index of a tenant + locale (A27: full tenant UUID).
pub fn index_uid(tenant_id: Uuid, locale: &str) -> String {
    format!("t_{tenant_id}_{locale}")
}

/// Market code as a document key (`price.<key>`). Codes are `[a-z0-9-]`, so mapping `-` to
/// `_` is injective and yields a plain Meilisearch attribute name.
pub fn market_key(code: &str) -> String {
    code.replace('-', "_")
}

/// A job whose idempotency key is shared by all requests in the same `window`, running only
/// after the window closed (+ one window of slack for slow dispatch transactions), so every
/// change inside the window is visible to it. ponytail: time buckets; a per-key "pending"
/// flag in the queue would remove the fixed delay.
fn debounced(
    kind: &'static str,
    key: String,
    payload: Value,
    tenant_id: Uuid,
    now: DateTime<Utc>,
    window: Duration,
) -> NewJob<'static> {
    let w = i64::try_from(window.as_millis()).unwrap_or(i64::MAX).max(1);
    let bucket = now.timestamp_millis().div_euclid(w);
    let run_at = DateTime::<Utc>::from_timestamp_millis((bucket + 2) * w);
    let mut job = NewJob::new(kind, payload);
    job.tenant_id = Some(tenant_id);
    job.run_at = run_at;
    job.max_attempts = JOB_ATTEMPTS;
    job.idempotency_key = Some(format!("{key}:{bucket}"));
    job
}

pub fn index_product_job(tenant_id: Uuid, product_id: Uuid, now: DateTime<Utc>) -> NewJob<'static> {
    debounced(
        INDEX_PRODUCT_JOB,
        format!("search:product:{tenant_id}:{product_id}"),
        json!({ "product_id": product_id }),
        tenant_id,
        now,
        DEBOUNCE,
    )
}

pub fn rebuild_job(tenant_id: Uuid, now: DateTime<Utc>) -> NewJob<'static> {
    debounced(
        REBUILD_JOB,
        format!("search:rebuild:{tenant_id}"),
        json!({}),
        tenant_id,
        now,
        REBUILD_DEBOUNCE,
    )
}

/// A rebuild requested by staff: runs now; requests in the same window share the job.
pub fn manual_rebuild_job(tenant_id: Uuid, now: DateTime<Utc>) -> NewJob<'static> {
    let mut job = rebuild_job(tenant_id, now);
    job.run_at = None;
    job.idempotency_key = job.idempotency_key.map(|k| format!("{k}:manual"));
    job
}

/// The search job an outbox event triggers, if any (the dispatcher enqueues it next to the
/// generic subscribers).
pub fn job_for_event(
    tenant_id: Option<Uuid>,
    event_type: &str,
    payload: &Value,
    now: DateTime<Utc>,
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
        | "inventory.changed" => Some(index_product_job(tenant_id, id("product_id")?, now)),
        "category.updated" | "category.moved" | "category.deleted" => {
            let category_id = id("category_id")?;
            Some(debounced(
                REINDEX_CATEGORY_JOB,
                format!("search:category:{tenant_id}:{category_id}"),
                json!({ "category_id": category_id }),
                tenant_id,
                now,
                DEBOUNCE,
            ))
        }
        // Filterable parameters, markets and market price lists shape every document.
        "parameter.updated" | "parameter.deleted" | "market.created" | "market.updated"
        | "price_list.created" | "price_list.updated" => Some(rebuild_job(tenant_id, now)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(ms: i64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_800_000_000_000 + ms)
            .single()
            .unwrap_or_default()
    }

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
    fn product_events_in_one_window_share_a_job_that_runs_after_it() {
        let (t, p) = (Uuid::now_v7(), Uuid::now_v7());
        let a = index_product_job(t, p, at(0));
        let b = index_product_job(t, p, at(1_999));
        let c = index_product_job(t, p, at(2_000));
        assert_eq!(a.idempotency_key, b.idempotency_key);
        assert_ne!(a.idempotency_key, c.idempotency_key);
        assert!(a.run_at.is_some_and(|r| r >= at(4_000)));
        assert_eq!(a.tenant_id, Some(t));
    }

    #[test]
    fn events_map_to_search_jobs() {
        let (t, p) = (Uuid::now_v7(), Uuid::now_v7());
        let payload = json!({ "product_id": p, "variant_id": Uuid::now_v7() });
        for ty in ["product.updated", "price.changed", "inventory.changed"] {
            let job = job_for_event(Some(t), ty, &payload, at(0));
            assert_eq!(job.map(|j| j.kind), Some(INDEX_PRODUCT_JOB), "{ty}");
        }
        let cat = json!({ "category_id": Uuid::now_v7() });
        assert_eq!(
            job_for_event(Some(t), "category.moved", &cat, at(0)).map(|j| j.kind),
            Some(REINDEX_CATEGORY_JOB)
        );
        assert_eq!(
            job_for_event(Some(t), "market.created", &json!({}), at(0)).map(|j| j.kind),
            Some(REBUILD_JOB)
        );
        assert!(job_for_event(Some(t), "coupon.created", &json!({}), at(0)).is_none());
        assert!(job_for_event(None, "product.updated", &payload, at(0)).is_none());
        assert!(job_for_event(Some(t), "product.updated", &json!({}), at(0)).is_none());
    }
}
