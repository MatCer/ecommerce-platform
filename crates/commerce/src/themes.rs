//! Theme artifacts and revisions (spec §7.5, §9.7, A22, A30).
//!
//! An artifact is an immutable, content-addressed build (`theme-kit pack`); its files live in
//! the private bucket under `artifacts/<id>/`. Channels name the platform's current artifacts
//! (`default-theme`, `checkout`). Each tenant has revisions pointing at artifacts and one active
//! revision. M1 ships one shared default artifact: publishing a new default appends a revision
//! to every tenant that follows the default (`origin = 'default'`) and activates it.

use chrono::{DateTime, Utc};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::storage::Storage;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

pub const DEFAULT_THEME: &str = "default-theme";
pub const CHECKOUT: &str = "checkout";

/// Earlier artifacts stay reachable for `/_astro/*` (A22): the previous 3 revisions or those
/// superseded within 7 days, whichever keeps more.
const RETAIN_REVISIONS: i64 = 3;
const RETAIN_DAYS: i32 = 7;

pub fn artifact_id_valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A file path inside an artifact: `manifest.json`, `server/...`, `client/...`; plain segments
/// only.
pub fn artifact_path_valid(path: &str) -> bool {
    path.len() <= 512
        && (path == "manifest.json" || path.starts_with("server/") || path.starts_with("client/"))
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@+~".contains(&b))
        })
}

pub fn object_key(id: &str, path: &str) -> Path {
    Path::from(format!("artifacts/{id}/{path}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Theme,
    Checkout,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Checkout => "checkout",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "theme" => Some(Self::Theme),
            "checkout" => Some(Self::Checkout),
            _ => None,
        }
    }
}

/// Uploads an unpacked artifact (`(relative path, bytes)` pairs, incl. `manifest.json`) to the
/// private bucket and registers it. Idempotent: the id is a content address.
pub async fn register_artifact(
    db: &PgPool,
    storage: &Storage,
    id: &str,
    kind: ArtifactKind,
    tokens: Option<&Value>,
    files: Vec<(String, Vec<u8>)>,
) -> Result<(), Error> {
    if !artifact_id_valid(id) {
        return Err(invalid(
            "invalid_artifact",
            "artifact id must be 32 hex characters",
        ));
    }
    if !files.iter().any(|(p, _)| p == "manifest.json") {
        return Err(invalid("invalid_artifact", "manifest.json is missing"));
    }
    if let Some((path, _)) = files.iter().find(|(p, _)| !artifact_path_valid(p)) {
        return Err(invalid(
            "invalid_artifact",
            format!("unexpected artifact file {path:?}"),
        ));
    }
    // Registered artifacts are immutable (edges may run them): publishing the same id again
    // must bring exactly the stored bytes, and nothing is written.
    if artifact_exists(db, id).await? {
        for (path, bytes) in &files {
            let stored = match storage.private.get(&object_key(id, path)).await {
                Ok(r) => Some(r.bytes().await?),
                Err(object_store::Error::NotFound { .. }) => None,
                Err(e) => return Err(e.into()),
            };
            if stored.as_deref() != Some(bytes.as_slice()) {
                return Err(Error::Conflict {
                    code: "artifact_mismatch",
                    detail: format!("artifact {id} is already published with different {path}"),
                });
            }
        }
        return Ok(());
    }
    for (path, bytes) in files {
        storage
            .private
            .put(&object_key(id, &path), PutPayload::from(bytes))
            .await?;
    }
    // Registered only after every file is stored, so a registered artifact is complete.
    sqlx::query!(
        "INSERT INTO platform.theme_artifacts (id, kind, tokens) VALUES ($1, $2, $3)
         ON CONFLICT (id) DO NOTHING",
        id,
        kind.as_str(),
        tokens
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn artifact_exists(db: &PgPool, id: &str) -> Result<bool, Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM platform.theme_artifacts WHERE id = $1) AS "x!""#,
        id
    )
    .fetch_one(db)
    .await?)
}

pub async fn set_channel(db: &PgPool, name: &str, artifact_id: &str) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO platform.artifact_channels (name, artifact_id) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET artifact_id = EXCLUDED.artifact_id, updated_at = now()",
        name,
        artifact_id
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn channel(db: &PgPool, name: &str) -> Result<Option<String>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT artifact_id FROM platform.artifact_channels WHERE name = $1",
        name
    )
    .fetch_optional(db)
    .await?)
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Revision {
    pub id: Uuid,
    pub number: i32,
    pub artifact_id: String,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
}

/// Points the tenant at `artifact_id` with a new published revision, unless its active
/// revision is custom (M3) or already uses that artifact. Returns the new revision.
pub async fn activate_default(
    tx: &mut TenantTx,
    actor: &str,
    artifact_id: &str,
) -> Result<Option<Revision>, Error> {
    let current = sqlx::query!(
        "SELECT r.id, r.artifact_id, r.origin FROM theme_active a
         JOIN theme_revisions r ON r.id = a.revision_id
         FOR UPDATE OF a",
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(c) = &current
        && (c.origin == "custom" || c.artifact_id == artifact_id)
    {
        return Ok(None);
    }
    let parent = current.as_ref().map(|c| c.id);
    let rev = sqlx::query_as!(
        Revision,
        "INSERT INTO theme_revisions (tenant_id, number, parent_id, artifact_id, origin, status,
                                      created_by, published_at)
         SELECT $1, coalesce(max(number), 0) + 1, $2, $3, 'default', 'published', $4, now()
         FROM theme_revisions
         RETURNING id, number, artifact_id, status, published_at",
        tx.tenant_id(),
        parent,
        artifact_id,
        actor
    )
    .fetch_one(&mut **tx)
    .await?;
    if let Some(p) = parent {
        sqlx::query!(
            "UPDATE theme_revisions SET status = 'superseded', superseded_at = now() WHERE id = $1",
            p
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!(
        "INSERT INTO theme_active (tenant_id, revision_id) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET revision_id = EXCLUDED.revision_id, updated_at = now()",
        tx.tenant_id(),
        rev.id
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "theme.published",
        "theme_revision",
        Some(&rev.id.to_string()),
        &json!({ "number": rev.number, "artifact_id": artifact_id, "parent_id": parent }),
    )
    .await?;
    Ok(Some(rev))
}

/// A new tenant starts on the current default artifact, if one is published.
pub async fn assign_default(tx: &mut TenantTx, actor: &str) -> Result<(), Error> {
    // Read through the tenant's own connection: callers may hold the only pooled one.
    let channel = sqlx::query_scalar!(
        "SELECT artifact_id FROM platform.artifact_channels WHERE name = $1",
        DEFAULT_THEME
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = channel {
        activate_default(tx, actor, &id).await?;
    }
    Ok(())
}

/// Publishes `artifact_id` as the default theme: the channel moves and every tenant following
/// the default gets a new active revision (one transaction per tenant). Returns the tenants
/// that changed.
pub async fn publish_default(
    db: &PgPool,
    actor: &str,
    artifact_id: &str,
) -> Result<Vec<Uuid>, Error> {
    let kind = sqlx::query_scalar!(
        "SELECT kind FROM platform.theme_artifacts WHERE id = $1",
        artifact_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound)?;
    if kind != ArtifactKind::Theme.as_str() {
        return Err(invalid("invalid_artifact", "not a theme artifact"));
    }
    set_channel(db, DEFAULT_THEME, artifact_id).await?;
    let tenants = sqlx::query_scalar!("SELECT id FROM platform.tenants ORDER BY id")
        .fetch_all(db)
        .await?;
    let mut changed = Vec::new();
    for tenant_id in tenants {
        let mut tx = tenant_tx(db, tenant_id).await?;
        if activate_default(&mut tx, actor, artifact_id)
            .await?
            .is_some()
        {
            changed.push(tenant_id);
        }
        tx.commit().await?;
    }
    Ok(changed)
}

/// The tenant's active artifact and the earlier ones whose assets stay served (A22).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ActiveTheme {
    pub artifact_id: Option<String>,
    pub retained: Vec<String>,
}

pub async fn active(tx: &mut TenantTx) -> Result<ActiveTheme, Error> {
    let Some(current) = sqlx::query!(
        "SELECT r.artifact_id, r.number FROM theme_active a
         JOIN theme_revisions r ON r.id = a.revision_id"
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(ActiveTheme::default());
    };
    let retained = sqlx::query_scalar!(
        "SELECT artifact_id FROM theme_revisions
         WHERE artifact_id <> $1 AND status = 'superseded'
           AND (number >= $2::int - $3::int
                OR superseded_at > now() - make_interval(days => $4::int))
         GROUP BY artifact_id ORDER BY max(number) DESC",
        current.artifact_id,
        current.number,
        i32::try_from(RETAIN_REVISIONS).unwrap_or(3),
        RETAIN_DAYS
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(ActiveTheme {
        artifact_id: Some(current.artifact_id),
        retained,
    })
}

/// Design tokens of the tenant's active artifact (for `GET /shop`).
pub async fn active_tokens(tx: &mut TenantTx) -> Result<Option<Value>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT t.tokens FROM theme_active a
         JOIN theme_revisions r ON r.id = a.revision_id
         JOIN platform.theme_artifacts t ON t.id = r.artifact_id"
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_paths() {
        assert!(artifact_id_valid("0123456789abcdef0123456789abcdef"));
        assert!(!artifact_id_valid("0123456789ABCDEF0123456789abcdef"));
        assert!(!artifact_id_valid("../x"));
        for ok in [
            "manifest.json",
            "server/entry.mjs",
            "client/_astro/app.X1_y-z.js",
            "client/favicon.svg",
        ] {
            assert!(artifact_path_valid(ok), "{ok}");
        }
        for bad in [
            "",
            "etc/passwd",
            "server/../manifest.json",
            "client//x",
            "client/./x",
            "client/a b",
            "/server/entry.mjs",
            "server\\x",
        ] {
            assert!(!artifact_path_valid(bad), "{bad}");
        }
    }
}
