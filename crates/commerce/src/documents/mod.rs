//! PDF documents (spec §10.6, §10.7, A21): invoices, packing slips and label sheets, rendered
//! by the worker with the Typst CLI into the private bucket and downloaded through
//! short-lived presigned URLs after authorization.
//!
//! Rendering: a fresh temporary directory holds the template (as `main.typ`), `data.json` and
//! any files it embeds (label PDFs); `typst compile --root <dir> --ignore-system-fonts` runs
//! with a time limit, so a template can read nothing outside that directory and output does
//! not depend on the host's fonts. The templates require accountant approval before real use.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use object_store::ObjectStoreExt;
use platform::Error;
use platform::db::TenantTx;
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::markets::invalid;

/// Renders a `documents` row (payload `{document_id}`).
pub const RENDER_JOB: &str = "documents.render";
/// Longest a render may take.
const RENDER_TIMEOUT: Duration = Duration::from_secs(60);
/// Label sheets and packing slips cover at most this many orders.
pub const MAX_ORDERS: usize = 100;
/// Presigned download URLs live 5 minutes (A21).
pub const URL_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Invoice,
    PackingSlip,
    Labels,
}

impl Template {
    fn source(self) -> &'static str {
        match self {
            Self::Invoice => include_str!("templates/invoice.typ"),
            Self::PackingSlip => include_str!("templates/packing_slip.typ"),
            Self::Labels => include_str!("templates/labels.typ"),
        }
    }
}

/// The Typst CLI (`TYPST_BIN`, default `typst` on `PATH`).
#[derive(Debug, Clone)]
pub struct Typst {
    pub bin: PathBuf,
}

impl Default for Typst {
    fn default() -> Self {
        Self {
            bin: PathBuf::from("typst"),
        }
    }
}

/// Removes the working directory however rendering ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Typst {
    /// Renders `template` with `data` (and `files`: name → bytes, plain names only).
    pub async fn render(
        &self,
        template: Template,
        data: &Value,
        files: &[(String, Vec<u8>)],
    ) -> Result<Vec<u8>, Error> {
        let dir = TempDir(std::env::temp_dir().join(format!("typst-{}", Uuid::now_v7())));
        let io = |e: std::io::Error| Error::Internal(format!("typst workdir: {e}"));
        tokio::fs::create_dir_all(&dir.0).await.map_err(io)?;
        tokio::fs::write(dir.0.join("main.typ"), template.source())
            .await
            .map_err(io)?;
        let json = serde_json::to_vec(data).map_err(|e| Error::Internal(e.to_string()))?;
        tokio::fs::write(dir.0.join("data.json"), json)
            .await
            .map_err(io)?;
        for (name, bytes) in files {
            let plain = !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
                && !name.starts_with('.');
            if !plain {
                return Err(Error::Internal(format!("bad render file name {name}")));
            }
            tokio::fs::write(dir.0.join(name), bytes)
                .await
                .map_err(io)?;
        }
        let out = dir.0.join("out.pdf");
        let child = tokio::process::Command::new(&self.bin)
            .arg("compile")
            .arg("--root")
            .arg(&dir.0)
            .arg("--ignore-system-fonts")
            .arg(dir.0.join("main.typ"))
            .arg(&out)
            // No secrets from the worker's environment; PATH only to find the binary.
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(RENDER_TIMEOUT, child)
            .await
            .map_err(|_| Error::Internal("typst timed out".into()))?
            .map_err(|e| Error::Unavailable(format!("typst could not run: {e}")))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Internal(format!(
                "typst failed: {}",
                err.chars().take(800).collect::<String>()
            )));
        }
        tokio::fs::read(&out).await.map_err(io)
    }
}

/// A presigned GET URL for a private object (A21), `attachment` with `filename`.
pub async fn download_url(storage: &Storage, key: &str, filename: &str) -> Result<String, Error> {
    use object_store::signer::Method;
    // ponytail: the file name comes from the key's last segment; a Content-Disposition
    // override needs `response-content-disposition`, which object_store's signer lacks.
    let _ = filename;
    let url = storage
        .private_signer
        .signed_url(Method::GET, &object_store::path::Path::from(key), URL_TTL)
        .await?;
    Ok(url.to_string())
}

// ---------------------------------------------------------------------------------------
// Generated documents: packing slips and label sheets

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedKind {
    PackingSlips,
    Labels,
}

impl GeneratedKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::PackingSlips => "packing_slips",
            Self::Labels => "labels",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Pending,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GeneratedDocument {
    pub id: Uuid,
    pub kind: GeneratedKind,
    pub order_ids: Vec<Uuid>,
    pub status: DocumentStatus,
    pub error: Option<String>,
    /// A 5-minute download link once `ready`.
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentInput {
    pub kind: GeneratedKind,
    /// 1-100 orders.
    pub order_ids: Vec<Uuid>,
    /// Packing slip language: `cs`, `sk` or `en` (default `cs`).
    #[serde(default)]
    pub locale: Option<String>,
}

/// Queues a document for rendering. The orders must exist (RLS: the tenant's own).
pub async fn request(
    tx: &mut TenantTx,
    actor: &str,
    input: &DocumentInput,
) -> Result<GeneratedDocument, Error> {
    let mut ids = input.order_ids.clone();
    ids.sort();
    ids.dedup();
    if ids.is_empty() || ids.len() > MAX_ORDERS {
        return Err(invalid("invalid_document", "1-100 orders"));
    }
    let locale = input.locale.as_deref().unwrap_or("cs");
    if !matches!(locale, "cs" | "sk" | "en") {
        return Err(invalid("invalid_document", "locale: cs, sk or en"));
    }
    let known = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM orders WHERE id = ANY($1)"#,
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).unwrap_or(0) != ids.len() {
        return Err(Error::NotFound);
    }
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO documents (id, tenant_id, kind, order_ids, locale, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        tx.tenant_id(),
        input.kind.as_str(),
        &ids,
        locale,
        actor
    )
    .execute(&mut **tx)
    .await?;
    let mut job = NewJob::new(RENDER_JOB, json!({ "document_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = 3;
    job.idempotency_key = Some(format!("document:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    get(tx, None, id).await
}

/// The document; with `storage`, a download URL when it is ready.
pub async fn get(
    tx: &mut TenantTx,
    storage: Option<&Storage>,
    id: Uuid,
) -> Result<GeneratedDocument, Error> {
    let r = sqlx::query!(
        "SELECT id, kind, order_ids, status, object_key, error, created_at
         FROM documents WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let url = match (storage, &r.object_key) {
        (Some(s), Some(key)) if r.status == "ready" => {
            Some(download_url(s, key, &format!("{}.pdf", r.kind)).await?)
        }
        _ => None,
    };
    Ok(GeneratedDocument {
        id: r.id,
        kind: if r.kind == "labels" {
            GeneratedKind::Labels
        } else {
            GeneratedKind::PackingSlips
        },
        order_ids: r.order_ids,
        status: match r.status.as_str() {
            "ready" => DocumentStatus::Ready,
            "failed" => DocumentStatus::Failed,
            _ => DocumentStatus::Pending,
        },
        error: r.error,
        url,
        created_at: r.created_at,
    })
}

/// Packing slip data (`packing_slip.typ`).
async fn packing_data(tx: &mut TenantTx, ids: &[Uuid], locale: &str) -> Result<Value, Error> {
    let shop = sqlx::query_scalar!(
        "SELECT name FROM platform.tenants WHERE id = $1",
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    let mut orders = Vec::new();
    for id in ids {
        let o = crate::orders::view(tx, *id).await?;
        let a = o.shipping_address.as_ref().or(o.billing_address.as_ref());
        orders.push(json!({
            "number": o.number,
            "placed_on": match locale {
                "en" => o.placed_at.format("%-d %b %Y").to_string(),
                _ => o.placed_at.format("%-d. %-m. %Y").to_string(),
            },
            "shipping": o.shipping.name,
            "pickup_point": o.shipping.pickup_point.as_ref().map(|p| format!(
                "{} ({}), {}, {} {}", p.name, p.id, p.street, p.zip, p.city)),
            "address": a.map(|a| {
                let mut lines = vec![a.name.clone()];
                if let Some(c) = a.company.as_ref().filter(|c| !c.is_empty()) {
                    lines.push(c.clone());
                }
                lines.push(a.street.clone());
                lines.push(format!("{} {}", a.postal_code, a.city));
                lines.push(a.country.clone());
                lines
            }).unwrap_or_default(),
            "email": o.email,
            "phone": o.phone,
            "notes": o.notes,
            "lines": o.lines.iter().map(|l| json!({
                "sku": l.sku, "name": l.name, "options": l.options_label,
                "quantity": l.quantity.to_string(),
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({ "locale": locale, "shop": shop, "orders": orders }))
}

/// Renders a queued document (the worker job). Missing labels fail the document (the admin
/// creates labels first; the bulk endpoint does it before queuing).
pub async fn render(
    db: &sqlx::PgPool,
    storage: &Storage,
    typst: &Typst,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<(), Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let d = sqlx::query!(
        "SELECT kind, order_ids, locale, status FROM documents WHERE id = $1",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    if d.status != "pending" {
        return Ok(());
    }
    let (template, data, files) = if d.kind == "labels" {
        let keys = sqlx::query_scalar!(
            r#"SELECT s.label_key AS "label_key!" FROM shipments s
               WHERE s.order_id = ANY($1) AND s.status <> 'cancelled' AND s.label_key IS NOT NULL
               ORDER BY array_position($1, s.order_id)"#,
            &d.order_ids
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut files = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let bytes = storage
                .private
                .get(&object_store::path::Path::from(key.as_str()))
                .await?
                .bytes()
                .await?;
            files.push((format!("label-{i}.pdf"), bytes.to_vec()));
        }
        let names: Vec<&String> = files.iter().map(|(n, _)| n).collect();
        (Template::Labels, json!({ "files": names }), files)
    } else {
        let data = packing_data(&mut tx, &d.order_ids, &d.locale).await?;
        tx.commit().await?;
        (Template::PackingSlip, data, Vec::new())
    };
    let outcome = if template == Template::Labels && files.is_empty() {
        Err(Error::Conflict {
            code: "no_labels",
            detail: "none of the orders has a label".into(),
        })
    } else {
        typst.render(template, &data, &files).await
    };
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    match outcome {
        Ok(pdf) => {
            let key = format!("documents/{tenant_id}/{id}.pdf");
            storage
                .private
                .put(&object_store::path::Path::from(key.as_str()), pdf.into())
                .await?;
            let published = sqlx::query!(
                "UPDATE documents SET status = 'ready', object_key = $2, updated_at = now()
                 WHERE id = $1 AND status = 'pending'",
                id,
                key
            )
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if published == 0 {
                // Withdrawn while rendering (a personal-data erasure): the file must not stay.
                storage
                    .private
                    .delete(&object_store::path::Path::from(key.as_str()))
                    .await?;
            }
        }
        Err(e) => {
            sqlx::query!(
                "UPDATE documents SET status = 'failed', error = $2, updated_at = now()
                 WHERE id = $1",
                id,
                e.to_string().chars().take(1000).collect::<String>()
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
