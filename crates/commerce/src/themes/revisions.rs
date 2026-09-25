//! Tenant theme revisions (WP23, spec §7.5, §12.3, A6, A21, A22, A30): sources, the builder's
//! callbacks, previews, publish/rollback, and the maintenance that expires stuck builds and
//! garbage-collects artifacts.
//!
//! Status machine (builder callbacks and staff actions):
//!
//! ```text
//! draft ──builder──► building ──artifact──► checking ──gates──► ready ──publish──► published
//!   │                   │                      │                 ▲                     │
//!   └──────────────────►┴──────────► failed ◄──┘                 │     another publish ▼
//!                                                               └──── rollback ◄── superseded
//! ```

use std::time::Duration;

use chrono::{DateTime, Utc};
use object_store::path::Path;
use object_store::signer::Method;
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

use super::archive::{self, Source, TOKENS_FILE};
use super::keys::{PREVIEW_TTL_SECS, ThemeKeys};
use super::{ArtifactKind, DEFAULT_THEME, check_artifact, object_key, register_artifact};
use crate::audit;
use crate::markets::invalid;
use crate::storefront::PublicUrls;
use crate::tenancy::{self, Resolved};

/// Worker job: hand a revision to the theme builder (`{revision_id}`, tenant job).
pub const BUILD_JOB: &str = "themes.build";
/// Hourly: expire stuck builds, garbage-collect artifacts.
pub const MAINTENANCE_JOB: &str = "themes.maintenance";

/// Builds a tenant may have queued or running at once (abuse control for the shared builder).
pub const MAX_PENDING_BUILDS: i64 = 3;
/// A build that has not moved for this long is failed by the maintenance job.
pub const STUCK_AFTER_MINUTES: i32 = 30;
/// Artifact GC: unreferenced artifacts older than this are deleted; failed revisions lose their
/// artifact after it, and so do ready revisions beyond the newest [`KEEP_READY`].
pub const GC_AFTER_DAYS: i32 = 7;
pub const KEEP_READY: i64 = 5;
/// A21: private objects are handed out as 5-minute presigned URLs.
const PRESIGN_TTL: Duration = Duration::from_secs(300);
/// Gate report size limit (it is shown in the admin, not a log store).
const MAX_CHECKS_BYTES: usize = 256 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;
pub const SCREENSHOTS: [&str; 6] = [
    "home-mobile",
    "home-desktop",
    "category-mobile",
    "category-desktop",
    "product-mobile",
    "product-desktop",
];

/// How a revision came about (`theme_revisions.change`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Default,
    Fork,
    Tokens,
    Upload,
    Reset,
    Ai,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fork => "fork",
            Self::Tokens => "tokens",
            Self::Upload => "upload",
            Self::Reset => "reset",
            Self::Ai => "ai",
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevisionSummary {
    pub id: Uuid,
    pub number: i32,
    pub parent_id: Option<Uuid>,
    /// `default` (follows the shared default theme) or `custom` (tenant-owned source).
    pub origin: String,
    /// `default`, `fork`, `tokens`, `upload`, `reset` or `ai`.
    pub change: String,
    /// `draft`, `building`, `checking`, `ready`, `failed`, `published` or `superseded`.
    pub status: String,
    pub artifact_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub status_changed_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    /// The revision the shop currently serves.
    pub active: bool,
    /// A tenant-owned source archive exists (downloadable).
    pub has_source: bool,
    /// Why the gates failed (from the check report), empty otherwise.
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Screenshot {
    /// `home-mobile`, `product-desktop`, ...
    pub name: String,
    /// Presigned URL, valid for 5 minutes (A21).
    pub url: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevisionDetail {
    pub revision: RevisionSummary,
    /// The builder's gate report: `pipeline`, `sandbox`, `steps[]`, `failures[]`.
    #[schema(value_type = Object)]
    pub checks: Value,
    /// Design tokens of the built artifact (`colors`, `fonts`, `radius`), if built.
    #[schema(value_type = Option<std::collections::HashMap<String, std::collections::HashMap<String, String>>>)]
    pub tokens: Option<Value>,
    pub screenshots: Vec<Screenshot>,
}

/// A presigned download (source archive).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Download {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PreviewLink {
    /// `preview-<n>--<shop>` URL carrying the token once; the edge swaps it for a cookie.
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

struct Row {
    id: Uuid,
    number: i32,
    parent_id: Option<Uuid>,
    origin: String,
    change: String,
    status: String,
    artifact_id: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    status_changed_at: DateTime<Utc>,
    published_at: Option<DateTime<Utc>>,
    active: bool,
    source_key: Option<String>,
    checks: Value,
}

impl Row {
    fn summary(&self) -> RevisionSummary {
        RevisionSummary {
            id: self.id,
            number: self.number,
            parent_id: self.parent_id,
            origin: self.origin.clone(),
            change: self.change.clone(),
            status: self.status.clone(),
            artifact_id: self.artifact_id.clone(),
            created_by: self.created_by.clone(),
            created_at: self.created_at,
            status_changed_at: self.status_changed_at,
            published_at: self.published_at,
            active: self.active,
            has_source: self.source_key.is_some(),
            failures: failures(&self.checks),
        }
    }
}

fn failures(checks: &Value) -> Vec<String> {
    checks["failures"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f.as_str().map(str::to_owned))
        .take(50)
        .collect()
}

/// The revision columns + whether it is the active one, followed by `$tail`.
macro_rules! select {
    ($tail:literal) => {
        concat!(
            "SELECT r.id, r.number, r.parent_id, r.origin, r.change, r.status, r.artifact_id,
                r.created_by, r.created_at, r.status_changed_at, r.published_at,
                (a.revision_id IS NOT NULL) AS active, r.source_key, r.checks
             FROM theme_revisions r LEFT JOIN theme_active a ON a.revision_id = r.id ",
            $tail
        )
    };
}

fn row(r: &sqlx::postgres::PgRow) -> Result<Row, sqlx::Error> {
    use sqlx::Row as _;
    Ok(Row {
        id: r.try_get("id")?,
        number: r.try_get("number")?,
        parent_id: r.try_get("parent_id")?,
        origin: r.try_get("origin")?,
        change: r.try_get("change")?,
        status: r.try_get("status")?,
        artifact_id: r.try_get("artifact_id")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        status_changed_at: r.try_get("status_changed_at")?,
        published_at: r.try_get("published_at")?,
        active: r.try_get("active")?,
        source_key: r.try_get("source_key")?,
        checks: r.try_get("checks")?,
    })
}

async fn fetch(tx: &mut TenantTx, id: Uuid, lock: bool) -> Result<Row, Error> {
    let sql = if lock {
        select!("WHERE r.id = $1 FOR UPDATE OF r")
    } else {
        select!("WHERE r.id = $1")
    };
    let r = sqlx::query(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    Ok(row(&r)?)
}

/// The newest 100 revisions.
pub async fn list(tx: &mut TenantTx) -> Result<Vec<RevisionSummary>, Error> {
    let rows = sqlx::query(select!("ORDER BY r.number DESC LIMIT 100"))
        .fetch_all(&mut **tx)
        .await?;
    rows.iter()
        .map(|r| Ok(row(r)?.summary()))
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Error::from)
}

/// One revision with its check report, tokens and presigned screenshot URLs.
pub async fn detail(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
) -> Result<RevisionDetail, Error> {
    let r = fetch(tx, id, false).await?;
    let tokens = match &r.artifact_id {
        Some(a) => sqlx::query_scalar!(
            "SELECT tokens FROM platform.theme_artifacts WHERE id = $1",
            a
        )
        .fetch_optional(&mut **tx)
        .await?
        .flatten(),
        None => None,
    };
    let mut screenshots = Vec::new();
    for name in r.checks["screenshots"].as_array().into_iter().flatten() {
        let Some(name) = name.as_str().filter(|n| SCREENSHOTS.contains(n)) else {
            continue;
        };
        let url = storage
            .private_signer
            .signed_url(
                Method::GET,
                &shot_key(tx.tenant_id(), id, name),
                PRESIGN_TTL,
            )
            .await?;
        screenshots.push(Screenshot {
            name: name.to_owned(),
            url: url.to_string(),
        });
    }
    Ok(RevisionDetail {
        revision: r.summary(),
        checks: r.checks,
        tokens,
        screenshots,
    })
}

fn source_key(tenant: Uuid, revision: Uuid) -> Path {
    Path::from(format!("theme-sources/{tenant}/{revision}.tar.gz"))
}

fn shot_key(tenant: Uuid, revision: Uuid, name: &str) -> Path {
    Path::from(format!("theme-shots/{tenant}/{revision}/{name}.png"))
}

/// Revision numbers are `max + 1` per tenant: one writer at a time.
pub(crate) async fn lock_numbering(tx: &mut TenantTx) -> Result<(), Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('theme_revisions:' || $1::text, 0))",
        tx.tenant_id().to_string()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn load_source(storage: &Storage, key: &str) -> Result<Source, Error> {
    let bytes = storage.private.get(&Path::from(key)).await?.bytes().await?;
    archive::read(&bytes).map_err(|e| Error::Internal(format!("stored source {key}: {e}")))
}

/// The source of the current default theme (`make theme-build` publishes it).
async fn default_source(tx: &mut TenantTx, storage: &Storage) -> Result<Source, Error> {
    let key = sqlx::query_scalar!(
        "SELECT a.source_key FROM platform.artifact_channels c
         JOIN platform.theme_artifacts a ON a.id = c.artifact_id WHERE c.name = $1",
        DEFAULT_THEME
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten()
    .ok_or(Error::Conflict {
        code: "default_source_missing",
        detail: "the default theme source is not published yet (make theme-build)".into(),
    })?;
    load_source(storage, &key).await
}

/// The source a revision was built from: its own archive, or (for revisions following the
/// default) the default artifact's source, falling back to the current default.
pub(crate) async fn revision_source(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
) -> Result<Source, Error> {
    let r = sqlx::query!(
        "SELECT r.source_key, t.source_key AS artifact_source FROM theme_revisions r
         LEFT JOIN platform.theme_artifacts t ON t.id = r.artifact_id WHERE r.id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    match r.source_key.or(r.artifact_source) {
        Some(key) => load_source(storage, &key).await,
        None => default_source(tx, storage).await,
    }
}

pub(crate) async fn active_id(tx: &mut TenantTx) -> Result<Option<Uuid>, Error> {
    Ok(sqlx::query_scalar!("SELECT revision_id FROM theme_active")
        .fetch_optional(&mut **tx)
        .await?)
}

/// Stores the source and creates a draft revision whose build is queued. `ai_run`: the AI run
/// (and its prompt) a `Change::Ai` revision checks.
pub(crate) async fn create(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    change: Change,
    parent: Option<Uuid>,
    source: &Source,
    ai_run: Option<(Uuid, &str)>,
) -> Result<RevisionSummary, Error> {
    lock_numbering(tx).await?;
    let pending = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM theme_revisions
           WHERE status IN ('draft', 'building', 'checking')"#
    )
    .fetch_one(&mut **tx)
    .await?;
    if pending >= MAX_PENDING_BUILDS {
        return Err(Error::Conflict {
            code: "builds_in_progress",
            detail: format!("{pending} theme builds are still running; wait for one to finish"),
        });
    }
    let id = Uuid::now_v7();
    let key = source_key(tx.tenant_id(), id);
    // Stored before the row: a rolled-back transaction leaves an unreferenced object, never a
    // revision without its source.
    storage
        .private
        .put(&key, PutPayload::from(archive::write(source)))
        .await?;
    sqlx::query!(
        "INSERT INTO theme_revisions (id, tenant_id, number, parent_id, origin, change, status,
                                      source_key, created_by, prompt, ai_run_id)
         SELECT $1, $2, coalesce(max(number), 0) + 1, $3, 'custom', $4, 'draft', $5, $6, $7, $8
         FROM theme_revisions",
        id,
        tx.tenant_id(),
        parent,
        change.as_str(),
        key.as_ref(),
        actor,
        ai_run.map(|(_, p)| p),
        ai_run.map(|(r, _)| r)
    )
    .execute(&mut **tx)
    .await?;
    let mut job = NewJob::new(BUILD_JOB, json!({ "revision_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = 8;
    job.idempotency_key = Some(format!("theme-build:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    let r = fetch(tx, id, false).await?;
    audit::record(
        tx,
        actor,
        "theme.revision_created",
        "theme_revision",
        Some(&id.to_string()),
        &json!({ "number": r.number, "change": change.as_str(), "parent_id": parent }),
    )
    .await?;
    Ok(r.summary())
}

/// A tenant-owned copy of the default theme (the tenant then stops following the default
/// once it publishes a custom revision, A30).
pub async fn fork(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
) -> Result<RevisionSummary, Error> {
    let source = default_source(tx, storage).await?;
    let parent = active_id(tx).await?;
    create(tx, storage, actor, Change::Fork, parent, &source, None).await
}

/// The latest default theme with the tenant's current design tokens.
pub async fn reset(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
) -> Result<RevisionSummary, Error> {
    let mut source = default_source(tx, storage).await?;
    let parent = active_id(tx).await?;
    let tokens = sqlx::query_scalar!(
        "SELECT t.tokens FROM theme_active a JOIN theme_revisions r ON r.id = a.revision_id
         JOIN platform.theme_artifacts t ON t.id = r.artifact_id"
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    if let Some(t) = tokens.and_then(|t| archive::validate_tokens(&t).ok()) {
        source
            .files
            .insert(TOKENS_FILE.to_owned(), archive::tokens_file(&t));
    }
    create(tx, storage, actor, Change::Reset, parent, &source, None).await
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokensInput {
    /// The revision whose source is edited; default: the active revision.
    pub base_revision_id: Option<Uuid>,
    /// `{colors, fonts, radius}`: allowlisted keys and values (A6).
    #[schema(value_type = std::collections::HashMap<String, std::collections::HashMap<String, String>>)]
    pub tokens: Value,
}

/// A revision that changes only `theme.tokens.json` (fast-path gates).
pub async fn edit_tokens(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    input: &TokensInput,
) -> Result<RevisionSummary, Error> {
    let tokens = archive::validate_tokens(&input.tokens)
        .map_err(|e| invalid("invalid_tokens", format!("tokens: {e}")))?;
    let base = match input.base_revision_id {
        Some(id) => Some(id),
        None => active_id(tx).await?,
    };
    // The fast path skips the code gates, so the code must already have passed them.
    if let Some(id) = base {
        ensure_validated_base(tx, id).await?;
    }
    let mut source = match base {
        Some(id) => revision_source(tx, storage, id).await?,
        None => default_source(tx, storage).await?,
    };
    source
        .files
        .insert(TOKENS_FILE.to_owned(), archive::tokens_file(&tokens));
    create(tx, storage, actor, Change::Tokens, base, &source, None).await
}

/// A base for a token edit or an AI run must have passed the gates (ready or published, or
/// follow the platform's default), and an AI revision must have been accepted by the staff.
pub(crate) async fn ensure_validated_base(tx: &mut TenantTx, id: Uuid) -> Result<(), Error> {
    let b = fetch(tx, id, false).await?;
    if !(matches!(b.status.as_str(), "ready" | "published" | "superseded") || b.origin == "default")
    {
        return Err(Error::Conflict {
            code: "base_not_validated",
            detail: format!(
                "revision #{} is {}; start from a revision that passed the checks",
                b.number, b.status
            ),
        });
    }
    ensure_ai_accepted(tx, &b).await
}

/// An AI revision that was never published is usable only once its run was accepted, and only
/// the run's final revision (the one the staff reviewed).
async fn ensure_ai_accepted(tx: &mut TenantTx, r: &Row) -> Result<(), Error> {
    if r.change != Change::Ai.as_str() || r.published_at.is_some() {
        return Ok(());
    }
    let accepted = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ai_theme_runs
                          WHERE revision_id = $1 AND status = 'accepted') AS "ok!""#,
        r.id
    )
    .fetch_one(&mut **tx)
    .await?;
    if accepted {
        Ok(())
    } else {
        Err(Error::Conflict {
            code: "ai_run_not_accepted",
            detail: format!(
                "revision #{} comes from an AI edit that was not accepted; review and accept it first",
                r.number
            ),
        })
    }
}

/// A power user's source archive (validated per A6; contract violations fail the gates).
pub async fn upload(
    tx: &mut TenantTx,
    storage: &Storage,
    actor: &str,
    gz: &[u8],
) -> Result<RevisionSummary, Error> {
    let source = archive::read(gz).map_err(|e| Error::Validation {
        code: "invalid_archive",
        detail: e.problems.join("\n"),
    })?;
    let parent = active_id(tx).await?;
    create(tx, storage, actor, Change::Upload, parent, &source, None).await
}

/// A presigned download of the revision's source archive (custom revisions only).
pub async fn source_download(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
) -> Result<Download, Error> {
    let r = fetch(tx, id, false).await?;
    let key = r.source_key.ok_or(Error::NotFound)?;
    let url = storage
        .private_signer
        .signed_url(Method::GET, &Path::from(key), PRESIGN_TTL)
        .await?;
    Ok(Download {
        url: url.to_string(),
        expires_at: Utc::now() + chrono::Duration::seconds(PRESIGN_TTL.as_secs() as i64),
    })
}

/// Publishes a `ready` revision or rolls back to an earlier published (`superseded`) one: the
/// active pointer moves in one transaction; the caller purges the edge afterwards.
pub async fn publish(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<RevisionSummary, Error> {
    let previous = sqlx::query_scalar!("SELECT revision_id FROM theme_active FOR UPDATE")
        .fetch_optional(&mut **tx)
        .await?;
    let r = fetch(tx, id, true).await?;
    if previous == Some(id) {
        return Ok(r.summary());
    }
    if !matches!(r.status.as_str(), "ready" | "superseded") || r.artifact_id.is_none() {
        return Err(Error::Conflict {
            code: "not_publishable",
            detail: format!(
                "revision #{} is {}; only revisions that passed the checks can be published",
                r.number, r.status
            ),
        });
    }
    ensure_ai_accepted(tx, &r).await?;
    if let Some(p) = previous {
        sqlx::query!(
            "UPDATE theme_revisions SET status = 'superseded', superseded_at = now(),
                    status_changed_at = now() WHERE id = $1",
            p
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE theme_revisions SET status = 'published', published_at = now(),
                superseded_at = NULL, status_changed_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO theme_active (tenant_id, revision_id) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET revision_id = EXCLUDED.revision_id, updated_at = now()",
        tx.tenant_id(),
        id
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        if r.status == "superseded" {
            "theme.rolled_back"
        } else {
            "theme.published"
        },
        "theme_revision",
        Some(&id.to_string()),
        &json!({ "number": r.number, "artifact_id": r.artifact_id, "previous": previous }),
    )
    .await?;
    Ok(fetch(tx, id, false).await?.summary())
}

// ---------------------------------------------------------------------------------------
// Previews (A21)

/// `preview-<n>--<shop host>` → (revision number, shop host).
pub fn parse_preview_host(host: &str) -> Option<(i32, &str)> {
    let rest = host.strip_prefix("preview-")?;
    let (n, shop) = rest.split_once("--")?;
    if n.is_empty() || n.len() > 9 || !n.bytes().all(|b| b.is_ascii_digit()) || shop.is_empty() {
        return None;
    }
    Some((n.parse().ok()?, shop))
}

/// The preview host of revision `number` on `shop_host`.
pub fn preview_host(number: i32, shop_host: &str) -> String {
    format!("preview-{number}--{shop_host}")
}

/// The tenant's primary shop host (the primary domain of its first market).
async fn primary_host(tx: &mut TenantTx) -> Result<String, Error> {
    sqlx::query_scalar!(
        "SELECT d.hostname FROM platform.domains d JOIN markets m ON m.id = d.market_id
         WHERE d.tenant_id = $1 AND d.verified_at IS NOT NULL
         ORDER BY d.is_primary DESC, m.created_at, d.hostname LIMIT 1",
        tx.tenant_id()
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::Conflict {
        code: "no_domain",
        detail: "the shop has no verified domain to preview on".into(),
    })
}

/// A signed preview URL (1 h) for a revision that has an artifact.
pub async fn preview_link(
    tx: &mut TenantTx,
    keys: &ThemeKeys,
    urls: &PublicUrls,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<PreviewLink, Error> {
    let r = fetch(tx, id, false).await?;
    if r.artifact_id.is_none() {
        return Err(Error::Conflict {
            code: "not_built",
            detail: format!("revision #{} has no build to preview", r.number),
        });
    }
    let host = preview_host(r.number, &primary_host(tx).await?);
    let expires = now.timestamp() + PREVIEW_TTL_SECS;
    let token = keys.preview_token(tx.tenant_id(), id, expires);
    Ok(PreviewLink {
        url: format!("{}/?preview_token={token}", urls.base(&host)),
        expires_at: DateTime::from_timestamp(expires, 0).unwrap_or(now),
    })
}

/// What the edge serves on a preview host: the shop's site with the previewed artifact.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PreviewSite {
    pub site: Resolved,
    pub revision_id: Uuid,
    pub revision_number: i32,
    /// The token's expiry; the edge caches the answer at most until then.
    pub expires_at: DateTime<Utc>,
}

/// Resolves `preview-<n>--<shop>` + token: the token must be authentic for the shop's tenant,
/// unexpired, and name revision `n`, which must have an artifact.
pub async fn resolve_preview(
    db: &PgPool,
    keys: &ThemeKeys,
    host: &str,
    token: &str,
    now: DateTime<Utc>,
) -> Result<Option<PreviewSite>, Error> {
    let Some((number, shop)) = parse_preview_host(host) else {
        return Ok(None);
    };
    let Some(mut site) = tenancy::resolve_host(db, shop).await? else {
        return Ok(None);
    };
    let Some(revision_id) = keys.verify_preview(site.tenant_id, token, now.timestamp()) else {
        return Ok(None);
    };
    let expires: i64 = token
        .split('.')
        .nth(1)
        .and_then(|e| e.parse().ok())
        .unwrap_or_default();
    let mut tx = tenant_tx(db, site.tenant_id).await?;
    let artifact = sqlx::query_scalar!(
        "SELECT artifact_id FROM theme_revisions WHERE id = $1 AND number = $2",
        revision_id,
        number
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    tx.commit().await?;
    let Some(artifact) = artifact else {
        return Ok(None);
    };
    site.hostname = host.to_owned();
    site.theme_artifact = Some(artifact);
    site.retained_artifacts = Vec::new();
    Ok(Some(PreviewSite {
        site,
        revision_id,
        revision_number: number,
        expires_at: DateTime::from_timestamp(expires, 0).unwrap_or(now),
    }))
}

// ---------------------------------------------------------------------------------------
// Builder callbacks (`/internal/v1/themes/*`, THEME_BUILDER_TOKEN)

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BuildSpec {
    pub tenant_id: Uuid,
    pub revision_id: Uuid,
    pub number: i32,
    pub change: String,
    pub status: String,
    /// Token-only change: the heavy gates (types, Lighthouse, smoke) are skipped.
    pub tokens_only: bool,
    /// The tenant's `ASTRO_KEY` (hex of 32 bytes): reproducible builds.
    pub astro_key_hex: String,
}

pub async fn build_spec(tx: &mut TenantTx, keys: &ThemeKeys, id: Uuid) -> Result<BuildSpec, Error> {
    let r = fetch(tx, id, false).await?;
    if r.source_key.is_none() {
        return Err(Error::Conflict {
            code: "not_buildable",
            detail: "revision has no source".into(),
        });
    }
    Ok(BuildSpec {
        tenant_id: tx.tenant_id(),
        revision_id: id,
        number: r.number,
        tokens_only: r.change == Change::Tokens.as_str(),
        change: r.change,
        status: r.status,
        astro_key_hex: hex::encode(keys.astro_key(tx.tenant_id())),
    })
}

/// The stored source archive (`.tar.gz`) of a revision.
pub async fn source_archive(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
) -> Result<Vec<u8>, Error> {
    let key = fetch(tx, id, false)
        .await?
        .source_key
        .ok_or(Error::NotFound)?;
    Ok(storage
        .private
        .get(&Path::from(key))
        .await?
        .bytes()
        .await?
        .to_vec())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusUpdate {
    /// `building`, `ready` or `failed`.
    pub status: String,
    /// The gate report (JSON object, at most 256 kB).
    #[schema(value_type = Object)]
    pub checks: Value,
}

fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("draft", "building")
            | ("building", "building")
            | ("draft" | "building" | "checking", "failed")
            | ("checking", "ready")
    )
}

/// Builder callback: `draft → building`, `building|checking → failed`, `checking → ready`.
pub async fn builder_status(
    tx: &mut TenantTx,
    id: Uuid,
    update: &StatusUpdate,
) -> Result<RevisionSummary, Error> {
    if !update.checks.is_object()
        || serde_json::to_vec(&update.checks).map_or(usize::MAX, |b| b.len()) > MAX_CHECKS_BYTES
    {
        return Err(invalid(
            "invalid_checks",
            "checks must be a JSON object of at most 256 kB",
        ));
    }
    let r = fetch(tx, id, true).await?;
    if !transition_allowed(&r.status, &update.status) {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: format!(
                "revision #{} is {}, not {}",
                r.number, r.status, update.status
            ),
        });
    }
    sqlx::query!(
        "UPDATE theme_revisions SET status = $2, checks = $3, status_changed_at = now()
         WHERE id = $1",
        id,
        update.status,
        update.checks
    )
    .execute(&mut **tx)
    .await?;
    Ok(fetch(tx, id, false).await?.summary())
}

/// What the builder needs to run the browser gates against the preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CheckSpec {
    pub artifact_id: String,
    /// `preview-<n>--<shop>`: the checks reach it over TLS through the public proxy.
    pub preview_host: String,
    pub preview_token: String,
    /// Pages the budgets are measured on: home, a category, a product.
    pub pages: Vec<String>,
}

/// Registers the built artifact (content-addressed, A22), moves the revision to `checking`
/// and returns a preview token for the gates.
#[allow(clippy::too_many_arguments)]
pub async fn attach_artifact(
    db: &PgPool,
    storage: &Storage,
    keys: &ThemeKeys,
    tenant: Uuid,
    id: Uuid,
    artifact_id: &str,
    files: Vec<(String, Vec<u8>)>,
    now: DateTime<Utc>,
) -> Result<CheckSpec, Error> {
    let tokens = check_artifact(artifact_id, ArtifactKind::Theme, &files)?;
    {
        let mut tx = tenant_tx(db, tenant).await?;
        let r = fetch(&mut tx, id, false).await?;
        if r.status != "building" {
            return Err(Error::Conflict {
                code: "invalid_transition",
                detail: format!("revision #{} is {}, not building", r.number, r.status),
            });
        }
        tx.rollback().await?;
    }
    register_artifact(
        db,
        storage,
        artifact_id,
        ArtifactKind::Theme,
        tokens.as_ref(),
        files,
    )
    .await?;
    let mut tx = tenant_tx(db, tenant).await?;
    let r = fetch(&mut tx, id, true).await?;
    if r.status != "building" {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: format!("revision #{} is {}, not building", r.number, r.status),
        });
    }
    sqlx::query!(
        "UPDATE theme_revisions SET artifact_id = $2, status = 'checking', status_changed_at = now()
         WHERE id = $1",
        id,
        artifact_id
    )
    .execute(&mut *tx)
    .await?;
    let host = preview_host(r.number, &primary_host(&mut tx).await?);
    let pages = check_pages(&mut tx).await?;
    tx.commit().await?;
    Ok(CheckSpec {
        artifact_id: artifact_id.to_owned(),
        preview_host: host,
        preview_token: keys.preview_token(tenant, id, now.timestamp() + PREVIEW_TTL_SECS),
        pages,
    })
}

/// Home, the category with the most active products (a real listing page) and its first
/// product, in the primary market's default locale.
pub(crate) async fn check_pages(tx: &mut TenantTx) -> Result<Vec<String>, Error> {
    let r = sqlx::query!(
        r#"WITH m AS (SELECT default_locale FROM markets ORDER BY created_at LIMIT 1),
           cat AS (
               SELECT c.id, ct.slug, count(*) AS n, c.position
               FROM categories c
               JOIN m ON true
               JOIN category_translations ct ON ct.category_id = c.id AND ct.locale = m.default_locale
               JOIN product_categories pc ON pc.category_id = c.id
               JOIN products p ON p.id = pc.product_id AND p.status = 'active'
               GROUP BY c.id, ct.slug, c.position
               ORDER BY n DESC, c.position, ct.slug LIMIT 1)
           SELECT cat.slug AS "category!", pt.slug AS "product!"
           FROM cat
           JOIN m ON true
           JOIN product_categories pc ON pc.category_id = cat.id
           JOIN products p ON p.id = pc.product_id AND p.status = 'active'
           JOIN product_translations pt ON pt.product_id = p.id AND pt.locale = m.default_locale
           ORDER BY pc.position, p.created_at LIMIT 1"#
    )
    .fetch_optional(&mut **tx)
    .await?;
    let mut pages = vec!["/".to_owned()];
    if let Some(r) = r {
        pages.push(format!("/c/{}", r.category));
        pages.push(format!("/p/{}", r.product));
    }
    Ok(pages)
}

/// Stores a gate screenshot (PNG, one of [`SCREENSHOTS`]) of a revision being checked.
pub async fn store_screenshot(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
    name: &str,
    png: &[u8],
) -> Result<(), Error> {
    if !SCREENSHOTS.contains(&name) {
        return Err(Error::NotFound);
    }
    if png.len() > MAX_SCREENSHOT_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid(
            "invalid_screenshot",
            "expected a PNG of at most 5 MB",
        ));
    }
    let r = fetch(tx, id, false).await?;
    if r.status != "checking" {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: format!("revision #{} is {}, not checking", r.number, r.status),
        });
    }
    storage
        .private
        .put(
            &shot_key(tx.tenant_id(), id, name),
            PutPayload::from(png.to_vec()),
        )
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Maintenance (worker cron)

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Maintenance {
    pub expired_builds: u64,
    pub released_revisions: u64,
    pub deleted_artifacts: Vec<String>,
}

/// Fails builds that have not moved for [`STUCK_AFTER_MINUTES`], releases artifacts of failed
/// and surplus ready revisions after [`GC_AFTER_DAYS`], then deletes theme artifacts no
/// revision or channel references any more (published and superseded revisions keep theirs:
/// rollback targets and retained `/_astro/*` assets, A22).
pub async fn maintenance(db: &PgPool, storage: &Storage) -> Result<Maintenance, Error> {
    let mut out = Maintenance::default();
    let tenants = sqlx::query_scalar!("SELECT id FROM platform.tenants ORDER BY id")
        .fetch_all(db)
        .await?;
    let stuck = json!([format!(
        "the build did not finish within {STUCK_AFTER_MINUTES} minutes"
    )]);
    let expired = json!([format!(
        "the build was removed after {GC_AFTER_DAYS} days; create a new revision to publish it"
    )]);
    let mut referenced: Vec<String> = Vec::new();
    for tenant in tenants {
        let mut tx = tenant_tx(db, tenant).await?;
        out.expired_builds += sqlx::query!(
            "UPDATE theme_revisions
             SET status = 'failed', status_changed_at = now(),
                 checks = checks || jsonb_build_object('failures', $2::jsonb)
             WHERE status IN ('draft', 'building', 'checking')
               AND status_changed_at < now() - make_interval(mins => $1)",
            STUCK_AFTER_MINUTES,
            stuck
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        out.released_revisions += sqlx::query!(
            "UPDATE theme_revisions r
             SET artifact_id = NULL, status = 'failed', status_changed_at = now(),
                 checks = CASE WHEN r.status = 'ready'
                     THEN r.checks || jsonb_build_object('failures', $3::jsonb) ELSE r.checks END
             WHERE r.artifact_id IS NOT NULL
               AND r.status_changed_at < now() - make_interval(days => $1)
               AND (r.status = 'failed'
                    OR (r.status = 'ready' AND r.id NOT IN (
                        SELECT id FROM theme_revisions WHERE status = 'ready'
                        ORDER BY number DESC LIMIT $2)))",
            GC_AFTER_DAYS,
            KEEP_READY,
            expired
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        // Artifacts this tenant still references (RLS: collected tenant by tenant).
        referenced.extend(
            sqlx::query_scalar!(
                r#"SELECT DISTINCT artifact_id AS "id!" FROM theme_revisions
                   WHERE artifact_id IS NOT NULL"#
            )
            .fetch_all(&mut *tx)
            .await?,
        );
        tx.commit().await?;
    }
    let candidates = sqlx::query_scalar!(
        "SELECT id FROM platform.theme_artifacts a
         WHERE kind = 'theme' AND created_at < now() - make_interval(days => $1)
           AND NOT EXISTS (SELECT 1 FROM platform.artifact_channels c WHERE c.artifact_id = a.id)
           AND id <> ALL($2)
         ORDER BY created_at LIMIT 200",
        GC_AFTER_DAYS,
        &referenced
    )
    .fetch_all(db)
    .await?;
    for id in candidates {
        if delete_artifact(db, storage, &id).await? {
            out.deleted_artifacts.push(id);
        }
    }
    Ok(out)
}

/// Deletes an artifact unless a revision references it (the foreign key decides, across every
/// tenant). Serialized with registration by the same advisory lock; the row goes last-committed,
/// so a failed object deletion leaves it registered for the next run.
async fn delete_artifact(db: &PgPool, storage: &Storage, id: &str) -> Result<bool, Error> {
    let mut tx = db.begin().await?;
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('theme_artifact:' || $1, 0))",
        id
    )
    .execute(&mut *tx)
    .await?;
    match sqlx::query!("DELETE FROM platform.theme_artifacts WHERE id = $1", id)
        .execute(&mut *tx)
        .await
    {
        Ok(_) => {}
        Err(e) if e.as_database_error().and_then(|d| d.code()).as_deref() == Some("23503") => {
            return Ok(false);
        }
        Err(e) => return Err(e.into()),
    }
    let manifest = match storage.private.get(&object_key(id, "manifest.json")).await {
        Ok(r) => Some(r.bytes().await?),
        Err(object_store::Error::NotFound { .. }) => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(m) = manifest {
        let m: Value = serde_json::from_slice(&m).unwrap_or_default();
        let mut paths: Vec<String> = m["runtime"]["modules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(|p| format!("server/{p}")))
            .collect();
        paths.extend(
            m["assets"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(p, _)| format!("client{p}")),
        );
        paths.push("manifest.json".into());
        for p in paths.iter().filter(|p| super::artifact_path_valid(p)) {
            match storage.private.delete(&object_key(id, p)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_hosts() {
        assert_eq!(
            parse_preview_host("preview-7--demo.localhost"),
            Some((7, "demo.localhost"))
        );
        assert_eq!(preview_host(12, "shop.cz"), "preview-12--shop.cz");
        for bad in [
            "preview---demo.localhost",
            "preview-x--demo",
            "preview-7-demo",
            "demo.localhost",
            "preview-7--",
            "preview-1234567890--a",
        ] {
            assert_eq!(parse_preview_host(bad), None, "{bad}");
        }
    }

    #[test]
    fn transitions() {
        for (from, to) in [
            ("draft", "building"),
            ("building", "failed"),
            ("checking", "ready"),
            ("checking", "failed"),
            ("draft", "failed"),
        ] {
            assert!(transition_allowed(from, to), "{from} -> {to}");
        }
        for (from, to) in [
            ("draft", "ready"),
            ("building", "ready"),
            ("ready", "failed"),
            ("published", "building"),
            ("failed", "building"),
            ("checking", "published"),
        ] {
            assert!(!transition_allowed(from, to), "{from} -> {to}");
        }
    }
}
