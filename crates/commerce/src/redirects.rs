//! URL redirects (spec §7.5, §9.5): the edge asks `GET /storefront/v1/redirects/resolve` when
//! a theme page is not found and answers with the redirect. Targets are paths on the same shop
//! only, so a redirect can never become an open redirect.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

pub const MAX_PAGE: i64 = 100;
const MAX_PATH: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedirectInput {
    /// Path on the shop, without query string: `/stara-kategorie`.
    #[schema(example = "/stary-produkt")]
    pub from_path: String,
    /// Path on the same shop, may carry a query string: `/p/novy-produkt`.
    #[schema(example = "/p/novy-produkt")]
    pub to_path: String,
    /// 301 (permanent, default) or 302.
    #[serde(default = "permanent")]
    pub code: u16,
}

fn permanent() -> u16 {
    301
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Redirect {
    pub id: Uuid,
    pub from_path: String,
    pub to_path: String,
    pub code: u16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RedirectPage {
    pub items: Vec<Redirect>,
    pub next_cursor: Option<Uuid>,
}

/// What the edge needs to answer a redirect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ResolvedRedirect {
    pub to_path: String,
    pub code: u16,
}

/// A shop path: starts with one `/`, no scheme/host, no whitespace, control characters or
/// backslashes (browsers treat `/\host` like `//host`).
fn path_ok(p: &str, allow_query: bool) -> bool {
    p.len() <= MAX_PATH
        && p.starts_with('/')
        && !p.starts_with("//")
        && !p.starts_with("/\\")
        && !p
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        && !p.contains('#')
        && (allow_query || !p.contains('?'))
}

/// Paths are matched without a trailing slash (except `/` itself), so `/a/` and `/a` are one.
pub fn normalize_path(p: &str) -> String {
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.into()
    }
}

impl RedirectInput {
    pub fn validate(&self) -> Result<(), Error> {
        if !path_ok(&self.from_path, false) {
            return Err(invalid(
                "invalid_from_path",
                "from_path must be a path on the shop (starting with /, no query string)",
            ));
        }
        if !path_ok(&self.to_path, true) {
            return Err(invalid(
                "invalid_to_path",
                "to_path must be a path on the same shop (starting with a single /)",
            ));
        }
        if normalize_path(&self.from_path) == normalize_path(&self.to_path) {
            return Err(invalid("redirect_loop", "to_path equals from_path"));
        }
        if self.code != 301 && self.code != 302 {
            return Err(invalid("invalid_code", "code must be 301 or 302"));
        }
        Ok(())
    }
}

struct Row {
    id: Uuid,
    from_path: String,
    to_path: String,
    code: i16,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<Row> for Redirect {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            from_path: r.from_path,
            to_path: r.to_path,
            code: u16::try_from(r.code).unwrap_or(301),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn db_error(e: sqlx::Error) -> Error {
    if e.as_database_error().and_then(|d| d.constraint()) == Some("redirects_from_unique") {
        Error::Conflict {
            code: "already_exists",
            detail: "a redirect for this path already exists".into(),
        }
    } else {
        e.into()
    }
}

fn code_i16(code: u16) -> i16 {
    i16::try_from(code).unwrap_or(301)
}

pub async fn list(
    tx: &mut TenantTx,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<RedirectPage, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let mut items: Vec<Redirect> = sqlx::query_as!(
        Row,
        "SELECT id, from_path, to_path, code, created_at, updated_at FROM redirects
         WHERE $1::uuid IS NULL OR id < $1 ORDER BY id DESC LIMIT $2",
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Redirect::from)
    .collect();
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(RedirectPage { items, next_cursor })
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Redirect, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, from_path, to_path, code, created_at, updated_at FROM redirects WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(Redirect::from)
    .ok_or(Error::NotFound)
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &RedirectInput,
) -> Result<Redirect, Error> {
    input.validate()?;
    let r: Redirect = sqlx::query_as!(
        Row,
        "INSERT INTO redirects (tenant_id, from_path, to_path, code) VALUES ($1, $2, $3, $4)
         RETURNING id, from_path, to_path, code, created_at, updated_at",
        tx.tenant_id(),
        normalize_path(&input.from_path),
        input.to_path,
        code_i16(input.code)
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?
    .into();
    audit::record(
        tx,
        actor,
        "redirect.created",
        "redirect",
        Some(&r.id.to_string()),
        &json!({ "after": r }),
    )
    .await?;
    Ok(r)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &RedirectInput,
) -> Result<Redirect, Error> {
    input.validate()?;
    let before = get(tx, id).await?;
    let after: Redirect = sqlx::query_as!(
        Row,
        "UPDATE redirects SET from_path = $2, to_path = $3, code = $4, updated_at = now()
         WHERE id = $1
         RETURNING id, from_path, to_path, code, created_at, updated_at",
        id,
        normalize_path(&input.from_path),
        input.to_path,
        code_i16(input.code)
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?
    .into();
    audit::record(
        tx,
        actor,
        "redirect.updated",
        "redirect",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM redirects WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "redirect.deleted",
        "redirect",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    Ok(())
}

/// The redirect for a requested path (query string ignored), if any.
pub async fn resolve(tx: &mut TenantTx, path: &str) -> Result<Option<ResolvedRedirect>, Error> {
    let path = path.split('?').next().unwrap_or_default();
    if !path_ok(path, false) {
        return Ok(None);
    }
    Ok(sqlx::query!(
        "SELECT to_path, code FROM redirects WHERE from_path = $1",
        normalize_path(path)
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|r| ResolvedRedirect {
        to_path: r.to_path,
        code: u16::try_from(r.code).unwrap_or(301),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(from: &str, to: &str) -> RedirectInput {
        RedirectInput {
            from_path: from.into(),
            to_path: to.into(),
            code: 301,
        }
    }

    #[test]
    fn only_same_shop_paths() {
        assert!(input("/old", "/p/new?x=1").validate().is_ok());
        for bad_to in [
            "https://evil.example/",
            "//evil.example",
            "/\\evil.example",
            "evil",
            "/a b",
            "/a\\b",
            "/a\n",
        ] {
            assert!(input("/old", bad_to).validate().is_err(), "{bad_to:?}");
        }
        for bad_from in ["old", "/old?x=1", "//x", "/a#b"] {
            assert!(input(bad_from, "/new").validate().is_err(), "{bad_from:?}");
        }
        assert!(input("/a/", "/a").validate().is_err(), "loop");
        let mut i = input("/a", "/b");
        i.code = 307;
        assert!(i.validate().is_err());
    }

    #[test]
    fn trailing_slashes_are_one_path() {
        assert_eq!(normalize_path("/a/"), "/a");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/a//"), "/a");
    }
}
