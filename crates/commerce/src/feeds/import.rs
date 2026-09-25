//! Feed imports (spec §10.8, A18, A21, A28).
//!
//! A run holds the feed file in the private bucket (a presigned upload, or a download of the
//! merchant's URL through [`SafeClient`]). The worker first analyzes it (dry run: counts,
//! missing fields, collisions, nothing written), then on request applies it:
//!
//! - items of one `ITEMGROUP_ID`/`g:item_group_id` become one product, its items the variants
//!   (SKU = item id); parameters that differ inside a group become options;
//! - categories from `CATEGORYTEXT`/`g:product_type`, parameters from `PARAM`, images downloaded
//!   through the SSRF-safe client into the media pipeline (re-encoded), prices into the
//!   market's price list with `imported` (no reduction claims without 30 days of history, no
//!   `compare_at`), stock when the feed states it;
//! - every created entity is recorded in `import_mappings`, so a re-import updates instead of
//!   duplicating (a retried job resumes); new products are `draft`, updated ones keep status;
//! - old product URLs become 301 redirects; a path that already redirects is reported and the
//!   first redirect wins.
//!
//! Each product is applied in its own transaction: one bad group is reported, not fatal.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use object_store::path::Path;
use object_store::signer::{HeaderValue, Method, SignedUrlOptions};
use object_store::{ObjectStoreExt, PutPayload};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::http::{Limits, SafeClient};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::parse::{self, MAX_IMAGES};
use super::{FeedItem, Source, slugify};
use crate::audit;
use crate::catalog::categories::{self, CategoryTranslation, NewCategory};
use crate::catalog::parameters::{self, ParameterInput, ParameterKind};
use crate::catalog::products::{
    self, OptionValue, ProductInput, ProductMedia, ProductOption, ProductStatus,
    ProductTranslation, VariantInput,
};
use crate::catalog::{I18n, ean_valid, sanitize_html};
use crate::inventory::{self, Adjustment};
use crate::markets::invalid;
use crate::media::{self, UploadTarget};
use crate::pricing::{self, PriceItem, PriceUpsert};
use crate::redirects::{self, RedirectInput};

pub const JOB: &str = "feeds.import";
/// Feeds are bigger than images: 100 MB and 2 minutes (A21's 20 MB / 10 s apply to images).
pub const MAX_FEED_BYTES: u64 = 100 * 1024 * 1024;
pub const FEED_LIMITS: Limits = Limits {
    max_bytes: MAX_FEED_BYTES,
    timeout: Duration::from_secs(120),
};
const MAX_LISTED: usize = 200;
const UPLOAD_CONTENT_TYPE: &str = "application/xml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Waiting for the upload (or the first analysis).
    Pending,
    Analyzing,
    /// Dry run done: read the report, then apply.
    Analyzed,
    Applying,
    Applied,
    Failed,
}

impl RunStatus {
    fn parse(s: &str) -> Self {
        match s {
            "analyzing" => Self::Analyzing,
            "analyzed" => Self::Analyzed,
            "applying" => Self::Applying,
            "applied" => Self::Applied,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewImport {
    pub source: Source,
    /// Prices go into this market's price list; names into its default locale.
    pub market_id: Uuid,
    /// Download the feed from this `http(s)` URL (public addresses only).
    pub url: Option<String>,
    /// Or upload a file of this many bytes (at most 100 MB) with the returned presigned PUT.
    pub upload_size: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ItemProblem {
    pub item_id: String,
    /// `invalid_item_id`, `missing_name`, `invalid_ean`, `currency_mismatch`, `sku_conflict`,
    /// `image_failed`, `product_failed`, ...
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Collision {
    /// `item_id` (repeated id, later items skipped), `redirect` (the old path already redirects:
    /// the first redirect wins), `slug` (a new slug got a suffix).
    pub kind: String,
    pub item_id: String,
    pub value: String,
}

/// Dry-run report, completed by the apply step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportReport {
    pub items: u32,
    /// Items that will not be imported (see `problems`).
    pub skipped_items: u32,
    pub products: u32,
    pub variants: u32,
    pub new_products: u32,
    pub updated_products: u32,
    pub categories: u32,
    pub new_categories: u32,
    pub parameters: u32,
    pub new_parameters: u32,
    pub images: u32,
    pub new_images: u32,
    pub redirects: u32,
    /// Field -> number of items without it (`price`, `url`, `image`, `ean`, `category`, ...).
    pub missing: BTreeMap<String, u32>,
    pub problems: Vec<ItemProblem>,
    pub collisions: Vec<Collision>,
    /// More problems or collisions than listed (200 each).
    pub truncated: bool,
}

impl ImportReport {
    fn problem(&mut self, item_id: &str, code: &str, detail: impl Into<String>) {
        if self.problems.len() < MAX_LISTED {
            self.problems.push(ItemProblem {
                item_id: item_id.into(),
                code: code.into(),
                detail: detail.into(),
            });
        } else {
            self.truncated = true;
        }
    }

    fn collision(&mut self, kind: &str, item_id: &str, value: &str) {
        if self.collisions.len() < MAX_LISTED {
            self.collisions.push(Collision {
                kind: kind.into(),
                item_id: item_id.into(),
                value: value.into(),
            });
        } else {
            self.truncated = true;
        }
    }

    fn missing(&mut self, field: &str) {
        *self.missing.entry(field.into()).or_default() += 1;
    }
}

/// Apply progress and outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Progress {
    pub total: u32,
    pub done: u32,
    pub created: u32,
    pub updated: u32,
    pub failed: u32,
    pub images_downloaded: u32,
    pub images_failed: u32,
    pub redirects_created: u32,
    pub redirects_skipped: u32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportRun {
    pub id: Uuid,
    pub source: Source,
    pub market_id: Uuid,
    pub url: Option<String>,
    pub status: RunStatus,
    pub report: Option<ImportReport>,
    pub progress: Progress,
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedImport {
    pub run: ImportRun,
    /// For uploads: PUT the file here, then call `analyze`.
    pub upload: Option<UploadTarget>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportRunList {
    pub items: Vec<ImportRun>,
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::Internal(e.to_string())
}

fn object_key(tenant_id: Uuid, id: Uuid) -> Path {
    Path::from(format!("imports/{tenant_id}/{id}.xml"))
}

fn job(tenant_id: Uuid, run_id: Uuid, step: &str) -> NewJob<'static> {
    let mut j = NewJob::new(JOB, json!({ "run_id": run_id, "step": step }));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 3;
    j.idempotency_key = Some(format!("{JOB}:{run_id}:{step}:{}", crate::id::new_id()));
    j
}

impl NewImport {
    pub fn validate(&self) -> Result<(), Error> {
        match (&self.url, self.upload_size) {
            (Some(url), None) => {
                let ok = url.len() <= 2000
                    && reqwest::Url::parse(url).is_ok_and(|u| {
                        matches!(u.scheme(), "http" | "https")
                            && u.host_str().is_some()
                            && u.username().is_empty()
                            && u.password().is_none()
                    });
                if !ok {
                    return Err(invalid(
                        "invalid_url",
                        "url must be an http(s) URL without credentials",
                    ));
                }
            }
            (None, Some(size)) if (1..=MAX_FEED_BYTES).contains(&size) => {}
            (None, Some(_)) => {
                return Err(invalid(
                    "file_too_large",
                    format!("upload_size must be 1-{MAX_FEED_BYTES} bytes"),
                ));
            }
            _ => {
                return Err(invalid(
                    "invalid_import",
                    "give exactly one of url and upload_size",
                ));
            }
        }
        Ok(())
    }
}

struct RunRow {
    id: Uuid,
    source: String,
    market_id: Uuid,
    url: Option<String>,
    object_key: String,
    status: String,
    report: Option<Value>,
    progress: Value,
    error: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    applied_at: Option<DateTime<Utc>>,
}

impl From<RunRow> for ImportRun {
    fn from(r: RunRow) -> Self {
        Self {
            id: r.id,
            source: Source::parse(&r.source),
            market_id: r.market_id,
            url: r.url,
            status: RunStatus::parse(&r.status),
            report: r.report.and_then(|v| serde_json::from_value(v).ok()),
            progress: serde_json::from_value(r.progress).unwrap_or_default(),
            error: r.error,
            created_by: r.created_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
            applied_at: r.applied_at,
        }
    }
}

async fn row(tx: &mut TenantTx, id: Uuid, lock: bool) -> Result<RunRow, Error> {
    let q = if lock {
        sqlx::query_as!(
            RunRow,
            "SELECT id, source, market_id, url, object_key, status, report, progress, error,
                    created_by, created_at, updated_at, applied_at
             FROM import_runs WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut **tx)
        .await?
    } else {
        sqlx::query_as!(
            RunRow,
            "SELECT id, source, market_id, url, object_key, status, report, progress, error,
                    created_by, created_at, updated_at, applied_at
             FROM import_runs WHERE id = $1",
            id
        )
        .fetch_optional(&mut **tx)
        .await?
    };
    q.ok_or(Error::NotFound)
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<ImportRun, Error> {
    Ok(row(tx, id, false).await?.into())
}

/// The 50 most recent runs.
pub async fn list(tx: &mut TenantTx) -> Result<ImportRunList, Error> {
    let items = sqlx::query_as!(
        RunRow,
        "SELECT id, source, market_id, url, object_key, status, report, progress, error,
                created_by, created_at, updated_at, applied_at
         FROM import_runs ORDER BY id DESC LIMIT 50"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(ImportRun::from)
    .collect();
    Ok(ImportRunList { items })
}

/// Creates a run: a URL run is analyzed right away; an upload run returns the upload target.
pub async fn create(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    input: &NewImport,
) -> Result<CreatedImport, Error> {
    input.validate()?;
    let tenant_id = tx.tenant_id();
    let id = crate::id::new_id();
    let key = object_key(tenant_id, id);
    let status = if input.url.is_some() {
        "analyzing"
    } else {
        "pending"
    };
    sqlx::query!(
        "INSERT INTO import_runs (id, tenant_id, source, market_id, url, object_key, status,
                                  created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        tenant_id,
        input.source.as_str(),
        input.market_id,
        input.url,
        key.as_ref(),
        status,
        actor
    )
    .execute(&mut **tx)
    .await
    .map_err(crate::catalog::db_error)?;
    let upload = if input.url.is_some() {
        queue::enqueue(&mut **tx, &job(tenant_id, id, "analyze")).await?;
        None
    } else {
        let options = SignedUrlOptions::new().with_signed_header(
            object_store::signer::HeaderName::from_static("content-type"),
            HeaderValue::from_static(UPLOAD_CONTENT_TYPE),
        );
        let url = storage
            .private_signer
            .signed_url_opts(Method::PUT, &key, media::UPLOAD_URL_TTL, &options)
            .await?;
        Some(UploadTarget {
            method: "PUT".into(),
            url: url.to_string(),
            headers: BTreeMap::from([("content-type".to_owned(), UPLOAD_CONTENT_TYPE.to_owned())]),
            expires_at: Utc::now()
                + chrono::Duration::from_std(media::UPLOAD_URL_TTL).map_err(internal)?,
        })
    };
    let run: ImportRun = row(tx, id, false).await?.into();
    audit::record(
        tx,
        actor,
        "import.created",
        "import_run",
        Some(&id.to_string()),
        &json!({ "source": input.source, "market_id": input.market_id, "url": input.url }),
    )
    .await?;
    Ok(CreatedImport { run, upload })
}

/// (Re)runs the dry run (after the upload, or to refresh the report before applying).
pub async fn analyze(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<ImportRun, Error> {
    let r = row(tx, id, true).await?;
    if !matches!(
        RunStatus::parse(&r.status),
        RunStatus::Pending | RunStatus::Analyzed | RunStatus::Failed
    ) {
        return Err(Error::Conflict {
            code: "import_busy",
            detail: format!("the import is {}", r.status),
        });
    }
    set_status(tx, id, "analyzing", None).await?;
    let j = job(tx.tenant_id(), id, "analyze");
    queue::enqueue(&mut **tx, &j).await?;
    audit::record(
        tx,
        actor,
        "import.analyze",
        "import_run",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

/// Applies an analyzed run.
pub async fn apply(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<ImportRun, Error> {
    let r = row(tx, id, true).await?;
    if RunStatus::parse(&r.status) != RunStatus::Analyzed {
        return Err(Error::Conflict {
            code: "import_not_analyzed",
            detail: "only an analyzed import can be applied; run the dry run first".into(),
        });
    }
    set_status(tx, id, "applying", None).await?;
    let j = job(tx.tenant_id(), id, "apply");
    queue::enqueue(&mut **tx, &j).await?;
    audit::record(
        tx,
        actor,
        "import.apply",
        "import_run",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

async fn set_status(
    tx: &mut TenantTx,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE import_runs SET status = $2, error = $3, updated_at = now(),
                applied_at = CASE WHEN $2 = 'applied' THEN now() ELSE applied_at END
         WHERE id = $1",
        id,
        status,
        error
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Worker

/// Everything the worker step needs about the run's market.
struct Target {
    locale: String,
    price_list: Option<(Uuid, String)>,
}

async fn target(tx: &mut TenantTx, market_id: Uuid) -> Result<Target, Error> {
    let r = sqlx::query!(
        r#"SELECT m.default_locale, m.price_list_id, pl.currency AS "currency?"
           FROM markets m LEFT JOIN price_lists pl ON pl.id = m.price_list_id
           WHERE m.id = $1"#,
        market_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Target {
        locale: r.default_locale,
        price_list: r.price_list_id.zip(r.currency),
    })
}

/// A run failed for a reason retrying cannot fix (bad file, blocked URL): record it.
async fn fail(db: &PgPool, tenant_id: Uuid, id: Uuid, message: &str) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let message: String = message.chars().take(2000).collect();
    set_status(&mut tx, id, "failed", Some(&message)).await?;
    tx.commit().await?;
    Ok(())
}

/// The worker step of a run (`analyze` or `apply`). Returns `Err` only for retryable failures
/// (database, storage); problems with the feed itself end the run as `failed`.
pub async fn run_step(
    db: &PgPool,
    storage: &Storage,
    fetch: &SafeClient,
    tenant_id: Uuid,
    id: Uuid,
    step: &str,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let r = row(&mut tx, id, false).await?;
    let expected = if step == "apply" {
        "applying"
    } else {
        "analyzing"
    };
    if r.status != expected {
        return Ok(()); // superseded or already done
    }
    let t = target(&mut tx, r.market_id).await?;
    tx.commit().await?;
    let source = Source::parse(&r.source);
    let key = Path::from(r.object_key.clone());

    // URL runs download once; the stored copy is what gets applied.
    if let Some(url) = &r.url
        && step == "analyze"
    {
        match fetch.get(url, FEED_LIMITS).await {
            Ok(f) => {
                storage.private.put(&key, PutPayload::from(f.bytes)).await?;
            }
            Err(e) => return fail(db, tenant_id, id, &format!("download failed: {e}")).await,
        }
    }
    let bytes = match storage.private.head(&key).await {
        Ok(meta) if meta.size > MAX_FEED_BYTES => {
            return fail(db, tenant_id, id, "the feed is larger than 100 MB").await;
        }
        Ok(meta) => storage.private.get_range(&key, 0..meta.size).await?,
        Err(object_store::Error::NotFound { .. }) => {
            return fail(db, tenant_id, id, "no feed file was uploaded").await;
        }
        Err(e) => return Err(e.into()),
    };
    let locale = t.locale.clone();
    let parsed = tokio::task::spawn_blocking(move || parse::read_all(&bytes, source, &locale))
        .await
        .map_err(internal)?;
    let items = match parsed {
        Ok(items) => items,
        Err(e) => return fail(db, tenant_id, id, &e.to_string()).await,
    };

    let mut tx = tenant_tx(db, tenant_id).await?;
    let (plan, mut report) = plan(&mut tx, source, &t, items).await?;
    tx.commit().await?;
    if step == "analyze" {
        let mut tx = tenant_tx(db, tenant_id).await?;
        sqlx::query!(
            "UPDATE import_runs SET status = 'analyzed', report = $2, error = NULL,
                    updated_at = now()
             WHERE id = $1 AND status = 'analyzing'",
            id,
            serde_json::to_value(&report).map_err(internal)?
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }

    let actor = format!("import:{id}");
    let mut progress = Progress {
        total: u32::try_from(plan.len()).unwrap_or(u32::MAX),
        ..Progress::default()
    };
    let ctx = ApplyCtx {
        db,
        storage,
        fetch,
        tenant_id,
        source,
        target: &t,
        actor: &actor,
    };
    for (i, group) in plan.iter().enumerate() {
        if let Err(e) = ctx.apply_group(group, &mut progress, &mut report).await {
            progress.failed += 1;
            report.problem(&group.items[0].item_id, "product_failed", e.to_string());
        }
        progress.done += 1;
        if i % 10 == 9 {
            save_progress(db, tenant_id, id, &progress, None).await?;
        }
    }
    save_progress(db, tenant_id, id, &progress, Some(&report)).await?;
    Ok(())
}

async fn save_progress(
    db: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    progress: &Progress,
    done: Option<&ImportReport>,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    sqlx::query!(
        "UPDATE import_runs SET progress = $2, updated_at = now(),
                report = coalesce($3, report),
                status = CASE WHEN $3::jsonb IS NULL THEN status ELSE 'applied' END,
                applied_at = CASE WHEN $3::jsonb IS NULL THEN applied_at ELSE now() END
         WHERE id = $1",
        id,
        serde_json::to_value(progress).map_err(internal)?,
        done.map(serde_json::to_value)
            .transpose()
            .map_err(internal)?
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Planning (dry run)

/// A product to import: its items (variants) and how it maps to existing data.
#[derive(Debug, Clone)]
struct Group {
    pub key: String,
    pub items: Vec<FeedItem>,
    /// The existing product this group updates.
    pub existing: Option<Uuid>,
}

fn sku_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic())
}

/// Validates items, groups them into products and fills the report (reads only).
async fn plan(
    tx: &mut TenantTx,
    source: Source,
    t: &Target,
    items: Vec<FeedItem>,
) -> Result<(Vec<Group>, ImportReport), Error> {
    let mut report = ImportReport {
        items: u32::try_from(items.len()).unwrap_or(u32::MAX),
        ..ImportReport::default()
    };
    let mut seen = HashSet::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for mut item in items {
        item.item_id = item.item_id.trim().to_owned();
        item.name = item.name.trim().to_owned();
        if !sku_ok(&item.item_id) {
            report.skipped_items += 1;
            report.problem(
                &item.item_id,
                "invalid_item_id",
                "the item id (SKU) must be 1-64 visible ASCII characters",
            );
            continue;
        }
        if !seen.insert(item.item_id.clone()) {
            report.skipped_items += 1;
            report.collision("item_id", &item.item_id, &item.item_id);
            continue;
        }
        if item.name.is_empty() {
            report.skipped_items += 1;
            report.missing("name");
            report.problem(&item.item_id, "missing_name", "the item has no name");
            continue;
        }
        item.name = item.name.chars().take(200).collect();
        if item.price_minor.is_none() {
            report.missing("price");
        }
        if let (Some(c), Some((_, list))) = (&item.currency, &t.price_list)
            && c != list
        {
            report.problem(
                &item.item_id,
                "currency_mismatch",
                format!("the price is in {c}, the market's price list in {list}"),
            );
            item.price_minor = None;
        }
        if item.url.is_none() {
            report.missing("url");
        }
        if item.images.is_empty() {
            report.missing("image");
        }
        if item.category.is_empty() {
            report.missing("category");
        }
        if item.brand.is_none() {
            report.missing("brand");
        }
        match &item.ean {
            None => report.missing("ean"),
            Some(e) if !ean_valid(e) => {
                report.problem(
                    &item.item_id,
                    "invalid_ean",
                    format!("{e} is not a valid EAN"),
                );
                item.ean = None;
            }
            Some(_) => {}
        }
        let key = item
            .group_id
            .clone()
            .unwrap_or_else(|| item.item_id.clone());
        match index.get(&key) {
            Some(&i) => groups[i].items.push(item),
            None => {
                index.insert(key.clone(), groups.len());
                groups.push(Group {
                    key,
                    items: vec![item],
                    existing: None,
                });
            }
        }
    }

    // Existing products: by mapping, else by SKU (a group spanning products is refused).
    let keys: Vec<String> = groups.iter().map(|g| g.key.clone()).collect();
    let mapped: HashMap<String, Uuid> = sqlx::query!(
        "SELECT m.external_id, m.entity_id FROM import_mappings m
         JOIN products p ON p.id = m.entity_id
         WHERE m.source = $1 AND m.entity_type = 'product' AND m.external_id = ANY($2)",
        source.as_str(),
        &keys
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.external_id, r.entity_id))
    .collect();
    let skus: Vec<String> = groups
        .iter()
        .flat_map(|g| g.items.iter().map(|i| i.item_id.clone()))
        .collect();
    let by_sku: HashMap<String, Uuid> = sqlx::query!(
        "SELECT sku, product_id FROM variants WHERE sku = ANY($1)",
        &skus
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.sku, r.product_id))
    .collect();
    let mut kept = Vec::with_capacity(groups.len());
    for mut g in groups {
        let owners: BTreeSet<Uuid> = g
            .items
            .iter()
            .filter_map(|i| by_sku.get(&i.item_id).copied())
            .collect();
        g.existing = mapped.get(&g.key).copied();
        let conflict = match g.existing {
            Some(p) => owners.iter().any(|o| *o != p),
            None => owners.len() > 1,
        };
        if conflict {
            report.skipped_items += u32::try_from(g.items.len()).unwrap_or(0);
            report.problem(
                &g.items[0].item_id,
                "sku_conflict",
                "the group's SKUs belong to other products",
            );
            continue;
        }
        g.existing = g.existing.or_else(|| owners.first().copied());
        kept.push(g);
    }
    let groups = kept;
    report.products = u32::try_from(groups.len()).unwrap_or(u32::MAX);
    report.variants = groups
        .iter()
        .map(|g| u32::try_from(g.items.len()).unwrap_or(0))
        .sum();
    report.updated_products =
        u32::try_from(groups.iter().filter(|g| g.existing.is_some()).count()).unwrap_or(0);
    report.new_products = report.products - report.updated_products;

    // Categories, parameters, images: distinct values vs existing mappings.
    let paths: BTreeSet<String> = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .flat_map(|i| (1..=i.category.len()).map(|n| i.category[..n].join("|")))
        .collect();
    let params: BTreeSet<String> = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .flat_map(|i| i.params.iter().map(|(n, _)| param_external(n)))
        .collect();
    let images: BTreeSet<String> = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .flat_map(|i| i.images.iter().cloned())
        .collect();
    let count_mapped = async |tx: &mut TenantTx, entity: &str, ids: &BTreeSet<String>| {
        let ids: Vec<String> = ids.iter().cloned().collect();
        sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM import_mappings
               WHERE source = $1 AND entity_type = $2 AND external_id = ANY($3)"#,
            source.as_str(),
            entity,
            &ids
        )
        .fetch_one(&mut **tx)
        .await
    };
    let n = |c: usize| u32::try_from(c).unwrap_or(u32::MAX);
    report.categories = n(paths.len());
    report.new_categories = report.categories
        - n(usize::try_from(count_mapped(tx, "category", &paths).await?).unwrap_or(0));
    report.parameters = n(params.len());
    report.new_parameters = report.parameters
        - n(usize::try_from(count_mapped(tx, "parameter", &params).await?).unwrap_or(0));
    report.images = n(images.len());
    report.new_images =
        report.images - n(usize::try_from(count_mapped(tx, "asset", &images).await?).unwrap_or(0));

    // Redirects: old paths already redirected (existing rows, or earlier in this feed).
    let mut from_paths = HashSet::new();
    let mut candidates: Vec<(String, String)> = Vec::new();
    for g in &groups {
        for i in &g.items {
            if let Some(p) = i.url.as_deref().and_then(old_path) {
                if from_paths.insert(p.clone()) {
                    candidates.push((i.item_id.clone(), p));
                } else {
                    report.collision("redirect", &i.item_id, &p);
                }
            }
        }
    }
    let wanted: Vec<String> = candidates.iter().map(|(_, p)| p.clone()).collect();
    let existing: HashSet<String> = sqlx::query_scalar!(
        "SELECT from_path FROM redirects WHERE from_path = ANY($1)",
        &wanted
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    for (item, p) in &candidates {
        if existing.contains(p) {
            report.collision("redirect", item, p);
        } else {
            report.redirects += 1;
        }
    }
    Ok((groups, report))
}

/// The redirect source for an old product URL: its path (query kept out), normalized.
pub fn old_path(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    let path = redirects::normalize_path(u.path());
    let ok = RedirectInput {
        from_path: path.clone(),
        to_path: "/p/x".into(),
        code: 301,
    }
    .validate()
    .is_ok();
    (ok && path != "/").then_some(path)
}

fn param_external(name: &str) -> String {
    name.trim().to_lowercase()
}

// ---------------------------------------------------------------------------------------
// Apply

struct ApplyCtx<'a> {
    db: &'a PgPool,
    storage: &'a Storage,
    fetch: &'a SafeClient,
    tenant_id: Uuid,
    source: Source,
    target: &'a Target,
    actor: &'a str,
}

async fn mapped(
    tx: &mut TenantTx,
    source: Source,
    entity: &str,
    external: &str,
) -> Result<Option<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT entity_id FROM import_mappings
         WHERE source = $1 AND entity_type = $2 AND external_id = $3",
        source.as_str(),
        entity,
        external
    )
    .fetch_optional(&mut **tx)
    .await?)
}

async fn map(
    tx: &mut TenantTx,
    source: Source,
    entity: &str,
    external: &str,
    id: Uuid,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO import_mappings (tenant_id, source, entity_type, external_id, entity_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id, source, entity_type, external_id)
         DO UPDATE SET entity_id = $5, updated_at = now()",
        tx.tenant_id(),
        source.as_str(),
        entity,
        external,
        id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A slug not yet used in `locale` by `table` (`product_translations` or
/// `category_translations`): `base`, `base-2`, ...
async fn free_slug(
    tx: &mut TenantTx,
    categories: bool,
    locale: &str,
    base: &str,
) -> Result<String, Error> {
    let base = if base.is_empty() { "item" } else { base };
    let pattern = format!("{base}%");
    let taken: HashSet<String> = if categories {
        sqlx::query_scalar!(
            "SELECT slug FROM category_translations WHERE locale = $1 AND slug LIKE $2",
            locale,
            pattern
        )
        .fetch_all(&mut **tx)
        .await?
    } else {
        sqlx::query_scalar!(
            "SELECT slug FROM product_translations WHERE locale = $1 AND slug LIKE $2",
            locale,
            pattern
        )
        .fetch_all(&mut **tx)
        .await?
    }
    .into_iter()
    .collect();
    Ok(std::iter::once(base.to_owned())
        .chain((2..10_000).map(|n| format!("{base}-{n}")))
        .find(|s| !taken.contains(s))
        .unwrap_or_else(|| format!("{base}-{}", crate::id::new_id().simple())))
}

/// Longest common word prefix of the names (the product name of a variant group).
fn common_name(items: &[FeedItem]) -> String {
    let first: Vec<&str> = items[0].name.split_whitespace().collect();
    let mut n = first.len();
    for i in &items[1..] {
        let words: Vec<&str> = i.name.split_whitespace().collect();
        n = n.min(first.iter().zip(&words).take_while(|(a, b)| a == b).count());
    }
    if n == 0 || items.len() == 1 {
        items[0].name.clone()
    } else {
        first[..n].join(" ")
    }
}

fn code(text: &str, fallback: &str) -> String {
    let c = slugify(text, 64);
    if c.is_empty() { fallback.to_owned() } else { c }
}

/// Options for a multi-item group: parameters present in every item whose values differ, as
/// long as they make every combination unique; else one `variant` option.
fn options(
    items: &[FeedItem],
    locale: &str,
) -> (Vec<ProductOption>, Vec<BTreeMap<String, String>>) {
    if items.len() < 2 {
        return (vec![], vec![BTreeMap::new(); items.len()]);
    }
    let value = |i: &FeedItem, name: &str| {
        i.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let differing: Vec<String> = items[0]
        .params
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| items.iter().all(|i| value(i, n).is_some()))
        .filter(|n| {
            let vals: BTreeSet<_> = items.iter().filter_map(|i| value(i, n)).collect();
            vals.len() > 1
        })
        .take(products::MAX_OPTIONS)
        .collect();
    let combos: BTreeSet<Vec<String>> = items
        .iter()
        .map(|i| differing.iter().filter_map(|n| value(i, n)).collect())
        .collect();
    let (names, labels): (Vec<String>, Vec<Vec<String>>) =
        if !differing.is_empty() && combos.len() == items.len() {
            (
                differing.clone(),
                items
                    .iter()
                    .map(|i| differing.iter().filter_map(|n| value(i, n)).collect())
                    .collect(),
            )
        } else {
            let name = match locale {
                "cs" => "Varianta",
                "sk" => "Variant",
                _ => "Variant",
            };
            // Item names tell variants apart when they differ; else the item ids do.
            let names: BTreeSet<&str> = items.iter().map(|i| i.name.as_str()).collect();
            let label = |i: &FeedItem| {
                if names.len() == items.len() {
                    i.name.clone()
                } else {
                    i.item_id.clone()
                }
            };
            (
                vec![name.to_owned()],
                items.iter().map(|i| vec![label(i)]).collect(),
            )
        };
    let mut opts = Vec::new();
    let mut per_item = vec![BTreeMap::new(); items.len()];
    let mut used_option_codes = HashSet::new();
    for (k, name) in names.iter().enumerate() {
        let mut option_code = code(name, "option");
        while !used_option_codes.insert(option_code.clone()) {
            option_code.push('x');
        }
        let mut values: Vec<OptionValue> = Vec::new();
        let mut code_of: HashMap<String, String> = HashMap::new();
        for (i, l) in labels.iter().enumerate() {
            let label = &l[k];
            let value_code = match code_of.get(label) {
                Some(c) => c.clone(),
                None => {
                    let base = code(label, "value");
                    let mut c = base.clone();
                    let mut n = 2;
                    while values.iter().any(|v| v.code == c) {
                        c = format!("{base}-{n}");
                        n += 1;
                    }
                    values.push(OptionValue {
                        code: c.clone(),
                        name_i18n: I18n::from([(
                            locale.to_owned(),
                            label.chars().take(100).collect(),
                        )]),
                    });
                    code_of.insert(label.clone(), c.clone());
                    c
                }
            };
            per_item[i].insert(option_code.clone(), value_code);
        }
        opts.push(ProductOption {
            code: option_code,
            name_i18n: I18n::from([(locale.to_owned(), name.chars().take(100).collect())]),
            values,
        });
    }
    (opts, per_item)
}

impl ApplyCtx<'_> {
    /// Image assets for the group's URLs (downloaded once per URL and tenant).
    async fn images(
        &self,
        group: &Group,
        progress: &mut Progress,
        report: &mut ImportReport,
    ) -> Result<HashMap<String, Uuid>, Error> {
        let mut out = HashMap::new();
        let urls: Vec<(&str, &str)> = group
            .items
            .iter()
            .flat_map(|i| {
                i.images
                    .iter()
                    .map(move |u| (u.as_str(), i.item_id.as_str()))
            })
            .collect();
        for (url, item_id) in urls {
            if out.contains_key(url) || out.len() >= MAX_IMAGES * 2 {
                continue;
            }
            let mut tx = tenant_tx(self.db, self.tenant_id).await?;
            if let Some(id) = mapped(&mut tx, self.source, "asset", url).await?
                && sqlx::query_scalar!("SELECT id FROM assets WHERE id = $1", id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .is_some()
            {
                out.insert(url.to_owned(), id);
                continue;
            }
            tx.commit().await?;
            let fetched = match self.fetch.get(url, Limits::IMAGE).await {
                Ok(f) => f,
                Err(e) => {
                    progress.images_failed += 1;
                    report.problem(item_id, "image_failed", format!("{url}: {e}"));
                    continue;
                }
            };
            let filename = fetched
                .url
                .path_segments()
                .and_then(|mut s| s.next_back())
                .map(str::to_owned);
            let mut tx = tenant_tx(self.db, self.tenant_id).await?;
            match media::ingest(
                &mut tx,
                self.storage,
                self.actor,
                filename.as_deref(),
                fetched.bytes,
            )
            .await
            {
                Ok(asset) => {
                    map(&mut tx, self.source, "asset", url, asset.id).await?;
                    tx.commit().await?;
                    progress.images_downloaded += 1;
                    out.insert(url.to_owned(), asset.id);
                }
                Err(e @ Error::Validation { .. }) => {
                    progress.images_failed += 1;
                    report.problem(item_id, "image_failed", format!("{url}: {e}"));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// The category of a path (creating missing levels), mapped by the joined path.
    async fn category(&self, tx: &mut TenantTx, path: &[String]) -> Result<Option<Uuid>, Error> {
        let mut parent: Option<Uuid> = None;
        for n in 1..=path.len().min(categories::MAX_DEPTH as usize) {
            let external = path[..n].join("|");
            let existing = match mapped(tx, self.source, "category", &external).await? {
                Some(id)
                    if sqlx::query_scalar!("SELECT id FROM categories WHERE id = $1", id)
                        .fetch_optional(&mut **tx)
                        .await?
                        .is_some() =>
                {
                    Some(id)
                }
                _ => None,
            };
            let id = match existing {
                Some(id) => id,
                None => {
                    let name: String = path[n - 1].chars().take(200).collect();
                    let slug =
                        free_slug(tx, true, &self.target.locale, &slugify(&name, 200)).await?;
                    let c = categories::create(
                        tx,
                        self.actor,
                        &NewCategory {
                            parent_id: parent,
                            image_asset_id: None,
                            translations: vec![CategoryTranslation {
                                locale: self.target.locale.clone(),
                                name,
                                slug,
                                description_html: String::new(),
                                seo_title: None,
                                seo_description: None,
                            }],
                        },
                    )
                    .await?;
                    map(tx, self.source, "category", &external, c.id).await?;
                    c.id
                }
            };
            parent = Some(id);
        }
        Ok(parent)
    }

    /// A text parameter for a `PARAM_NAME` (existing mapping, else an existing key, else new).
    async fn parameter(&self, tx: &mut TenantTx, name: &str) -> Result<Option<Uuid>, Error> {
        let external = param_external(name);
        if let Some(id) = mapped(tx, self.source, "parameter", &external).await?
            && let Some(kind) = sqlx::query_scalar!("SELECT kind FROM parameters WHERE id = $1", id)
                .fetch_optional(&mut **tx)
                .await?
        {
            return Ok((kind == "text").then_some(id));
        }
        let key = code(name, "param");
        let existing = sqlx::query!("SELECT id, kind FROM parameters WHERE key = $1", key)
            .fetch_optional(&mut **tx)
            .await?;
        let id = match existing {
            Some(p) if p.kind != "text" => return Ok(None),
            Some(p) => p.id,
            None => {
                parameters::create(
                    tx,
                    self.actor,
                    &ParameterInput {
                        key,
                        name_i18n: I18n::from([(
                            self.target.locale.clone(),
                            name.chars().take(200).collect(),
                        )]),
                        kind: ParameterKind::Text,
                        unit: None,
                        filterable: false,
                    },
                )
                .await?
                .id
            }
        };
        map(tx, self.source, "parameter", &external, id).await?;
        Ok(Some(id))
    }

    async fn apply_group(
        &self,
        group: &Group,
        progress: &mut Progress,
        report: &mut ImportReport,
    ) -> Result<(), Error> {
        let assets = self.images(group, progress, report).await?;
        let mut tx = tenant_tx(self.db, self.tenant_id).await?;
        let locale = self.target.locale.clone();
        let first = &group.items[0];
        let name = common_name(&group.items);
        let category = self.category(&mut tx, &first.category).await?;

        let (opts, per_item) = options(&group.items, &locale);
        let option_names: HashSet<String> = opts
            .iter()
            .filter_map(|o| o.name_i18n.get(&locale).cloned())
            .collect();
        // Parameters: same value in every item -> product level, else per variant.
        let mut param_values = Vec::new();
        let mut done = HashSet::new();
        for (pname, _) in group.items.iter().flat_map(|i| i.params.iter()) {
            if option_names.contains(pname) || !done.insert(pname.clone()) {
                continue;
            }
            let Some(pid) = self.parameter(&mut tx, pname).await? else {
                continue;
            };
            let values: Vec<Option<&String>> = group
                .items
                .iter()
                .map(|i| i.params.iter().find(|(n, _)| n == pname).map(|(_, v)| v))
                .collect();
            let uniform = values.iter().all(|v| *v == values[0]) && values[0].is_some();
            let text =
                |v: &str| json!({ locale.clone(): v.chars().take(1000).collect::<String>() });
            if uniform {
                if let Some(v) = values[0] {
                    param_values.push(products::ParameterValue {
                        parameter_id: pid,
                        variant_sku: None,
                        value: text(v),
                    });
                }
            } else {
                for (i, v) in values.iter().enumerate() {
                    if let Some(v) = v {
                        param_values.push(products::ParameterValue {
                            parameter_id: pid,
                            variant_sku: Some(group.items[i].item_id.clone()),
                            value: text(v),
                        });
                    }
                }
            }
        }

        // Media: every image once; an image of only one item of a group shows for that variant.
        let mut media: Vec<ProductMedia> = Vec::new();
        for i in &group.items {
            for url in &i.images {
                let Some(asset) = assets.get(url) else {
                    continue;
                };
                if media.iter().any(|m| m.asset_id == *asset) || media.len() >= 100 {
                    continue;
                }
                let owners = group
                    .items
                    .iter()
                    .filter(|x| x.images.contains(url))
                    .count();
                media.push(ProductMedia {
                    asset_id: *asset,
                    variant_sku: (group.items.len() > 1 && owners == 1).then(|| i.item_id.clone()),
                    alt_i18n: I18n::from([(locale.clone(), name.chars().take(200).collect())]),
                });
            }
        }

        let existing = match group.existing {
            Some(id) => products::get(&mut tx, id).await.ok(),
            None => None,
        };
        let variant_ids: HashMap<String, Uuid> = existing
            .as_ref()
            .map(|p| p.variants.iter().map(|v| (v.sku.clone(), v.id)).collect())
            .unwrap_or_default();
        let variants: Vec<VariantInput> = group
            .items
            .iter()
            .zip(&per_item)
            .map(|(i, opts)| VariantInput {
                id: variant_ids.get(&i.item_id).copied(),
                sku: i.item_id.clone(),
                ean: i.ean.clone(),
                option_values: opts.clone(),
                weight_g: None,
                is_default: false,
            })
            .collect();
        let description = sanitize_html(&first.description);
        let skus: HashSet<&str> = group.items.iter().map(|i| i.item_id.as_str()).collect();

        let input = match &existing {
            None => {
                let slug = free_slug(&mut tx, false, &locale, &slugify(&name, 200)).await?;
                if slug != slugify(&name, 200) {
                    report.collision("slug", &first.item_id, &slug);
                }
                ProductInput {
                    status: ProductStatus::Draft,
                    brand: first.brand.clone(),
                    gpsr: Default::default(),
                    unit_measure: None,
                    unit_quantity: None,
                    heureka_category: first.heureka_category.clone(),
                    google_category: first.google_category.clone(),
                    translations: vec![ProductTranslation {
                        locale: locale.clone(),
                        name: name.clone(),
                        slug,
                        description_html: description,
                        short_description: String::new(),
                        seo_title: None,
                        seo_description: None,
                    }],
                    options: opts,
                    variants,
                    category_ids: category.into_iter().collect(),
                    media,
                    parameters: param_values,
                    tax_categories: BTreeMap::new(),
                }
            }
            Some(p) => {
                // The feed owns what it states; everything else the merchant set is kept.
                let mut translations = p.translations.clone();
                match translations.iter_mut().find(|t| t.locale == locale) {
                    Some(t) => {
                        t.name = name.clone();
                        t.description_html = description;
                    }
                    None => {
                        let slug = free_slug(&mut tx, false, &locale, &slugify(&name, 200)).await?;
                        translations.push(ProductTranslation {
                            locale: locale.clone(),
                            name: name.clone(),
                            slug,
                            description_html: description,
                            short_description: String::new(),
                            seo_title: None,
                            seo_description: None,
                        });
                    }
                }
                let mut category_ids = p.category_ids.clone();
                if let Some(c) = category
                    && !category_ids.contains(&c)
                {
                    category_ids.push(c);
                }
                let imported_params: HashSet<Uuid> =
                    param_values.iter().map(|v| v.parameter_id).collect();
                let mut parameters = param_values;
                parameters.extend(p.parameters.iter().cloned().filter(|v| {
                    !imported_params.contains(&v.parameter_id)
                        && v.variant_sku.as_deref().is_none_or(|s| skus.contains(s))
                }));
                let mut all_media = media;
                for m in &p.media {
                    if !all_media.iter().any(|x| x.asset_id == m.asset_id)
                        && m.variant_sku.as_deref().is_none_or(|s| skus.contains(s))
                    {
                        all_media.push(m.clone());
                    }
                }
                ProductInput {
                    status: p.status,
                    brand: first.brand.clone().or(p.brand.clone()),
                    gpsr: p.gpsr.clone(),
                    unit_measure: p.unit_measure,
                    unit_quantity: p.unit_quantity,
                    heureka_category: first
                        .heureka_category
                        .clone()
                        .or(p.heureka_category.clone()),
                    google_category: first.google_category.clone().or(p.google_category.clone()),
                    translations,
                    options: opts,
                    variants,
                    category_ids,
                    media: all_media,
                    parameters,
                    tax_categories: p.tax_categories.clone(),
                }
            }
        };
        let product = match &existing {
            Some(p) => products::replace(&mut tx, self.actor, p.id, &input).await?,
            None => products::create(&mut tx, self.actor, &input).await?,
        };
        map(&mut tx, self.source, "product", &group.key, product.id).await?;
        let by_sku: HashMap<&str, Uuid> = product
            .variants
            .iter()
            .map(|v| (v.sku.as_str(), v.id))
            .collect();
        for i in &group.items {
            if let Some(v) = by_sku.get(i.item_id.as_str()) {
                map(&mut tx, self.source, "variant", &i.item_id, *v).await?;
            }
        }

        // Prices: base prices flagged as imported (A18); the feed's "was" price is never used.
        if let Some((list, _)) = self.target.price_list {
            let items: Vec<PriceItem> = group
                .items
                .iter()
                .filter_map(|i| {
                    Some(PriceItem {
                        variant_id: *by_sku.get(i.item_id.as_str())?,
                        amount_minor: i.price_minor?,
                        compare_at_minor: None,
                    })
                })
                .collect();
            if !items.is_empty() {
                pricing::upsert_prices(
                    &mut tx,
                    self.actor,
                    list,
                    &PriceUpsert {
                        reason: Default::default(),
                        imported: true,
                        items,
                    },
                )
                .await?;
            }
        }
        for i in &group.items {
            if let (Some(stock), Some(v)) = (i.stock, by_sku.get(i.item_id.as_str())) {
                inventory::adjust(
                    &mut tx,
                    self.actor,
                    *v,
                    &format!("{}:{}", self.actor, i.item_id),
                    &Adjustment {
                        delta: None,
                        on_hand: Some(stock),
                        note: Some("feed import".into()),
                    },
                )
                .await?;
            }
        }

        // Redirects from the old URLs to the product (first wins).
        let slug = product
            .translations
            .iter()
            .find(|t| t.locale == locale)
            .or(product.translations.first())
            .map(|t| t.slug.clone())
            .unwrap_or_default();
        let to_path = format!("/p/{slug}");
        let mut paths = BTreeSet::new();
        for i in &group.items {
            let Some(from) = i.url.as_deref().and_then(old_path) else {
                continue;
            };
            if from == to_path || !paths.insert(from.clone()) {
                continue;
            }
            let created = sqlx::query_scalar!(
                "INSERT INTO redirects (tenant_id, from_path, to_path, code)
                 VALUES ($1, $2, $3, 301)
                 ON CONFLICT (tenant_id, from_path) DO NOTHING RETURNING id",
                self.tenant_id,
                from,
                to_path
            )
            .fetch_optional(&mut *tx)
            .await?;
            if created.is_some() {
                progress.redirects_created += 1;
            } else {
                progress.redirects_skipped += 1;
            }
        }
        tx.commit().await?;
        if existing.is_some() {
            progress.updated += 1;
        } else {
            progress.created += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn item(id: &str, name: &str, params: &[(&str, &str)]) -> FeedItem {
        FeedItem {
            item_id: id.into(),
            name: name.into(),
            params: params
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect(),
            ..FeedItem::default()
        }
    }

    #[test]
    fn differing_params_become_options() {
        let items = vec![
            item(
                "A",
                "Tričko Basic černá M",
                &[
                    ("Barva", "černá"),
                    ("Velikost", "M"),
                    ("Materiál", "bavlna"),
                ],
            ),
            item(
                "B",
                "Tričko Basic černá L",
                &[
                    ("Barva", "černá"),
                    ("Velikost", "L"),
                    ("Materiál", "bavlna"),
                ],
            ),
            item(
                "C",
                "Tričko Basic bílá M",
                &[("Barva", "bílá"), ("Velikost", "M"), ("Materiál", "bavlna")],
            ),
        ];
        assert_eq!(common_name(&items), "Tričko Basic");
        let (opts, per_item) = options(&items, "cs");
        let codes: Vec<&str> = opts.iter().map(|o| o.code.as_str()).collect();
        assert_eq!(codes, ["barva", "velikost"]);
        assert_eq!(per_item[2]["barva"], "bila");
        assert_eq!(per_item[1]["velikost"], "l");
    }

    #[test]
    fn indistinct_variants_get_a_variant_option() {
        let items = vec![item("A", "Hrnek", &[]), item("B", "Hrnek", &[])];
        let (opts, per_item) = options(&items, "cs");
        assert_eq!(opts[0].name_i18n["cs"], "Varianta");
        assert_ne!(per_item[0]["varianta"], "");
        assert_eq!(opts[0].values.len(), 2, "codes are made unique");
        assert!(options(&items[..1], "cs").0.is_empty());
    }

    #[test]
    fn old_paths() {
        assert_eq!(
            old_path("https://old.example/produkt/tricko/?v=1").as_deref(),
            Some("/produkt/tricko")
        );
        assert_eq!(old_path("https://old.example/"), None);
        assert_eq!(old_path("not a url"), None);
    }

    #[test]
    fn imports_need_exactly_one_source() {
        let base = NewImport {
            source: Source::Heureka,
            market_id: Uuid::now_v7(),
            url: None,
            upload_size: None,
        };
        assert!(base.validate().is_err());
        let url = NewImport {
            url: Some("https://feeds.example/heureka.xml".into()),
            ..base.clone()
        };
        assert!(url.validate().is_ok());
        for bad in [
            "file:///etc/passwd",
            "https://u:p@x.example/f",
            "gopher://x",
        ] {
            let b = NewImport {
                url: Some(bad.into()),
                ..base.clone()
            };
            assert!(b.validate().is_err(), "{bad}");
        }
        let upload = NewImport {
            upload_size: Some(MAX_FEED_BYTES + 1),
            ..base
        };
        assert!(upload.validate().is_err());
    }
}
