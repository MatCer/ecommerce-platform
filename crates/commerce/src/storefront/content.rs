//! Content page models (spec §7.5, §8.2): CMS and legal pages (`/pages/<slug>`), the blog
//! index and posts (`/blog`, `/blog/<slug>`), plus the menus and legal links of `/shop`.
//!
//! Only published pages whose `published_at` has passed are visible. The translation is picked
//! for the request locale, then the market's default locale, then any.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::cards::{self, ProductCard};
use super::images::{self, Image};
use super::pages::MenuItem;
use super::product::{breadcrumb_ld, home_link};
use super::{CacheHints, Context, Link, Seo, alternates, messages, plain_excerpt};
use crate::content::blocks::FaqItem;
use crate::content::menus::{MenuEntry, MenuLink};
use crate::content::{Block, LegalType, PageKind};
use crate::media::AssetVariant;

/// A block ready to render (images and products resolved, links localized).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockView {
    Heading {
        level: u8,
        text: String,
    },
    /// Sanitized HTML: the only block themes may render unescaped.
    RichText {
        html: String,
    },
    Image {
        image: Image,
        caption: String,
    },
    Button {
        label: String,
        href: String,
    },
    /// Purchasable products of the grid in the merchant's order (may be empty).
    ProductGrid {
        title: String,
        products: Vec<ProductCard>,
    },
    /// `answer_html` is sanitized HTML.
    Faq {
        items: Vec<FaqItem>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CmsPage {
    pub title: String,
    pub blocks: Vec<BlockView>,
    pub breadcrumbs: Vec<Link>,
    pub seo: Seo,
    pub cache: CacheHints,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BlogPostSummary {
    pub title: String,
    pub href: String,
    pub excerpt: String,
    pub published_at: DateTime<Utc>,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BlogIndex {
    pub title: String,
    /// Newest first (at most 50).
    pub posts: Vec<BlogPostSummary>,
    pub seo: Seo,
    pub cache: CacheHints,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct BlogPost {
    pub title: String,
    pub blocks: Vec<BlockView>,
    pub published_at: DateTime<Utc>,
    pub image: Option<Image>,
    pub breadcrumbs: Vec<Link>,
    pub seo: Seo,
    pub cache: CacheHints,
}

const BLOG_POSTS: i64 = 50;

struct Found {
    id: Uuid,
    kind: String,
    published_at: DateTime<Utc>,
    image: Option<Value>,
    title: String,
    slug: String,
    excerpt: String,
    blocks: Value,
    seo_title: Option<String>,
    seo_description: Option<String>,
}

/// The visible page with `slug` (in any locale) among `kinds`, in the best translation.
async fn find(
    tx: &mut TenantTx,
    ctx: &Context,
    slug: &str,
    kinds: &[&str],
) -> Result<Option<Found>, Error> {
    let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
    let Some(id) = sqlx::query_scalar!(
        "SELECT p.id FROM page_translations t JOIN pages p ON p.id = t.page_id
         WHERE t.slug = $1 AND p.kind = ANY($2) AND p.status = 'published'
           AND p.published_at <= $3
         ORDER BY (t.locale = $4) DESC, (t.locale = $5) DESC LIMIT 1",
        slug,
        &kinds,
        ctx.now,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    Ok(sqlx::query_as!(
        Found,
        r#"SELECT p.id, p.kind, p.published_at AS "published_at!", a.variants AS "image?",
                  t.title, t.slug, t.excerpt, t.blocks, t.seo_title, t.seo_description
           FROM pages p
           CROSS JOIN LATERAL (
               SELECT * FROM page_translations pt WHERE pt.page_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           LEFT JOIN assets a ON a.id = p.image_asset_id AND a.status = 'ready'
           WHERE p.id = $1"#,
        id,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_optional(&mut **tx)
    .await?)
}

fn image_of(variants: Option<&Value>, alt: &str) -> Option<Image> {
    let v: Vec<AssetVariant> = serde_json::from_value(variants?.clone()).ok()?;
    images::from_variants(&v, alt.to_owned())
}

/// Resolves stored blocks: images (ready assets only), product cards, localized shop paths.
async fn views(tx: &mut TenantTx, ctx: &Context, stored: &Value) -> Result<Vec<BlockView>, Error> {
    // Stored blocks were validated on write; a block that no longer parses is skipped.
    let blocks: Vec<Block> = stored
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| serde_json::from_value(b.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let (asset_ids, product_ids) = crate::content::blocks::references(&blocks);
    let assets: HashMap<Uuid, Value> = sqlx::query!(
        "SELECT id, variants FROM assets WHERE id = ANY($1) AND status = 'ready'",
        &asset_ids
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, r.variants))
    .collect();
    let cards: HashMap<Uuid, ProductCard> = cards::cards(tx, ctx, &product_ids)
        .await?
        .into_iter()
        .filter(|c| c.stock.purchasable())
        .map(|c| (c.id, c))
        .collect();
    let href = |h: &str| {
        if h.starts_with('/') {
            ctx.path(h)
        } else {
            h.to_owned()
        }
    };
    Ok(blocks
        .into_iter()
        .filter_map(|b| {
            Some(match b {
                Block::Heading { text, level } => BlockView::Heading { level, text },
                Block::RichText { html } => BlockView::RichText { html },
                Block::Image {
                    asset_id,
                    alt,
                    caption,
                } => BlockView::Image {
                    image: image_of(assets.get(&asset_id), &alt)?,
                    caption,
                },
                Block::Button { label, href: h } => BlockView::Button {
                    label,
                    href: href(&h),
                },
                Block::ProductGrid { title, product_ids } => BlockView::ProductGrid {
                    title,
                    products: product_ids
                        .iter()
                        .filter_map(|id| cards.get(id).cloned())
                        .collect(),
                },
                Block::Faq { items } => BlockView::Faq { items },
            })
        })
        .collect())
}

/// hreflang alternates of a page: its slug in each (market, locale) that has a translation.
async fn page_alternates(
    tx: &mut TenantTx,
    ctx: &Context,
    id: Uuid,
    kind: PageKind,
) -> Result<Vec<super::Alternate>, Error> {
    let slugs: HashMap<String, String> = sqlx::query!(
        "SELECT locale, slug FROM page_translations WHERE page_id = $1",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.locale, r.slug))
    .collect();
    Ok(alternates(ctx, |_, locale| {
        slugs.get(locale).map(|s| kind.path(s))
    }))
}

fn description(found: &Found, blocks: &[BlockView]) -> String {
    if let Some(d) = found.seo_description.clone().filter(|d| !d.is_empty()) {
        return d;
    }
    if !found.excerpt.is_empty() {
        return found.excerpt.clone();
    }
    let html: String = blocks
        .iter()
        .filter_map(|b| match b {
            BlockView::RichText { html } => Some(html.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    plain_excerpt(&html, 160)
}

/// `/pages/<slug>`: a CMS or legal page.
pub async fn cms_page(
    tx: &mut TenantTx,
    ctx: &Context,
    slug: &str,
) -> Result<Option<CmsPage>, Error> {
    let Some(found) = find(tx, ctx, slug, &["page", "legal"]).await? else {
        return Ok(None);
    };
    let blocks = views(tx, ctx, &found.blocks).await?;
    let kind = PageKind::parse(&found.kind);
    let path = kind.path(&found.slug);
    let breadcrumbs = vec![
        home_link(ctx),
        Link {
            label: found.title.clone(),
            href: ctx.path(&path),
        },
    ];
    let seo = Seo {
        title: found
            .seo_title
            .clone()
            .unwrap_or_else(|| found.title.clone()),
        description: description(&found, &blocks),
        canonical: ctx.page_url(&path),
        alternates: page_alternates(tx, ctx, found.id, kind).await?,
        json_ld: vec![
            json!({
                "@context": "https://schema.org",
                "@type": "WebPage",
                "name": found.title,
                "url": ctx.page_url(&path),
            }),
            breadcrumb_ld(ctx, &breadcrumbs),
        ],
        robots: None,
    };
    Ok(Some(CmsPage {
        title: found.title,
        blocks,
        breadcrumbs,
        seo,
        cache: CacheHints::public(300, vec![format!("page:{}", found.id)]),
    }))
}

/// `/blog`: published posts, newest first.
pub async fn blog_index(tx: &mut TenantTx, ctx: &Context) -> Result<BlogIndex, Error> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.published_at AS "published_at!", a.variants AS "image?",
                  t.title, t.slug, t.excerpt, t.blocks
           FROM pages p
           CROSS JOIN LATERAL (
               SELECT * FROM page_translations pt WHERE pt.page_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           LEFT JOIN assets a ON a.id = p.image_asset_id AND a.status = 'ready'
           WHERE p.kind = 'blog_post' AND p.status = 'published' AND p.published_at <= $1
           ORDER BY p.published_at DESC, p.id DESC LIMIT $4"#,
        ctx.now,
        ctx.locale,
        ctx.market.default_locale,
        BLOG_POSTS
    )
    .fetch_all(&mut **tx)
    .await?;
    let posts = rows
        .into_iter()
        .map(|r| {
            let excerpt = if r.excerpt.is_empty() {
                let blocks: Vec<Block> = serde_json::from_value(r.blocks).unwrap_or_default();
                plain_excerpt(&crate::content::blocks::plain_text(&blocks), 200)
            } else {
                r.excerpt
            };
            BlogPostSummary {
                href: ctx.path(&PageKind::BlogPost.path(&r.slug)),
                image: image_of(r.image.as_ref(), &r.title),
                title: r.title,
                excerpt,
                published_at: r.published_at,
            }
        })
        .collect();
    let title = messages::text(&ctx.locale, "blog.title").to_owned();
    Ok(BlogIndex {
        seo: Seo {
            title: format!("{title} | {}", ctx.shop_name),
            description: String::new(),
            canonical: ctx.page_url("/blog"),
            alternates: alternates(ctx, |_, _| Some("/blog".into())),
            json_ld: vec![json!({
                "@context": "https://schema.org",
                "@type": "Blog",
                "name": format!("{title} | {}", ctx.shop_name),
                "url": ctx.page_url("/blog"),
            })],
            robots: None,
        },
        title,
        posts,
        cache: CacheHints::public(300, vec!["blog".into()]),
    })
}

/// `/blog/<slug>`: one post.
pub async fn blog_post(
    tx: &mut TenantTx,
    ctx: &Context,
    slug: &str,
) -> Result<Option<BlogPost>, Error> {
    let Some(found) = find(tx, ctx, slug, &["blog_post"]).await? else {
        return Ok(None);
    };
    let blocks = views(tx, ctx, &found.blocks).await?;
    let kind = PageKind::parse(&found.kind);
    let path = kind.path(&found.slug);
    let breadcrumbs = vec![
        home_link(ctx),
        Link {
            label: messages::text(&ctx.locale, "blog.title").to_owned(),
            href: ctx.path("/blog"),
        },
        Link {
            label: found.title.clone(),
            href: ctx.path(&path),
        },
    ];
    let image = image_of(found.image.as_ref(), &found.title);
    let seo = Seo {
        title: found
            .seo_title
            .clone()
            .unwrap_or_else(|| found.title.clone()),
        description: description(&found, &blocks),
        canonical: ctx.page_url(&path),
        alternates: page_alternates(tx, ctx, found.id, kind).await?,
        json_ld: vec![
            json!({
                "@context": "https://schema.org",
                "@type": "BlogPosting",
                "headline": found.title,
                "datePublished": found.published_at,
                "url": ctx.page_url(&path),
                "image": image.as_ref().map(|i| ctx.url(&i.src)),
                "publisher": { "@type": "Organization", "name": ctx.shop_name },
            }),
            breadcrumb_ld(ctx, &breadcrumbs),
        ],
        robots: None,
    };
    Ok(Some(BlogPost {
        title: found.title,
        blocks,
        published_at: found.published_at,
        image,
        breadcrumbs,
        seo,
        cache: CacheHints::public(300, vec![format!("page:{}", found.id), "blog".into()]),
    }))
}

// ---------------------------------------------------------------------------------------
// Menus and legal links for `/shop`

struct Target {
    label: String,
    href: String,
}

/// Resolves menu entries: labels in the request locale (else the target's name), hrefs to
/// active products, categories and visible pages; entries whose target is gone are dropped.
pub async fn resolve_menu(
    tx: &mut TenantTx,
    ctx: &Context,
    entries: &[MenuEntry],
) -> Result<Vec<MenuItem>, Error> {
    let all: Vec<&MenuEntry> = entries
        .iter()
        .flat_map(|e| std::iter::once(e).chain(&e.children))
        .collect();
    let ids = |f: fn(&MenuLink) -> Option<Uuid>| -> Vec<Uuid> {
        all.iter().filter_map(|e| f(&e.link)).collect()
    };
    let categories = ids(|l| match l {
        MenuLink::Category { id } => Some(*id),
        _ => None,
    });
    let products = ids(|l| match l {
        MenuLink::Product { id } => Some(*id),
        _ => None,
    });
    let pages = ids(|l| match l {
        MenuLink::Page { id } => Some(*id),
        _ => None,
    });
    let mut targets: HashMap<Uuid, Target> = HashMap::new();
    for r in sqlx::query!(
        r#"SELECT c.id, t.name AS "name!", t.slug AS "slug!" FROM categories c
           CROSS JOIN LATERAL (
               SELECT name, slug FROM category_translations ct WHERE ct.category_id = c.id
               ORDER BY (ct.locale = $2) DESC, (ct.locale = $3) DESC, ct.locale LIMIT 1
           ) t WHERE c.id = ANY($1)"#,
        &categories,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    {
        targets.insert(
            r.id,
            Target {
                label: r.name,
                href: ctx.path(&format!("/c/{}", r.slug)),
            },
        );
    }
    for r in sqlx::query!(
        r#"SELECT p.id, t.name AS "name!", t.slug AS "slug!" FROM products p
           CROSS JOIN LATERAL (
               SELECT name, slug FROM product_translations pt WHERE pt.product_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t WHERE p.id = ANY($1) AND p.status = 'active'"#,
        &products,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    {
        targets.insert(
            r.id,
            Target {
                label: r.name,
                href: ctx.path(&format!("/p/{}", r.slug)),
            },
        );
    }
    for r in sqlx::query!(
        r#"SELECT p.id, p.kind, t.title AS "title!", t.slug AS "slug!"
           FROM pages p
           CROSS JOIN LATERAL (
               SELECT title, slug FROM page_translations pt WHERE pt.page_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           WHERE p.id = ANY($1) AND p.status = 'published' AND p.published_at <= $4"#,
        &pages,
        ctx.locale,
        ctx.market.default_locale,
        ctx.now
    )
    .fetch_all(&mut **tx)
    .await?
    {
        targets.insert(
            r.id,
            Target {
                label: r.title,
                href: ctx.path(&PageKind::parse(&r.kind).path(&r.slug)),
            },
        );
    }
    let resolve = |e: &MenuEntry| -> Option<Link> {
        let label = serde_json::to_value(&e.label_i18n)
            .ok()
            .and_then(|v| ctx.text(&v));
        let (fallback, href) = match &e.link {
            MenuLink::Category { id } | MenuLink::Product { id } | MenuLink::Page { id } => {
                let t = targets.get(id)?;
                (Some(t.label.clone()), t.href.clone())
            }
            MenuLink::Url { url } if url.starts_with('/') => (None, ctx.path(url)),
            MenuLink::Url { url } => (None, url.clone()),
        };
        Some(Link {
            label: label.or(fallback)?,
            href,
        })
    };
    Ok(entries
        .iter()
        .filter_map(|e| {
            let top = resolve(e)?;
            Some(MenuItem {
                label: top.label,
                href: top.href,
                children: e.children.iter().filter_map(&resolve).collect(),
            })
        })
        .collect())
}

/// Published legal pages for the footer (the cookies page is the consent `policy_url`), in
/// the order of [`LegalType::ALL`], plus the cookies page path if it is published.
pub async fn legal_links(
    tx: &mut TenantTx,
    ctx: &Context,
) -> Result<(Vec<Link>, Option<String>), Error> {
    let rows = sqlx::query!(
        r#"SELECT p.legal_type AS "legal_type!", t.title AS "title!", t.slug AS "slug!"
           FROM pages p
           CROSS JOIN LATERAL (
               SELECT title, slug FROM page_translations pt WHERE pt.page_id = p.id
               ORDER BY (pt.locale = $1) DESC, (pt.locale = $2) DESC, pt.locale LIMIT 1
           ) t
           WHERE p.kind = 'legal' AND p.status = 'published' AND p.published_at <= $3"#,
        ctx.locale,
        ctx.market.default_locale,
        ctx.now
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut links: Vec<(LegalType, Link)> = rows
        .into_iter()
        .filter_map(|r| {
            Some((
                LegalType::parse(&r.legal_type)?,
                Link {
                    label: r.title,
                    href: ctx.path(&PageKind::Legal.path(&r.slug)),
                },
            ))
        })
        .collect();
    links.sort_by_key(|(t, _)| *t);
    let cookies = links
        .iter()
        .find(|(t, _)| *t == LegalType::Cookies)
        .map(|(_, l)| l.href.clone());
    Ok((
        links
            .into_iter()
            .filter(|(t, _)| *t != LegalType::Cookies)
            .map(|(_, l)| l)
            .collect(),
        cookies,
    ))
}
