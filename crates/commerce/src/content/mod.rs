//! Content (spec §7.5): CMS pages, legal pages and blog posts made of typed [`blocks`], menus,
//! and the legal templates + go-live checks ([`legal`], A29).
//!
//! Every change publishes [`PAGE_CHANGED_EVENT`] / [`MENU_CHANGED_EVENT`], which purge the
//! tenant's cached storefront pages (A2).

pub mod blocks;
pub mod legal;
pub mod menus;

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub use blocks::Block;

use crate::audit;
use crate::catalog::{I18n, check_opt_text, check_text, db_error, slug_valid};
use crate::markets::{invalid, is_locale};

pub const PAGE_CHANGED_EVENT: &str = "page.changed";
pub const MENU_CHANGED_EVENT: &str = "menu.changed";
pub const MAX_PAGE: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// A CMS page at `/pages/<slug>`.
    Page,
    /// A legal page (from a platform template) at `/pages/<slug>`.
    Legal,
    /// A blog post at `/blog/<slug>`.
    BlogPost,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Legal => "legal",
            Self::BlogPost => "blog_post",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "legal" => Self::Legal,
            "blog_post" => Self::BlogPost,
            _ => Self::Page,
        }
    }

    /// Storefront path of a page of this kind.
    pub fn path(self, slug: &str) -> String {
        match self {
            Self::BlogPost => format!("/blog/{slug}"),
            _ => format!("/pages/{slug}"),
        }
    }
}

/// Legal page types (spec §14): each has a platform template (not legal advice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LegalType {
    Terms,
    Privacy,
    Cookies,
    /// Withdrawal instructions and the model withdrawal form.
    Withdrawal,
    /// Complaints (warranty claims) procedure.
    Complaints,
    /// How reviews are verified (Omnibus; required once the shop publishes reviews).
    Reviews,
}

impl LegalType {
    pub const ALL: [Self; 6] = [
        Self::Terms,
        Self::Privacy,
        Self::Cookies,
        Self::Withdrawal,
        Self::Complaints,
        Self::Reviews,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terms => "terms",
            Self::Privacy => "privacy",
            Self::Cookies => "cookies",
            Self::Withdrawal => "withdrawal",
            Self::Complaints => "complaints",
            Self::Reviews => "reviews",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    #[default]
    Draft,
    Published,
}

impl PageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PageTranslation {
    #[schema(example = "cs")]
    pub locale: String,
    #[schema(example = "Doprava a platba")]
    pub title: String,
    /// Unique per tenant and locale across pages and posts.
    #[schema(example = "doprava-a-platba")]
    pub slug: String,
    /// Blog cards and meta description fallback.
    #[serde(default)]
    pub excerpt: String,
    #[serde(default)]
    pub blocks: Vec<Block>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
}

/// A page document as written by `POST /pages` and `PUT /pages/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PageInput {
    pub kind: PageKind,
    /// Required for (and only for) `legal` pages; one page per type.
    pub legal_type: Option<LegalType>,
    #[serde(default)]
    pub status: PageStatus,
    /// When a published page becomes visible; defaults to the first publication time.
    pub published_at: Option<DateTime<Utc>>,
    /// Cover image (blog cards, social previews).
    pub image_asset_id: Option<Uuid>,
    pub translations: Vec<PageTranslation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Page {
    pub id: Uuid,
    pub kind: PageKind,
    pub legal_type: Option<LegalType>,
    pub status: PageStatus,
    pub published_at: Option<DateTime<Utc>>,
    pub image_asset_id: Option<Uuid>,
    pub translations: Vec<PageTranslation>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageSummary {
    pub id: Uuid,
    pub kind: PageKind,
    pub legal_type: Option<LegalType>,
    pub status: PageStatus,
    pub published_at: Option<DateTime<Utc>>,
    /// locale -> title
    pub title: I18n,
    /// locale -> slug
    pub slug: I18n,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageList {
    pub items: Vec<PageSummary>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageFilter {
    pub kind: Option<PageKind>,
}

impl PageInput {
    pub fn validate(&self) -> Result<(), Error> {
        if (self.kind == PageKind::Legal) != self.legal_type.is_some() {
            return Err(invalid(
                "invalid_legal_type",
                "legal_type is required for legal pages and only for them",
            ));
        }
        if self.translations.is_empty() || self.translations.len() > 20 {
            return Err(invalid(
                "invalid_translations",
                "a page needs 1-20 translations",
            ));
        }
        let mut locales = BTreeSet::new();
        for t in &self.translations {
            if !is_locale(&t.locale) || !locales.insert(t.locale.as_str()) {
                return Err(invalid(
                    "invalid_translations",
                    format!("invalid or repeated locale {:?}", t.locale),
                ));
            }
            check_text("title", "invalid_title", &t.title, 1, 200)?;
            if !slug_valid(&t.slug) {
                return Err(invalid(
                    "invalid_slug",
                    "slug: lowercase ASCII words joined by hyphens",
                ));
            }
            check_text("excerpt", "invalid_excerpt", &t.excerpt, 0, 500)?;
            check_opt_text("seo_title", "invalid_seo", t.seo_title.as_deref(), 200)?;
            check_opt_text(
                "seo_description",
                "invalid_seo",
                t.seo_description.as_deref(),
                500,
            )?;
            blocks::normalize(&t.blocks)?;
        }
        Ok(())
    }
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

/// Assets and products referenced by blocks must exist in the tenant (RLS scopes the check).
async fn check_references(tx: &mut TenantTx, input: &PageInput) -> Result<(), Error> {
    let mut assets: Vec<Uuid> = input.image_asset_id.into_iter().collect();
    let mut products = vec![];
    for t in &input.translations {
        let (a, p) = blocks::references(&t.blocks);
        assets.extend(a);
        products.extend(p);
    }
    assets.sort();
    assets.dedup();
    products.sort();
    products.dedup();
    let known_assets = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM assets WHERE id = ANY($1)"#,
        &assets
    )
    .fetch_one(&mut **tx)
    .await?;
    let known_products = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM products WHERE id = ANY($1)"#,
        &products
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known_assets).ok() != Some(assets.len())
        || usize::try_from(known_products).ok() != Some(products.len())
    {
        return Err(invalid(
            "unknown_reference",
            "an image or product referenced by the page does not exist",
        ));
    }
    Ok(())
}

async fn save(tx: &mut TenantTx, id: Uuid, input: &PageInput) -> Result<(), Error> {
    let tenant_id = tx.tenant_id();
    check_references(tx, input).await?;
    // A published page keeps its first publication time unless one is given.
    sqlx::query!(
        "UPDATE pages SET kind = $2, legal_type = $3, status = $4,
                published_at = CASE WHEN $4 = 'published' THEN coalesce($5, published_at, now())
                                    ELSE coalesce($5, published_at) END,
                image_asset_id = $6, updated_at = now()
         WHERE id = $1",
        id,
        input.kind.as_str(),
        input.legal_type.map(LegalType::as_str),
        input.status.as_str(),
        input.published_at,
        input.image_asset_id,
    )
    .execute(&mut **tx)
    .await
    .map_err(page_db_error)?;
    sqlx::query!("DELETE FROM page_translations WHERE page_id = $1", id)
        .execute(&mut **tx)
        .await?;
    for t in &input.translations {
        let blocks = serde_json::to_value(blocks::normalize(&t.blocks)?).map_err(internal)?;
        sqlx::query!(
            "INSERT INTO page_translations (tenant_id, page_id, locale, title, slug, excerpt,
                 blocks, seo_title, seo_description)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            tenant_id,
            id,
            t.locale,
            t.title.trim(),
            t.slug,
            t.excerpt.trim(),
            blocks,
            t.seo_title.as_deref(),
            t.seo_description.as_deref(),
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

fn page_db_error(e: sqlx::Error) -> Error {
    if e.as_database_error().and_then(|d| d.constraint()) == Some("pages_legal_type") {
        Error::Conflict {
            code: "legal_page_exists",
            detail: "a legal page of this type already exists".into(),
        }
    } else {
        db_error(e)
    }
}

async fn changed(tx: &mut TenantTx, id: Uuid) -> Result<(), Error> {
    platform::queue::publish(&mut **tx, PAGE_CHANGED_EVENT, &json!({ "page_id": id })).await?;
    Ok(())
}

pub async fn create(tx: &mut TenantTx, actor: &str, input: &PageInput) -> Result<Page, Error> {
    input.validate()?;
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO pages (id, tenant_id, kind, legal_type) VALUES ($1, $2, $3, $4)",
        id,
        tx.tenant_id(),
        input.kind.as_str(),
        input.legal_type.map(LegalType::as_str),
    )
    .execute(&mut **tx)
    .await
    .map_err(page_db_error)?;
    save(tx, id, input).await?;
    let page = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "page.created",
        "page",
        Some(&id.to_string()),
        &json!({ "after": page }),
    )
    .await?;
    changed(tx, id).await?;
    Ok(page)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &PageInput,
) -> Result<Page, Error> {
    input.validate()?;
    sqlx::query_scalar!("SELECT id FROM pages WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let before = get(tx, id).await?;
    save(tx, id, input).await?;
    let after = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "page.updated",
        "page",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    changed(tx, id).await?;
    Ok(after)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM pages WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "page.deleted",
        "page",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    changed(tx, id).await?;
    Ok(())
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Page, Error> {
    let row = sqlx::query!(
        "SELECT id, kind, legal_type, status, published_at, image_asset_id, created_at, updated_at
         FROM pages WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let translations = sqlx::query!(
        "SELECT locale, title, slug, excerpt, blocks, seo_title, seo_description
         FROM page_translations WHERE page_id = $1 ORDER BY locale",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|t| {
        Ok(PageTranslation {
            locale: t.locale,
            title: t.title,
            slug: t.slug,
            excerpt: t.excerpt,
            blocks: serde_json::from_value(t.blocks).map_err(internal)?,
            seo_title: t.seo_title,
            seo_description: t.seo_description,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;
    Ok(Page {
        id: row.id,
        kind: PageKind::parse(&row.kind),
        legal_type: row.legal_type.as_deref().and_then(LegalType::parse),
        status: if row.status == "published" {
            PageStatus::Published
        } else {
            PageStatus::Draft
        },
        published_at: row.published_at,
        image_asset_id: row.image_asset_id,
        translations,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn list(
    tx: &mut TenantTx,
    filter: &PageFilter,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<PageList, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let rows = sqlx::query!(
        r#"SELECT p.id, p.kind, p.legal_type, p.status, p.published_at, p.updated_at,
                  coalesce((SELECT jsonb_object_agg(t.locale, t.title) FROM page_translations t
                            WHERE t.page_id = p.id), '{}') AS "title!",
                  coalesce((SELECT jsonb_object_agg(t.locale, t.slug) FROM page_translations t
                            WHERE t.page_id = p.id), '{}') AS "slug!"
           FROM pages p
           WHERE ($1::uuid IS NULL OR p.id < $1) AND ($2::text IS NULL OR p.kind = $2)
           ORDER BY p.id DESC LIMIT $3"#,
        cursor,
        filter.kind.map(PageKind::as_str),
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(|r| {
            Ok(PageSummary {
                id: r.id,
                kind: PageKind::parse(&r.kind),
                legal_type: r.legal_type.as_deref().and_then(LegalType::parse),
                status: if r.status == "published" {
                    PageStatus::Published
                } else {
                    PageStatus::Draft
                },
                published_at: r.published_at,
                title: serde_json::from_value(r.title).map_err(internal)?,
                slug: serde_json::from_value(r.slug).map_err(internal)?,
                updated_at: r.updated_at,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(PageList { items, next_cursor })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn input() -> PageInput {
        PageInput {
            kind: PageKind::Page,
            legal_type: None,
            status: PageStatus::Draft,
            published_at: None,
            image_asset_id: None,
            translations: vec![PageTranslation {
                locale: "cs".into(),
                title: "Kontakt".into(),
                slug: "kontakt".into(),
                excerpt: String::new(),
                blocks: vec![],
                seo_title: None,
                seo_description: None,
            }],
        }
    }

    #[test]
    fn validation() {
        assert!(input().validate().is_ok());
        let mut legal = input();
        legal.kind = PageKind::Legal;
        assert!(legal.validate().is_err(), "legal needs a type");
        legal.legal_type = Some(LegalType::Terms);
        assert!(legal.validate().is_ok());
        let mut typed = input();
        typed.legal_type = Some(LegalType::Terms);
        assert!(typed.validate().is_err(), "only legal pages have a type");
        let mut bad = input();
        bad.translations[0].slug = "Kontakt".into();
        assert!(bad.validate().is_err());
        let mut dup = input();
        dup.translations.push(dup.translations[0].clone());
        assert!(dup.validate().is_err());
        let mut none = input();
        none.translations.clear();
        assert!(none.validate().is_err());
    }

    #[test]
    fn paths_by_kind() {
        assert_eq!(PageKind::BlogPost.path("novinky"), "/blog/novinky");
        assert_eq!(
            PageKind::Legal.path("obchodni-podminky"),
            "/pages/obchodni-podminky"
        );
    }
}
