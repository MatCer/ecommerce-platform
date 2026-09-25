//! Full tenant data export (spec §10.8, A21): a zip with one JSON Lines file per tenant table
//! plus `assets-manifest.jsonl` (every ready image with its public variant URLs), built by a
//! worker job and stored in the private bucket; staff download it through a 5-minute
//! presigned URL.
//!
//! Tables are discovered from the catalog, so new modules are exported without changes here:
//! every `public` table with a `tenant_id` column that the runtime role can read **and that
//! has forced row-level security** (anything else is skipped, fail closed). Rows are read in
//! the tenant's transaction, so RLS decides what is exported. Secrets never leave: every
//! `bytea` column (token hashes, IP hashes, ciphertexts, keys) and `password_hash` are
//! removed, mail bodies are dropped, and capability/session tables are skipped entirely.

use std::io::{Seek, SeekFrom, Write};

use chrono::{DateTime, Utc};
use object_store::buffered::BufWriter;
use object_store::path::Path;
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;
use zip::write::SimpleFileOptions;

use crate::audit;
use crate::media::AssetVariant;

pub const JOB: &str = "data.export";
/// Rows fetched from a table cursor at a time.
const FETCH: i64 = 1000;

/// Tables holding live credentials or rate-limit bookkeeping only: never exported.
pub const SKIPPED_TABLES: &[&str] = &[
    "checkout_handoffs",
    "customer_auth_attempts",
    "customer_magic_links",
    "customer_sessions",
    "data_exports",
    "idempotency_keys",
    "order_tokens",
    "withdrawal_tokens",
];

/// Columns removed besides every `bytea` column (and the bodies and unsubscribe URLs of
/// `email_messages`, which hold live sign-in links and subscriber capability tokens).
const SKIPPED_COLUMNS: &[&str] = &["password_hash"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    Pending,
    Running,
    Ready,
    Failed,
}

impl ExportStatus {
    fn parse(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataExport {
    pub id: Uuid,
    pub status: ExportStatus,
    pub size_bytes: Option<i64>,
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataExportList {
    pub items: Vec<DataExport>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExportDownload {
    /// Presigned GET, valid 5 minutes (A21).
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

struct Row {
    id: Uuid,
    status: String,
    size_bytes: Option<i64>,
    error: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

impl From<Row> for DataExport {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            status: ExportStatus::parse(&r.status),
            size_bytes: r.size_bytes,
            error: r.error,
            created_by: r.created_by,
            created_at: r.created_at,
            completed_at: r.completed_at,
        }
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::Internal(e.to_string())
}

fn object_key(tenant_id: Uuid, id: Uuid) -> String {
    format!("exports/{tenant_id}/{id}.zip")
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<DataExport, Error> {
    Ok(sqlx::query_as!(
        Row,
        "SELECT id, status, size_bytes, error, created_by, created_at, completed_at
         FROM data_exports WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .into())
}

/// The 20 most recent exports.
pub async fn list(tx: &mut TenantTx) -> Result<DataExportList, Error> {
    let items = sqlx::query_as!(
        Row,
        "SELECT id, status, size_bytes, error, created_by, created_at, completed_at
         FROM data_exports ORDER BY id DESC LIMIT 20"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(DataExport::from)
    .collect();
    Ok(DataExportList { items })
}

/// Starts an export; one at a time per tenant.
pub async fn create(tx: &mut TenantTx, actor: &str) -> Result<DataExport, Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('data_export:' || $1::text, 0))",
        tx.tenant_id().to_string()
    )
    .fetch_one(&mut **tx)
    .await?;
    let busy = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM data_exports WHERE status IN ('pending', 'running')"#
    )
    .fetch_one(&mut **tx)
    .await?;
    if busy > 0 {
        return Err(Error::Conflict {
            code: "export_busy",
            detail: "an export is already being prepared".into(),
        });
    }
    let id = crate::id::new_id();
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "INSERT INTO data_exports (id, tenant_id, object_key, created_by) VALUES ($1, $2, $3, $4)",
        id,
        tenant_id,
        object_key(tenant_id, id),
        actor
    )
    .execute(&mut **tx)
    .await?;
    let mut j = NewJob::new(JOB, json!({ "export_id": id }));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 3;
    j.idempotency_key = Some(format!("{JOB}:{id}"));
    queue::enqueue(&mut **tx, &j).await?;
    audit::record(
        tx,
        actor,
        "data_export.created",
        "data_export",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

/// A 5-minute download link of a ready export (audited: it is the whole shop's data).
pub async fn download(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    id: Uuid,
) -> Result<ExportDownload, Error> {
    let key = sqlx::query_scalar!(
        "SELECT object_key FROM data_exports WHERE id = $1 AND status = 'ready'",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    audit::record(
        tx,
        actor,
        "data_export.downloaded",
        "data_export",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    Ok(ExportDownload {
        url: crate::documents::download_url(storage, &key, "export.zip").await?,
        expires_at: Utc::now() + chrono::Duration::seconds(300),
    })
}

pub async fn fail(db: &PgPool, tenant_id: Uuid, id: Uuid, message: &str) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let message: String = message.chars().take(2000).collect();
    sqlx::query!(
        "UPDATE data_exports SET status = 'failed', error = $2, updated_at = now()
         WHERE id = $1 AND status <> 'ready'",
        id,
        message
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// A table to export and the columns to drop from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportTable {
    pub name: String,
    pub dropped: Vec<String>,
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The exported tables (see the module docs), by name.
pub async fn tables(tx: &mut TenantTx) -> Result<Vec<ExportTable>, Error> {
    let rows = sqlx::query!(
        r#"SELECT c.relname::text AS "name!",
                  coalesce(array_agg(a.attname::text ORDER BY a.attnum)
                           FILTER (WHERE a.atttypid = 'bytea'::regtype
                                   OR a.attname::text = ANY($1)
                                   OR (c.relname = 'email_messages'
                                       AND a.attname IN ('html', 'body_text', 'list_unsubscribe'))),
                           '{}') AS "dropped!"
           FROM pg_class c
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
           WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND NOT c.relispartition
             AND c.relrowsecurity AND c.relforcerowsecurity
             AND has_table_privilege(c.oid, 'SELECT')
             AND EXISTS (SELECT 1 FROM pg_attribute t
                         WHERE t.attrelid = c.oid AND t.attname = 'tenant_id'
                           AND NOT t.attisdropped)
             AND NOT (c.relname::text = ANY($2))
           GROUP BY c.relname
           ORDER BY c.relname"#,
        &SKIPPED_COLUMNS
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>(),
        &SKIPPED_TABLES
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|r| identifier(&r.name) && r.dropped.iter().all(|c| identifier(c)))
        .map(|r| ExportTable {
            name: r.name,
            dropped: r.dropped,
        })
        .collect())
}

/// Writes every exported table into `zip` as `<table>.jsonl`, reading through a cursor.
async fn write_tables(
    tx: &mut TenantTx,
    zip: &mut zip::ZipWriter<std::fs::File>,
    opts: SimpleFileOptions,
) -> Result<(), Error> {
    for t in tables(tx).await? {
        // Names and columns come from the catalog and are checked identifiers.
        let dropped = t
            .dropped
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DECLARE export_rows NO SCROLL CURSOR FOR
             SELECT (to_jsonb(t) - ARRAY[{dropped}]::text[])::text FROM \"{}\" t",
            t.name
        )))
        .execute(&mut **tx)
        .await?;
        zip.start_file(format!("{}.jsonl", t.name), opts)
            .map_err(internal)?;
        loop {
            let batch: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "FETCH FORWARD {FETCH} FROM export_rows"
            )))
            .fetch_all(&mut **tx)
            .await?;
            // ponytail: synchronous writes to a local temp file from async code; move to
            // spawn_blocking if exports ever stall the worker.
            for line in &batch {
                zip.write_all(line.as_bytes()).map_err(internal)?;
                zip.write_all(b"\n").map_err(internal)?;
            }
            if i64::try_from(batch.len()).unwrap_or(0) < FETCH {
                break;
            }
        }
        sqlx::query("CLOSE export_rows").execute(&mut **tx).await?;
    }
    Ok(())
}

async fn write_manifest(
    tx: &mut TenantTx,
    storage: &Storage,
    zip: &mut zip::ZipWriter<std::fs::File>,
    opts: SimpleFileOptions,
) -> Result<(), Error> {
    zip.start_file("assets-manifest.jsonl", opts)
        .map_err(internal)?;
    let rows = sqlx::query!(
        "SELECT id, filename, mime, bytes, width, height, sha256, variants
         FROM assets WHERE status = 'ready' ORDER BY id"
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        let variants: Vec<AssetVariant> = serde_json::from_value(r.variants).unwrap_or_default();
        let line = json!({
            "id": r.id, "filename": r.filename, "mime": r.mime, "bytes": r.bytes,
            "width": r.width, "height": r.height, "sha256": r.sha256,
            "variants": variants.iter().map(|v| json!({
                "format": v.format, "width": v.width, "height": v.height, "bytes": v.bytes,
                "url": storage.media_url(&v.key),
            })).collect::<Vec<_>>(),
        });
        zip.write_all(line.to_string().as_bytes())
            .map_err(internal)?;
        zip.write_all(b"\n").map_err(internal)?;
    }
    Ok(())
}

/// The worker job. `Err` = retry.
pub async fn run(db: &PgPool, storage: &Storage, tenant_id: Uuid, id: Uuid) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let started = sqlx::query_scalar!(
        "UPDATE data_exports SET status = 'running', updated_at = now()
         WHERE id = $1 AND status IN ('pending', 'running')
         RETURNING object_key",
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(key) = started else {
        return Ok(()); // done or failed already
    };
    tx.commit().await?;

    let file = tempfile::tempfile().map_err(internal)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    // One transaction: every table is read from the same tenant scope.
    let mut tx = tenant_tx(db, tenant_id).await?;
    zip.start_file("README.txt", opts).map_err(internal)?;
    zip.write_all(
        format!(
            "Shop data export {id}, created {}.\n\
             One JSON Lines file per table; assets-manifest.jsonl lists images with their URLs.\n\
             Secrets (password hashes, token and IP hashes, credentials) are not included.\n",
            Utc::now().to_rfc3339()
        )
        .as_bytes(),
    )
    .map_err(internal)?;
    write_tables(&mut tx, &mut zip, opts).await?;
    write_manifest(&mut tx, storage, &mut zip, opts).await?;
    tx.commit().await?;
    let mut file = zip.finish().map_err(internal)?;
    let size = file.seek(SeekFrom::End(0)).map_err(internal)?;
    file.seek(SeekFrom::Start(0)).map_err(internal)?;

    let mut upload = BufWriter::new(storage.private.clone(), Path::from(key));
    let mut src = tokio::fs::File::from_std(file);
    tokio::io::copy(&mut src, &mut upload)
        .await
        .map_err(internal)?;
    tokio::io::AsyncWriteExt::shutdown(&mut upload)
        .await
        .map_err(internal)?;

    let mut tx = tenant_tx(db, tenant_id).await?;
    sqlx::query!(
        "UPDATE data_exports SET status = 'ready', size_bytes = $2, completed_at = now(),
                updated_at = now(), error = NULL
         WHERE id = $1",
        id,
        i64::try_from(size).unwrap_or(i64::MAX)
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
