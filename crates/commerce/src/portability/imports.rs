//! CSV imports of customers, historical orders and newsletter subscribers (spec §10.8, §11.5,
//! A20, A28). The same run lifecycle as feed imports:
//!
//! 1. `create` returns a presigned PUT for the CSV (at most 20 MB);
//! 2. `analyze` (worker) snapshots the upload to a key only the server writes, maps the
//!    columns, validates every row strictly and stores the dry-run report (row errors, counts,
//!    a preview); nothing is written;
//! 3. `apply` (worker) re-reads the snapshot and upserts the valid records in batches.
//!    Upserts are keyed by natural keys (customer email, order number, subscriber email), so a
//!    retried job or a re-imported file converges instead of duplicating.
//!
//! Imports have no side effects beyond their own rows: no email, outbox event, stock
//! movement, payment, invoice or analytics event. The CSV objects are deleted once applied.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use object_store::path::Path;
use object_store::signer::{HeaderValue, Method, SignedUrlOptions};
use object_store::{ObjectStoreExt, PutPayload};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::import_subscribers as subscribers;
use super::table::{self, Field, Row, Table};
use super::{import_customers as customers, import_orders as orders};
use crate::audit;
use crate::markets::invalid;
use crate::media::{self, UploadTarget};

pub const JOB: &str = "data.import";
const MAX_LISTED: usize = 200;
const PREVIEW: usize = 10;
const BATCH: usize = 200;
const UPLOAD_CONTENT_TYPE: &str = "text/csv";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Customers,
    /// Historical orders: archived, never processed (A28).
    Orders,
    /// Newsletter subscribers with consent evidence (A20).
    Subscribers,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Customers => "customers",
            Self::Orders => "orders",
            Self::Subscribers => "subscribers",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "orders" => Self::Orders,
            "subscribers" => Self::Subscribers,
            _ => Self::Customers,
        }
    }

    pub fn fields(self) -> &'static [Field] {
        match self {
            Self::Customers => customers::FIELDS,
            Self::Orders => orders::FIELDS,
            Self::Subscribers => subscribers::FIELDS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Waiting for the upload.
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

/// Our field name → the file's column header. Unmapped fields use the header of the same name.
pub type Mapping = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDataImport {
    pub kind: Kind,
    /// Customers without a `locale` get this market's default locale; subscribers join it.
    pub market_id: Uuid,
    /// Bytes of the CSV (at most 20 MB) to upload with the returned presigned PUT.
    pub upload_size: u64,
    #[serde(default)]
    pub mapping: Mapping,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalyzeInput {
    /// Replaces the column mapping before the dry run.
    pub mapping: Option<Mapping>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RowError {
    /// Line in the file (the header is line 1).
    pub line: u64,
    pub field: Option<String>,
    /// `missing`, `invalid_email`, `invalid_date`, `in_future`, `duplicate`,
    /// `conflicting_order`, ...
    pub code: String,
    pub detail: String,
}

/// Dry-run report (the apply step adds its outcome to `progress`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportReport {
    /// The file's column headers (for the mapping UI).
    pub headers: Vec<String>,
    /// Data rows in the file.
    pub rows: u32,
    /// Valid records that apply will import (orders: orders, not lines).
    pub records: u32,
    /// Rows with at least one error; they are skipped (a bad line drops its whole order).
    pub invalid_rows: u32,
    /// Records that do not exist yet / already exist (by email or order number).
    pub new: u32,
    pub existing: u32,
    /// Kind-specific counts (`with_address`, `lines`, `linked_to_customer`, `subscribed`,
    /// `pending_not_marketable`, `already_subscribed`, `kept_unsubscribed`).
    pub counts: BTreeMap<String, u32>,
    pub errors: Vec<RowError>,
    /// More errors than listed (200).
    pub truncated: bool,
    /// The first 10 valid records as they will be imported.
    pub preview: Vec<BTreeMap<String, String>>,
}

impl ImportReport {
    pub(super) fn error(
        &mut self,
        line: u64,
        field: Option<&str>,
        code: &str,
        detail: impl Into<String>,
    ) {
        if self.errors.len() < MAX_LISTED {
            self.errors.push(RowError {
                line,
                field: field.map(str::to_owned),
                code: code.to_owned(),
                detail: detail.into(),
            });
        } else {
            self.truncated = true;
        }
    }

    pub(super) fn count(&mut self, key: &str, n: usize) {
        *self.counts.entry(key.to_owned()).or_default() += u32::try_from(n).unwrap_or(u32::MAX);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Progress {
    pub total: u32,
    pub done: u32,
    pub created: u32,
    pub updated: u32,
    /// Subscribers: how many were subscribed, left pending, already subscribed or kept.
    pub outcomes: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataImport {
    pub id: Uuid,
    pub kind: Kind,
    pub market_id: Uuid,
    pub mapping: Mapping,
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
pub struct CreatedDataImport {
    pub import: DataImport,
    /// PUT the CSV here (with exactly these headers), then call `analyze`.
    pub upload: UploadTarget,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataImportList {
    pub items: Vec<DataImport>,
}

/// Validation context of a run.
#[derive(Debug, Clone)]
pub struct Defaults {
    pub market_id: Uuid,
    pub locale: String,
    pub now: DateTime<Utc>,
}

impl Defaults {
    #[cfg(test)]
    pub fn test() -> Self {
        Self {
            market_id: Uuid::nil(),
            locale: "cs".into(),
            now: "2026-09-25T12:00:00Z".parse().unwrap_or_default(),
        }
    }
}

/// Collects a row's problems instead of stopping at the first.
pub(super) struct Check<'a> {
    row: &'a Row,
    report: &'a mut ImportReport,
    pub ok: bool,
}

impl<'a> Check<'a> {
    pub fn new(row: &'a Row, report: &'a mut ImportReport) -> Self {
        Self {
            row,
            report,
            ok: true,
        }
    }

    /// Validates the optional cell `name` with `f`.
    pub fn opt<T>(&mut self, name: &str, f: impl Fn(&str) -> table::CellResult<T>) -> Option<T> {
        let v = self.row.get(name)?;
        match f(v) {
            Ok(v) => Some(v),
            Err((code, detail)) => {
                self.fail(Some(name), code, detail);
                None
            }
        }
    }

    /// Like [`Self::opt`], and a missing cell is an error.
    pub fn req<T>(&mut self, name: &str, f: impl Fn(&str) -> table::CellResult<T>) -> Option<T> {
        if self.row.get(name).is_none() {
            self.fail(Some(name), "missing", format!("{name} is required"));
            return None;
        }
        self.opt(name, f)
    }

    pub fn fail(&mut self, field: Option<&str>, code: &str, detail: impl Into<String>) {
        self.ok = false;
        self.report.error(self.row.line, field, code, detail);
    }

    pub fn has(&self, name: &str) -> bool {
        self.row.get(name).is_some()
    }
}

/// The valid records of a file.
enum Records {
    Customers(Vec<customers::Record>),
    Orders(Vec<orders::Record>),
    Subscribers(Vec<subscribers::Record>),
}

impl Records {
    fn len(&self) -> usize {
        match self {
            Self::Customers(r) => r.len(),
            Self::Orders(r) => r.len(),
            Self::Subscribers(r) => r.len(),
        }
    }

    fn preview(&self) -> Vec<BTreeMap<String, String>> {
        match self {
            Self::Customers(r) => r
                .iter()
                .take(PREVIEW)
                .map(customers::Record::preview)
                .collect(),
            Self::Orders(r) => r
                .iter()
                .take(PREVIEW)
                .map(orders::Record::preview)
                .collect(),
            Self::Subscribers(r) => r
                .iter()
                .take(PREVIEW)
                .map(subscribers::Record::preview)
                .collect(),
        }
    }
}

/// Parses and validates a file (pure; runs on a blocking thread).
fn validate(
    kind: Kind,
    bytes: &[u8],
    mapping: &Mapping,
    d: &Defaults,
) -> Result<(Records, ImportReport), table::FileError> {
    let Table { headers, rows } = table::read(bytes, kind.fields(), mapping)?;
    let mut report = ImportReport {
        headers,
        rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
        ..ImportReport::default()
    };
    let records = match kind {
        Kind::Customers => Records::Customers(customers::validate(&rows, d, &mut report)),
        Kind::Orders => Records::Orders(orders::validate(&rows, d, &mut report)),
        Kind::Subscribers => Records::Subscribers(subscribers::validate(&rows, d, &mut report)),
    };
    report.records = u32::try_from(records.len()).unwrap_or(u32::MAX);
    report.preview = records.preview();
    Ok((records, report))
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::Internal(e.to_string())
}

fn upload_key(tenant_id: Uuid, id: Uuid) -> Path {
    Path::from(format!("data-import-uploads/{tenant_id}/{id}.csv"))
}

fn object_key(tenant_id: Uuid, id: Uuid) -> Path {
    Path::from(format!("data-imports/{tenant_id}/{id}.csv"))
}

fn job(tenant_id: Uuid, id: Uuid, step: &str) -> NewJob<'static> {
    let mut j = NewJob::new(JOB, json!({ "import_id": id, "step": step }));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 3;
    j.idempotency_key = Some(format!("{JOB}:{id}:{step}:{}", crate::id::new_id()));
    j
}

fn check_mapping(kind: Kind, mapping: &Mapping) -> Result<(), Error> {
    for (field, header) in mapping {
        if !kind.fields().iter().any(|f| f.name == field) {
            return Err(invalid(
                "invalid_mapping",
                format!("{field:?} is not a {} field", kind.as_str()),
            ));
        }
        if header.trim().is_empty() || header.chars().count() > 200 {
            return Err(invalid(
                "invalid_mapping",
                format!("the column for {field} must be 1-200 characters"),
            ));
        }
    }
    Ok(())
}

struct RunRow {
    id: Uuid,
    kind: String,
    market_id: Uuid,
    mapping: Value,
    status: String,
    report: Option<Value>,
    progress: Value,
    error: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    applied_at: Option<DateTime<Utc>>,
}

impl From<RunRow> for DataImport {
    fn from(r: RunRow) -> Self {
        Self {
            id: r.id,
            kind: Kind::parse(&r.kind),
            market_id: r.market_id,
            mapping: serde_json::from_value(r.mapping).unwrap_or_default(),
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

async fn row(tx: &mut TenantTx, id: Uuid) -> Result<RunRow, Error> {
    sqlx::query_as!(
        RunRow,
        "SELECT id, kind, market_id, mapping, status, report, progress, error,
                created_by, created_at, updated_at, applied_at
         FROM data_imports WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<DataImport, Error> {
    let r = sqlx::query_as!(
        RunRow,
        "SELECT id, kind, market_id, mapping, status, report, progress, error,
                created_by, created_at, updated_at, applied_at
         FROM data_imports WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(r.into())
}

/// The 50 most recent runs.
pub async fn list(tx: &mut TenantTx) -> Result<DataImportList, Error> {
    let items = sqlx::query_as!(
        RunRow,
        "SELECT id, kind, market_id, mapping, status, report, progress, error,
                created_by, created_at, updated_at, applied_at
         FROM data_imports ORDER BY id DESC LIMIT 50"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(DataImport::from)
    .collect();
    Ok(DataImportList { items })
}

/// Creates a run and the presigned upload target for its CSV.
pub async fn create(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    input: &NewDataImport,
) -> Result<CreatedDataImport, Error> {
    if !(1..=table::MAX_BYTES).contains(&input.upload_size) {
        return Err(invalid(
            "file_too_large",
            format!("upload_size must be 1-{} bytes", table::MAX_BYTES),
        ));
    }
    check_mapping(input.kind, &input.mapping)?;
    let tenant_id = tx.tenant_id();
    let id = crate::id::new_id();
    let key = object_key(tenant_id, id);
    sqlx::query!(
        "INSERT INTO data_imports (id, tenant_id, kind, market_id, mapping, object_key, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        tenant_id,
        input.kind.as_str(),
        input.market_id,
        serde_json::to_value(&input.mapping).map_err(internal)?,
        key.as_ref(),
        actor
    )
    .execute(&mut **tx)
    .await
    .map_err(crate::catalog::db_error)?;
    let options = SignedUrlOptions::new().with_signed_header(
        object_store::signer::HeaderName::from_static("content-type"),
        HeaderValue::from_static(UPLOAD_CONTENT_TYPE),
    );
    let url = storage
        .private_signer
        .signed_url_opts(
            Method::PUT,
            &upload_key(tenant_id, id),
            media::UPLOAD_URL_TTL,
            &options,
        )
        .await?;
    let upload = UploadTarget {
        method: "PUT".into(),
        url: url.to_string(),
        headers: BTreeMap::from([("content-type".to_owned(), UPLOAD_CONTENT_TYPE.to_owned())]),
        expires_at: Utc::now()
            + chrono::Duration::from_std(media::UPLOAD_URL_TTL).map_err(internal)?,
    };
    audit::record(
        tx,
        actor,
        "data_import.created",
        "data_import",
        Some(&id.to_string()),
        &json!({ "kind": input.kind, "market_id": input.market_id }),
    )
    .await?;
    Ok(CreatedDataImport {
        import: get(tx, id).await?,
        upload,
    })
}

/// (Re)runs the dry run, optionally with a new column mapping.
pub async fn analyze(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &AnalyzeInput,
) -> Result<DataImport, Error> {
    let r = row(tx, id).await?;
    if !matches!(
        RunStatus::parse(&r.status),
        RunStatus::Pending | RunStatus::Analyzed | RunStatus::Failed
    ) {
        return Err(Error::Conflict {
            code: "import_busy",
            detail: format!("the import is {}", r.status),
        });
    }
    if let Some(mapping) = &input.mapping {
        check_mapping(Kind::parse(&r.kind), mapping)?;
    }
    sqlx::query!(
        "UPDATE data_imports SET status = 'analyzing', error = NULL, updated_at = now(),
                mapping = coalesce($2, mapping)
         WHERE id = $1",
        id,
        input
            .mapping
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(internal)?
    )
    .execute(&mut **tx)
    .await?;
    let j = job(tx.tenant_id(), id, "analyze");
    queue::enqueue(&mut **tx, &j).await?;
    audit::record(
        tx,
        actor,
        "data_import.analyze",
        "data_import",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

/// Applies an analyzed run.
pub async fn apply(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<DataImport, Error> {
    let r = row(tx, id).await?;
    if RunStatus::parse(&r.status) != RunStatus::Analyzed {
        return Err(Error::Conflict {
            code: "import_not_analyzed",
            detail: "only an analyzed import can be applied; run the dry run first".into(),
        });
    }
    sqlx::query!(
        "UPDATE data_imports SET status = 'applying', updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    let j = job(tx.tenant_id(), id, "apply");
    queue::enqueue(&mut **tx, &j).await?;
    audit::record(
        tx,
        actor,
        "data_import.apply",
        "data_import",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

/// Ends a run as failed (a bad file, or the job's last attempt failed).
pub async fn fail(db: &PgPool, tenant_id: Uuid, id: Uuid, message: &str) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let message: String = message.chars().take(2000).collect();
    sqlx::query!(
        "UPDATE data_imports SET status = 'failed', error = $2, updated_at = now() WHERE id = $1",
        id,
        message
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Reads an object of at most [`table::MAX_BYTES`]; `Ok(None)` when missing or too large.
async fn read_object(storage: &Storage, key: &Path) -> Result<Option<Vec<u8>>, Error> {
    match storage.private.head(key).await {
        Ok(meta) if meta.size > table::MAX_BYTES => Ok(None),
        Ok(meta) => Ok(Some(
            storage.private.get_range(key, 0..meta.size).await?.to_vec(),
        )),
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The worker step (`analyze` or `apply`). `Err` only for retryable failures; problems with
/// the file end the run as `failed`.
pub async fn run_step(
    db: &PgPool,
    storage: &Storage,
    tenant_id: Uuid,
    id: Uuid,
    step: &str,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let r = get(&mut tx, id).await?;
    let expected = if step == "apply" {
        RunStatus::Applying
    } else {
        RunStatus::Analyzing
    };
    if r.status != expected {
        return Ok(()); // superseded or already done
    }
    let locale = sqlx::query_scalar!(
        "SELECT default_locale FROM markets WHERE id = $1",
        r.market_id
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let d = Defaults {
        market_id: r.market_id,
        locale,
        now: Utc::now(),
    };
    let key = object_key(tenant_id, id);
    if step == "analyze" {
        let Some(bytes) = read_object(storage, &upload_key(tenant_id, id)).await? else {
            return fail(db, tenant_id, id, "no CSV of at most 20 MB was uploaded").await;
        };
        storage.private.put(&key, PutPayload::from(bytes)).await?;
    }
    let Some(bytes) = read_object(storage, &key).await? else {
        return fail(
            db,
            tenant_id,
            id,
            "the uploaded CSV is gone; upload it again",
        )
        .await;
    };
    let (kind, mapping, d2) = (r.kind, r.mapping.clone(), d.clone());
    let parsed = tokio::task::spawn_blocking(move || validate(kind, &bytes, &mapping, &d2))
        .await
        .map_err(internal)?;
    let (records, mut report) = match parsed {
        Ok(v) => v,
        Err(e) => return fail(db, tenant_id, id, &e.0).await,
    };

    if step == "analyze" {
        let mut tx = tenant_tx(db, tenant_id).await?;
        match &records {
            Records::Customers(v) => customers::classify(&mut tx, v, &mut report).await?,
            Records::Orders(v) => orders::classify(&mut tx, v, &mut report).await?,
            Records::Subscribers(v) => subscribers::classify(&mut tx, v, &mut report).await?,
        }
        sqlx::query!(
            "UPDATE data_imports SET status = 'analyzed', report = $2, error = NULL,
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

    let mut progress = Progress {
        total: report.records,
        ..Progress::default()
    };
    let n = records.len();
    let mut start = 0;
    while start < n {
        let end = (start + BATCH).min(n);
        let mut tx = tenant_tx(db, tenant_id).await?;
        for i in start..end {
            let created = match &records {
                Records::Customers(v) => customers::apply(&mut tx, &v[i], &d).await?,
                Records::Orders(v) => orders::apply(&mut tx, &v[i], id).await?,
                Records::Subscribers(v) => {
                    let (created, outcome) = subscribers::apply(&mut tx, &v[i], &d, id).await?;
                    *progress
                        .outcomes
                        .entry(outcome.key().to_owned())
                        .or_default() += 1;
                    created
                }
            };
            if created {
                progress.created += 1;
            } else {
                progress.updated += 1;
            }
            progress.done += 1;
        }
        let last = end == n;
        sqlx::query!(
            "UPDATE data_imports SET progress = $2, updated_at = now(),
                    status = CASE WHEN $3 THEN 'applied' ELSE status END,
                    applied_at = CASE WHEN $3 THEN now() ELSE applied_at END
             WHERE id = $1",
            id,
            serde_json::to_value(&progress).map_err(internal)?,
            last
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        start = end;
    }
    if n == 0 {
        let mut tx = tenant_tx(db, tenant_id).await?;
        sqlx::query!(
            "UPDATE data_imports SET status = 'applied', applied_at = now(), updated_at = now(),
                    progress = $2
             WHERE id = $1",
            id,
            serde_json::to_value(&progress).map_err(internal)?
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    // The file holds personal data: once applied it is not kept.
    for k in [upload_key(tenant_id, id), key] {
        match storage.private.delete(&k).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
            Err(e) => tracing::warn!(import = %id, error = %e, "import file not deleted"),
        }
    }
    Ok(())
}
