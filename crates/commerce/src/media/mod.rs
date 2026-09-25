//! Media assets (spec §7.5, §8.3, A21).
//!
//! Upload flow: `create_upload` issues a presigned PUT into the private bucket (the content
//! type is part of the signature) -> the client uploads to `uploads/<tenant>/<asset>` ->
//! `complete` verifies the object (size, sniffed type, header dimensions) and copies the
//! verified bytes to `originals/<tenant>/<asset>`, a key only the server writes, so a later
//! PUT with a still-valid URL cannot swap what gets processed -> the worker re-encodes it into
//! responsive variants in the public bucket (`process`) and the asset becomes `ready`
//! (`asset.ready` event). Public keys are content-addressed per asset:
//! `media/<tenant>/<asset>/<sha256 of the bytes>.<ext>`, immutable and safe to cache forever;
//! no two assets share an object, so deleting one never breaks another.

pub mod encode;

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use object_store::path::Path;
use object_store::signer::{HeaderValue, Method, SignedUrlOptions};
use object_store::{Attribute, Attributes, ObjectStoreExt, PutOptions, PutPayload};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

/// Job kind of the re-encoding step.
pub const PROCESS_JOB: &str = "media.process";
/// Job kind deleting objects of deleted assets (after the deleting transaction commits).
pub const PURGE_JOB: &str = "media.purge";
pub const UPLOAD_URL_TTL: Duration = Duration::from_secs(15 * 60);
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    /// Upload URL issued, waiting for `complete`.
    Pending,
    /// Verified, variants being generated.
    Processing,
    Ready,
    Failed,
}

impl AssetStatus {
    fn parse(s: &str) -> Self {
        match s {
            "processing" => Self::Processing,
            "ready" => Self::Ready,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AssetVariant {
    pub width: u32,
    pub height: u32,
    /// `avif`, `webp`, `jpeg` or `png`.
    pub format: String,
    /// Public-bucket key.
    pub key: String,
    pub bytes: u64,
    /// Public URL (filled on read from the configured media base URL).
    #[serde(default, skip_deserializing)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Asset {
    pub id: Uuid,
    pub status: AssetStatus,
    pub filename: Option<String>,
    /// Sniffed type of the original (after `complete`).
    pub mime: Option<String>,
    pub bytes: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub sha256: Option<String>,
    pub variants: Vec<AssetVariant>,
    /// Why processing failed.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewUpload {
    /// Original file name, for display only.
    #[schema(example = "tricko.jpg")]
    pub filename: Option<String>,
    /// `image/jpeg`, `image/png`, `image/webp` or `image/gif`. Must be sent with the PUT.
    #[schema(example = "image/jpeg")]
    pub content_type: String,
    /// Exact size in bytes (at most 20 MB); `complete` checks the uploaded file against it.
    #[schema(example = 184_320)]
    pub size: u64,
}

/// Where and how to upload the file.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct UploadTarget {
    #[schema(example = "PUT")]
    pub method: String,
    pub url: String,
    /// Headers the upload must send exactly as given (they are part of the signature).
    pub headers: BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Upload {
    pub asset: Asset,
    pub upload: UploadTarget,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssetPage {
    pub items: Vec<Asset>,
    pub next_cursor: Option<Uuid>,
}

impl NewUpload {
    pub fn validate(&self) -> Result<(), Error> {
        if !encode::ACCEPTED.contains(&self.content_type.as_str()) {
            return Err(invalid(
                "unsupported_type",
                "content_type must be image/jpeg, image/png, image/webp or image/gif",
            ));
        }
        if self.size == 0 || self.size > encode::MAX_BYTES {
            return Err(invalid(
                "file_too_large",
                format!("size must be 1-{} bytes", encode::MAX_BYTES),
            ));
        }
        if self.filename.as_deref().is_some_and(|f| {
            f.is_empty() || f.chars().count() > 255 || f.chars().any(char::is_control)
        }) {
            return Err(invalid(
                "invalid_filename",
                "filename must be 1-255 printable characters",
            ));
        }
        Ok(())
    }
}

fn internal(e: impl std::fmt::Display) -> Error {
    Error::Internal(e.to_string())
}

/// Where the client uploads (presigned PUT).
pub fn upload_key(tenant_id: Uuid, id: Uuid) -> Path {
    Path::from(format!("uploads/{tenant_id}/{id}"))
}

/// The verified original: written only by `complete`.
fn original_key(tenant_id: Uuid, id: Uuid) -> Path {
    Path::from(format!("originals/{tenant_id}/{id}"))
}

/// Creates a pending asset and a presigned upload URL into the private bucket.
pub async fn create_upload(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    input: &NewUpload,
) -> Result<Upload, Error> {
    input.validate()?;
    let tenant_id = tx.tenant_id();
    let id = crate::id::new_id();
    let key = upload_key(tenant_id, id);
    let size = i64::try_from(input.size).map_err(internal)?;
    sqlx::query!(
        "INSERT INTO assets (id, tenant_id, filename, key, mime, bytes) VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        tenant_id,
        input.filename,
        key.as_ref(),
        input.content_type,
        size
    )
    .execute(&mut **tx)
    .await?;

    // ponytail: a presigned PUT cannot cap the size (S3 signers own `content-length`);
    // `complete` rejects and deletes oversize files. Presigned POST policies
    // (`content-length-range`) would enforce it at the bucket if abuse shows up.
    let options = SignedUrlOptions::new().with_signed_header(
        object_store::signer::HeaderName::from_static("content-type"),
        HeaderValue::from_str(&input.content_type).map_err(internal)?,
    );
    let url = storage
        .private_signer
        .signed_url_opts(Method::PUT, &key, UPLOAD_URL_TTL, &options)
        .await?;
    let asset = get(tx, storage, id).await?;
    audit::record(
        tx,
        actor,
        "asset.created",
        "asset",
        Some(&id.to_string()),
        &json!({ "after": asset }),
    )
    .await?;
    let expires_at = Utc::now() + chrono::Duration::from_std(UPLOAD_URL_TTL).map_err(internal)?;
    Ok(Upload {
        asset,
        upload: UploadTarget {
            method: "PUT".into(),
            url: url.to_string(),
            headers: BTreeMap::from([("content-type".to_owned(), input.content_type.clone())]),
            expires_at,
        },
    })
}

/// Verifies the uploaded original and queues processing. Repeating it on a processing or
/// ready asset returns the asset unchanged. A rejected file is deleted and the asset stays
/// `pending`, so the client can upload a correct file with the same URL while it is valid.
pub async fn complete(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    id: Uuid,
) -> Result<Asset, Error> {
    let row = sqlx::query!(
        "SELECT status, key, bytes FROM assets WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    match AssetStatus::parse(&row.status) {
        AssetStatus::Pending => {}
        // The verified original is kept: processing can simply run again.
        AssetStatus::Failed => {
            sqlx::query!(
                "UPDATE assets SET status = 'processing', error = NULL, updated_at = now()
                 WHERE id = $1",
                id
            )
            .execute(&mut **tx)
            .await?;
            queue_processing(tx, id).await?;
            let asset = get(tx, storage, id).await?;
            audit::record(
                tx,
                actor,
                "asset.retried",
                "asset",
                Some(&id.to_string()),
                &json!({ "after": asset }),
            )
            .await?;
            return Ok(asset);
        }
        AssetStatus::Processing | AssetStatus::Ready => return get(tx, storage, id).await,
    }
    let key = Path::from(row.key);
    let meta = match storage.private.head(&key).await {
        Ok(meta) => meta,
        Err(object_store::Error::NotFound { .. }) => {
            return Err(Error::Conflict {
                code: "upload_missing",
                detail: "no file was uploaded for this asset".into(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    if meta.size > encode::MAX_BYTES {
        storage.private.delete(&key).await?;
        return Err(invalid(
            "file_too_large",
            encode::Rejected::TooLarge.detail(),
        ));
    }
    // Bounded by the checked size, even if the object was replaced after the HEAD.
    let bytes = storage.private.get_range(&key, 0..meta.size).await?;
    let verified = tokio::task::spawn_blocking(move || {
        let sha = hex::encode(Sha256::digest(&bytes));
        encode::verify(&bytes).map(|v| (v, sha, bytes))
    })
    .await
    .map_err(internal)?;
    let (verified, sha256, bytes) = match verified {
        Ok(v) => v,
        Err(rejected) => {
            storage.private.delete(&key).await?;
            return Err(invalid(rejected.code(), rejected.detail()));
        }
    };
    if row.bytes.and_then(|b| usize::try_from(b).ok()) != Some(bytes.len()) {
        storage.private.delete(&key).await?;
        return Err(invalid(
            "size_mismatch",
            "the uploaded file does not have the declared size",
        ));
    }
    let original = original_key(tx.tenant_id(), id);
    storage
        .private
        .put(&original, PutPayload::from(bytes))
        .await?;
    sqlx::query!(
        "UPDATE assets SET status = 'processing', key = $6, mime = $2, width = $3, height = $4,
                sha256 = $5, updated_at = now()
         WHERE id = $1",
        id,
        verified.mime,
        i32::try_from(verified.width).map_err(internal)?,
        i32::try_from(verified.height).map_err(internal)?,
        sha256,
        original.as_ref()
    )
    .execute(&mut **tx)
    .await?;
    queue_processing(tx, id).await?;
    // The staging upload goes only once this transaction commits (a rollback keeps it, so
    // `complete` can be retried).
    let mut purge_upload = NewJob::new(
        PURGE_JOB,
        json!({ "private": [key.as_ref()], "public": [] }),
    );
    purge_upload.tenant_id = Some(tx.tenant_id());
    queue::enqueue(&mut **tx, &purge_upload).await?;
    let asset = get(tx, storage, id).await?;
    audit::record(
        tx,
        actor,
        "asset.completed",
        "asset",
        Some(&id.to_string()),
        &json!({ "after": asset }),
    )
    .await?;
    Ok(asset)
}

/// Queues [`PROCESS_JOB`]. The caller holds the asset row lock and has checked the status,
/// so each call is a deliberate new run (first completion or a retry after failure).
async fn queue_processing(tx: &mut TenantTx, id: Uuid) -> Result<(), Error> {
    let mut job = NewJob::new(PROCESS_JOB, json!({ "asset_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = 5;
    job.idempotency_key = Some(format!("{PROCESS_JOB}:{id}:{}", crate::id::new_id()));
    queue::enqueue(&mut **tx, &job).await?;
    Ok(())
}

/// Outcome of [`process`] for the job runner.
#[derive(Debug, PartialEq, Eq)]
pub enum Processed {
    Ready,
    /// The image could not be decoded; the asset is `failed` (no retry helps).
    Failed,
    /// Not in `processing` (already done, or deleted): nothing to do.
    Skipped,
}

/// Worker step: re-encodes the verified original into public variants. Idempotent: keys are
/// content-addressed and only a `processing` asset is touched. The asset row stays locked for
/// the whole run, so a duplicate run waits and then skips, and a concurrent delete waits until
/// the variants are recorded (its purge then removes them). Storage and database errors are
/// returned for a retry, after removing what this run wrote (see [`mark_failed`] for the last
/// attempt). CPU-heavy work runs on a blocking thread; callers bound how many run at once.
pub async fn process(
    db: &PgPool,
    storage: &Storage,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Processed, Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    let row = sqlx::query!(
        "SELECT status, key, sha256 FROM assets WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row.filter(|r| r.status == "processing") else {
        return Ok(Processed::Skipped);
    };
    let key = Path::from(row.key);
    // Bounded download of the verified original, checked against the digest from `complete`.
    let size = storage.private.head(&key).await?.size;
    let rendered = if size > encode::MAX_BYTES {
        Err(encode::Rejected::TooLarge)
    } else {
        let original = storage.private.get_range(&key, 0..size).await?;
        let expected = row.sha256.unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            if hex::encode(Sha256::digest(&original)) != expected {
                return Err(encode::Rejected::Corrupt(
                    "the original changed after verification".into(),
                ));
            }
            encode::render(&original)
        })
        .await
        .map_err(internal)?
    };
    let encoded = match rendered {
        Ok(e) => e,
        Err(rejected) => {
            set_failed(&mut tx, id, &rejected.detail()).await?;
            tx.commit().await?;
            return Ok(Processed::Failed);
        }
    };

    let mut variants = Vec::with_capacity(encoded.len());
    let stored = store_variants(storage, tenant_id, id, encoded, &mut variants).await;
    let recorded = match stored {
        Ok(()) => record_ready(tx, id, &variants).await,
        Err(e) => Err(e),
    };
    if let Err(e) = recorded {
        let keys: Vec<String> = variants.into_iter().map(|v| v.key).collect();
        if let Err(cleanup) = purge(storage, &[], &keys).await {
            tracing::warn!(asset = %id, error = %cleanup, "removing partial variants failed");
        }
        return Err(e);
    }
    Ok(Processed::Ready)
}

async fn store_variants(
    storage: &Storage,
    tenant_id: Uuid,
    id: Uuid,
    encoded: Vec<encode::Encoded>,
    variants: &mut Vec<AssetVariant>,
) -> Result<(), Error> {
    for e in encoded {
        let key = format!(
            "media/{tenant_id}/{id}/{}.{}",
            hex::encode(Sha256::digest(&e.bytes)),
            e.format.ext()
        );
        let attributes = Attributes::from_iter([
            (Attribute::ContentType, e.format.mime()),
            (Attribute::CacheControl, IMMUTABLE),
        ]);
        let bytes = u64::try_from(e.bytes.len()).map_err(internal)?;
        // Listed before the write, so a failed write is cleaned up too.
        variants.push(AssetVariant {
            width: e.width,
            height: e.height,
            format: e.format.name().into(),
            key: key.clone(),
            bytes,
            url: String::new(),
        });
        storage
            .public
            .put_opts(
                &Path::from(key),
                PutPayload::from(e.bytes),
                PutOptions {
                    attributes,
                    ..Default::default()
                },
            )
            .await?;
    }
    Ok(())
}

async fn record_ready(mut tx: TenantTx, id: Uuid, variants: &[AssetVariant]) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE assets SET status = 'ready', variants = $2, error = NULL, updated_at = now()
         WHERE id = $1",
        id,
        serde_json::to_value(variants).map_err(internal)?
    )
    .execute(&mut *tx)
    .await?;
    queue::publish(&mut *tx, "asset.ready", &json!({ "asset_id": id })).await?;
    tx.commit().await?;
    Ok(())
}

async fn set_failed(tx: &mut TenantTx, id: Uuid, reason: &str) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE assets SET status = 'failed', error = $2, updated_at = now()
         WHERE id = $1 AND status = 'processing'",
        id,
        reason
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Marks a `processing` asset as failed with a client-safe reason (undecodable image, or the
/// job's last attempt failing on storage or database errors).
pub async fn mark_failed(
    db: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    reason: &str,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant_id).await?;
    set_failed(&mut tx, id, reason).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes an asset that no product uses (`409 asset_in_use` otherwise); categories lose it
/// as their image. The objects (upload, original, variants) are removed by a `media.purge`
/// job after commit.
pub async fn delete(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    id: Uuid,
) -> Result<(), Error> {
    // Lock first: a running `process` finishes and records its variants before we read them.
    sqlx::query!("SELECT id FROM assets WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let before = get(tx, storage, id).await?;
    sqlx::query!("DELETE FROM assets WHERE id = $1", id)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|d| d.code().as_deref() == Some("23503"))
            {
                Error::Conflict {
                    code: "asset_in_use",
                    detail: "the asset is used by a product".into(),
                }
            } else {
                e.into()
            }
        })?;
    let public_keys: Vec<&str> = before.variants.iter().map(|v| v.key.as_str()).collect();
    let tenant_id = tx.tenant_id();
    let private_keys = [
        upload_key(tenant_id, id).to_string(),
        original_key(tenant_id, id).to_string(),
    ];
    let mut job = NewJob::new(
        PURGE_JOB,
        json!({ "private": private_keys, "public": public_keys }),
    );
    job.tenant_id = Some(tx.tenant_id());
    queue::enqueue(&mut **tx, &job).await?;
    audit::record(
        tx,
        actor,
        "asset.deleted",
        "asset",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    Ok(())
}

/// Worker step for [`PURGE_JOB`]: deletes the listed objects (missing ones are fine).
pub async fn purge(storage: &Storage, private: &[String], public: &[String]) -> Result<(), Error> {
    for (store, keys) in [(&storage.private, private), (&storage.public, public)] {
        for key in keys {
            match store.delete(&Path::from(key.as_str())).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

struct Row {
    id: Uuid,
    status: String,
    filename: Option<String>,
    mime: Option<String>,
    bytes: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    sha256: Option<String>,
    variants: serde_json::Value,
    error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn to_asset(storage: &Storage, r: Row) -> Result<Asset, Error> {
    let mut variants: Vec<AssetVariant> = serde_json::from_value(r.variants).map_err(internal)?;
    for v in &mut variants {
        v.url = storage.media_url(&v.key);
    }
    Ok(Asset {
        id: r.id,
        status: AssetStatus::parse(&r.status),
        filename: r.filename,
        mime: r.mime,
        bytes: r.bytes,
        width: r.width,
        height: r.height,
        sha256: r.sha256,
        variants,
        error: r.error,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

pub async fn get(tx: &mut TenantTx, storage: &Storage, id: Uuid) -> Result<Asset, Error> {
    let row = sqlx::query_as!(
        Row,
        "SELECT id, status, filename, mime, bytes, width, height, sha256, variants, error,
                created_at, updated_at
         FROM assets WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    to_asset(storage, row)
}

pub const MAX_PAGE: i64 = 100;

/// Newest first, optionally one status.
pub async fn list(
    tx: &mut TenantTx,
    storage: &Storage,
    status: Option<AssetStatus>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<AssetPage, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_PAGE}"),
        ));
    }
    let status = status.map(|s| match s {
        AssetStatus::Pending => "pending",
        AssetStatus::Processing => "processing",
        AssetStatus::Ready => "ready",
        AssetStatus::Failed => "failed",
    });
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, status, filename, mime, bytes, width, height, sha256, variants, error,
                created_at, updated_at
         FROM assets
         WHERE ($1::uuid IS NULL OR id < $1) AND ($2::text IS NULL OR status = $2)
         ORDER BY id DESC LIMIT $3",
        cursor,
        status,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(|r| to_asset(storage, r))
        .collect::<Result<Vec<_>, Error>>()?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        items.last().map(|a| a.id)
    } else {
        None
    };
    Ok(AssetPage { items, next_cursor })
}
