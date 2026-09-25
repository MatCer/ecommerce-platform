//! Page models: shop (layout data), home, category and search listings, recommendations and
//! search suggestions (spec §8.2). The product page is in [`super::product`].

use std::collections::{BTreeMap, BTreeSet};

use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::cards::{self, ProductCard};
use super::images::Image;
use super::listing::{self, Facet, FacetKind, ListingQuery, PER_PAGE, Sort};
use super::product::{breadcrumb_ld, category_trail, home_link};
use super::{
    Alternate, CacheHints, Context, Link, Search, Seo, alternates, locale_path, messages,
    plain_excerpt,
};
use crate::media::AssetVariant;
use crate::money::MoneyView;
use crate::themes;

// ---------------------------------------------------------------------------------------
// Shop

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MenuItem {
    pub label: String,
    pub href: String,
    pub children: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Menus {
    pub main: Vec<MenuItem>,
    pub footer: Vec<MenuItem>,
}

pub use crate::consent::ConsentPurpose;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConsentConfig {
    /// Purposes the banner asks for (A20).
    pub purposes: Vec<ConsentPurpose>,
    pub policy_url: String,
    /// Version of the consent texts; post it back with the choice (`POST /_p/consent`).
    pub text_version: String,
    /// The preferences page on the checkout origin.
    pub preferences_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Tracking {
    /// Share of consented page views that report Web Vitals (spec §9.6).
    pub rum_sample_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Trust {
    pub delivery: String,
    pub returns: String,
    pub payments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MarketLink {
    pub code: String,
    pub name: String,
    /// hreflang value (`sk-SK`).
    pub locale: String,
    pub currency: String,
    /// Home page of the market.
    pub href: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ShopModel {
    pub name: String,
    /// Active locale (`cs`); also the `lang` of pages.
    pub locale: String,
    pub currency: String,
    /// `""` in the market's default locale, else `/<locale>` (spec §9.1). Page-model hrefs
    /// already carry it; themes prefix the links they build themselves (`/search`,
    /// `/p/<slug>`, `/_p/public/*`).
    pub base_path: String,
    /// Locales of this market, the default first, each with its home page URL.
    pub locales: Vec<Alternate>,
    /// The checkout origin (`https://checkout.<shop host>`, A1): account, order status and the
    /// withdrawal form (`/withdraw`, A19) live there, not on the theme's origin.
    pub checkout_url: String,
    pub currencies: Vec<String>,
    /// The tenant's markets (other shops of the same merchant).
    pub markets: Vec<MarketLink>,
    pub menus: Menus,
    /// Placeholders until CMS pages exist (WP13).
    pub legal_pages: Vec<Link>,
    pub consent: ConsentConfig,
    /// Free shipping from this order value; `None` until shipping methods exist (WP10).
    pub free_shipping_threshold: Option<MoneyView>,
    pub tracking: Tracking,
    pub trust: Trust,
    /// Design tokens of the active theme (A6).
    pub tokens: Option<Value>,
    /// Platform UI messages in the active locale (`cart.add` -> "Přidat do košíku").
    pub messages: BTreeMap<String, String>,
    pub seo: Seo,
    pub cache: CacheHints,
}

fn t(ctx: &Context, key: &str) -> String {
    messages::text(&ctx.locale, key).to_owned()
}

fn link(ctx: &Context, key: &str, href: &str) -> Link {
    Link {
        label: t(ctx, key),
        href: ctx.path(href),
    }
}

struct CategoryRow {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    slug: String,
    image: Option<Value>,
}

/// Top two levels of the category tree in the request locale.
async fn category_tree(tx: &mut TenantTx, ctx: &Context) -> Result<Vec<CategoryRow>, Error> {
    Ok(sqlx::query_as!(
        CategoryRow,
        r#"SELECT c.id, c.parent_id, t.name AS "name!", t.slug AS "slug!", a.variants AS "image?"
           FROM categories c
           CROSS JOIN LATERAL (
               SELECT name, slug FROM category_translations ct WHERE ct.category_id = c.id
               ORDER BY (ct.locale = $1) DESC, (ct.locale = $2) DESC, ct.locale LIMIT 1
           ) t
           LEFT JOIN assets a ON a.id = c.image_asset_id AND a.status = 'ready'
           WHERE c.parent_id IS NULL
              OR c.parent_id IN (SELECT id FROM categories WHERE parent_id IS NULL)
           ORDER BY c.position, c.id"#,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?)
}

pub async fn shop(tx: &mut TenantTx, ctx: &Context) -> Result<ShopModel, Error> {
    let tree = category_tree(tx, ctx).await?;
    let main = tree
        .iter()
        .filter(|c| c.parent_id.is_none())
        .map(|c| MenuItem {
            label: c.name.clone(),
            href: ctx.path(&format!("/c/{}", c.slug)),
            children: tree
                .iter()
                .filter(|k| k.parent_id == Some(c.id))
                .map(|k| Link {
                    label: k.name.clone(),
                    href: ctx.path(&format!("/c/{}", k.slug)),
                })
                .collect(),
        })
        .collect();
    let footer = [
        ("menu.shipping", "/pages/doprava-a-platba"),
        ("menu.returns", "/pages/reklamace-a-vraceni"),
        ("menu.contact", "/pages/kontakt"),
    ]
    .iter()
    .map(|(k, href)| MenuItem {
        label: t(ctx, k),
        href: ctx.path(href),
        children: Vec::new(),
    })
    .collect();
    let markets = ctx
        .markets
        .iter()
        .filter_map(|m| {
            Some(MarketLink {
                code: m.code.clone(),
                name: m.name.clone(),
                locale: m.hreflang(),
                currency: m.currency.code().into(),
                href: format!("{}/", m.base_url.as_ref()?),
                current: m.id == ctx.market.id,
            })
        })
        .collect();
    let tokens = themes::active_tokens(tx).await?;
    Ok(ShopModel {
        name: ctx.shop_name.clone(),
        locale: ctx.locale.clone(),
        currency: ctx.market.currency.code().into(),
        base_path: ctx.base_path(),
        checkout_url: ctx.base_url.replacen("://", "://checkout.", 1),
        locales: ctx
            .market
            .locales_default_first()
            .map(|l| Alternate {
                locale: l.to_owned(),
                href: ctx.url(&locale_path(&ctx.market.default_locale, l, "/")),
            })
            .collect(),
        currencies: vec![ctx.market.currency.code().into()],
        markets,
        menus: Menus { main, footer },
        legal_pages: vec![
            link(ctx, "legal.terms", "/pages/obchodni-podminky"),
            link(ctx, "legal.privacy", "/pages/ochrana-osobnich-udaju"),
        ],
        consent: ConsentConfig {
            purposes: vec![
                ConsentPurpose::Analytics,
                ConsentPurpose::Ads,
                ConsentPurpose::Personalization,
            ],
            policy_url: ctx.path("/pages/cookies"),
            text_version: crate::consent::TEXT_VERSION.into(),
            preferences_url: ctx.checkout_url("/consent"),
        },
        free_shipping_threshold: None,
        tracking: Tracking {
            rum_sample_rate: 0.1,
        },
        trust: Trust {
            delivery: t(ctx, "trust.delivery"),
            returns: t(ctx, "trust.returns"),
            payments: t(ctx, "trust.payments")
                .split(" · ")
                .map(str::to_owned)
                .collect(),
        },
        tokens,
        messages: messages::catalog(&ctx.locale).clone(),
        seo: Seo {
            title: ctx.shop_name.clone(),
            description: String::new(),
            canonical: ctx.page_url("/"),
            alternates: alternates(ctx, |_, _| Some("/".into())),
            json_ld: vec![serde_json::json!({
                "@context": "https://schema.org",
                "@type": "Organization",
                "name": ctx.shop_name,
                "url": ctx.page_url("/"),
            })],
            robots: None,
        },
        cache: CacheHints::public(300, vec!["shop".into()]),
    })
}

// ---------------------------------------------------------------------------------------
// Home

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Hero {
    pub title: String,
    pub subtitle: String,
    pub image: Option<Image>,
    pub cta: Link,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategoryTile {
    pub label: String,
    pub href: String,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct HomePage {
    /// Placeholder built from the catalog until CMS blocks exist (WP13).
    pub hero: Hero,
    pub categories: Vec<CategoryTile>,
    pub featured: Vec<ProductCard>,
    pub seo: Seo,
    pub cache: CacheHints,
}

fn image_from(variants: &Value, alt: &str) -> Option<Image> {
    let v: Vec<AssetVariant> = serde_json::from_value(variants.clone()).ok()?;
    super::images::from_variants(&v, alt.to_owned())
}

/// The first product image in a category subtree (category tiles without their own image).
async fn category_image(
    tx: &mut TenantTx,
    category_id: Uuid,
    alt: &str,
) -> Result<Option<Image>, Error> {
    let variants = sqlx::query_scalar!(
        r#"WITH RECURSIVE subtree AS (
               SELECT id FROM categories WHERE id = $1
               UNION ALL
               SELECT c.id FROM categories c JOIN subtree s ON c.parent_id = s.id
           )
           SELECT a.variants FROM product_categories pc
           JOIN products p ON p.id = pc.product_id AND p.status = 'active'
           JOIN product_media pm ON pm.product_id = p.id
           JOIN assets a ON a.id = pm.asset_id AND a.status = 'ready'
           WHERE pc.category_id IN (SELECT id FROM subtree)
           ORDER BY pc.position, p.id, pm.position LIMIT 1"#,
        category_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(variants.and_then(|v| image_from(&v, alt)))
}

pub async fn home(tx: &mut TenantTx, ctx: &Context) -> Result<HomePage, Error> {
    let tree = category_tree(tx, ctx).await?;
    let mut categories = Vec::new();
    for c in tree.iter().filter(|c| c.parent_id.is_none()) {
        let image = match &c.image {
            Some(v) => image_from(v, &c.name),
            None => category_image(tx, c.id, &c.name).await?,
        };
        categories.push(CategoryTile {
            label: c.name.clone(),
            href: ctx.path(&format!("/c/{}", c.slug)),
            image,
        });
    }
    let newest = sqlx::query_scalar!(
        "SELECT id FROM products WHERE status = 'active' ORDER BY created_at DESC, id LIMIT 24"
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut featured = cards::cards(tx, ctx, &newest).await?;
    // In-stock products first; the order is otherwise newest first.
    featured.sort_by_key(|c| !c.stock.purchasable());
    featured.truncate(8);
    let cta = categories.first().map_or_else(
        || link(ctx, "home.hero_cta", "/"),
        |c| Link {
            label: t(ctx, "home.hero_cta"),
            href: c.href.clone(),
        },
    );
    let tags = std::iter::once("home".to_owned())
        .chain(featured.iter().map(|c| format!("product:{}", c.id)))
        .collect();
    Ok(HomePage {
        hero: Hero {
            title: t(ctx, "home.hero_title"),
            subtitle: ctx.shop_name.clone(),
            image: featured.first().and_then(|c| c.images.first().cloned()),
            cta,
        },
        categories,
        featured,
        seo: Seo {
            title: ctx.shop_name.clone(),
            description: t(ctx, "trust.delivery"),
            canonical: ctx.page_url("/"),
            alternates: alternates(ctx, |_, _| Some("/".into())),
            json_ld: vec![serde_json::json!({
                "@context": "https://schema.org",
                "@type": "WebSite",
                "name": ctx.shop_name,
                "url": ctx.page_url("/"),
                "potentialAction": {
                    "@type": "SearchAction",
                    "target": ctx.page_url("/search?q={query}"),
                    "query-input": "required name=query",
                },
            })],
            robots: None,
        },
        cache: CacheHints::public(60, tags),
    })
}

// ---------------------------------------------------------------------------------------
// Listings (category, search)

/// Query parameters of a listing URL: `sort`, `page`, `q`; every other key is a facet filter
/// (`?color=red&color=blue&size=m`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingParams {
    pub sort: Option<Sort>,
    pub page: u32,
    pub q: Option<String>,
    pub filters: BTreeMap<String, BTreeSet<String>>,
}

/// The search engine's limits (WP7): more facets or values are ignored, never a 422.
const MAX_FILTER_KEYS: usize = 10;
const MAX_FILTER_VALUES: usize = 20;

impl ListingParams {
    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut out = Self {
            page: 1,
            ..Self::default()
        };
        for (k, v) in pairs {
            match k.as_str() {
                "sort" => out.sort = Sort::parse(v),
                "page" => out.page = v.parse().unwrap_or(1).max(1),
                "q" => out.q = Some(v.chars().take(100).collect()),
                // Facet filters are `f.<facet key>` (the search engine's keys, WP7); anything
                // else in the URL (utm_*, ...) is ignored.
                _ => {
                    let Some(key) = k.strip_prefix("f.") else {
                        continue;
                    };
                    // The same key rule as the search engine (`opt.<code>`, `param.<code>`,
                    // `brand`).
                    let valid_key = key == "brand"
                        || key
                            .strip_prefix("opt.")
                            .or_else(|| key.strip_prefix("param."))
                            .is_some_and(crate::catalog::code_valid);
                    if !valid_key
                        || v.trim().is_empty()
                        || v.chars().count() > 200
                        || (out.filters.len() >= MAX_FILTER_KEYS && !out.filters.contains_key(key))
                    {
                        continue;
                    }
                    let set = out.filters.entry(key.to_owned()).or_default();
                    if set.len() < MAX_FILTER_VALUES {
                        set.insert(v.clone());
                    }
                }
            }
        }
        out
    }

    /// `path?query` with the given state (sorted keys, defaults omitted).
    fn href(
        path: &str,
        q: Option<&str>,
        filters: &BTreeMap<String, BTreeSet<String>>,
        sort: Sort,
        page: u32,
    ) -> String {
        let mut ser: Vec<(String, String)> = Vec::new();
        if let Some(q) = q {
            ser.push(("q".into(), q.into()));
        }
        for (k, vs) in filters {
            for v in vs {
                ser.push((format!("f.{k}"), v.clone()));
            }
        }
        if sort != Sort::Recommended {
            ser.push(("sort".into(), sort.as_str().into()));
        }
        if page > 1 {
            ser.push(("page".into(), page.to_string()));
        }
        if ser.is_empty() {
            path.to_owned()
        } else {
            let query: Vec<String> = ser
                .iter()
                .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                .collect();
            format!("{path}?{}", query.join("&"))
        }
    }
}

/// Percent-encoding for query components (RFC 3986 unreserved characters kept).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategoryView {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description_html: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FacetValueView {
    pub value: String,
    pub label: String,
    pub selected: bool,
    /// Selecting it would give no result (counts are not shown, A23).
    pub disabled: bool,
    /// The listing URL with this value toggled.
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FacetView {
    /// Facet key (`opt.color`, `param.material`, `brand`); the listing query parameter is
    /// `f.<key>` (`?f.opt.color=red`).
    pub key: String,
    pub label: String,
    pub kind: FacetKind,
    pub values: Vec<FacetValueView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SortOption {
    pub value: Sort,
    pub label: String,
    pub selected: bool,
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Pagination {
    pub page: u32,
    pub pages: u32,
    pub prev: Option<String>,
    pub next: Option<String>,
}

/// A category page or search results.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ListingPage {
    pub title: String,
    /// The category (absent on search results).
    pub category: Option<CategoryView>,
    /// The search query (absent on category pages).
    pub query: Option<String>,
    pub subcategories: Vec<Link>,
    pub breadcrumbs: Vec<Link>,
    pub facets: Vec<FacetView>,
    pub sort: Vec<SortOption>,
    pub products: Vec<ProductCard>,
    pub total: u32,
    pub pagination: Pagination,
    pub seo: Seo,
    pub cache: CacheHints,
}

fn facet_views(
    path: &str,
    q: Option<&str>,
    facets: Vec<Facet>,
    applied: &BTreeMap<String, BTreeSet<String>>,
    sort: Sort,
) -> Vec<FacetView> {
    facets
        .into_iter()
        .map(|f| FacetView {
            values: f
                .values
                .into_iter()
                .map(|v| {
                    let mut filters = applied.clone();
                    let set = filters.entry(f.key.clone()).or_default();
                    if !set.remove(&v.value) {
                        set.insert(v.value.clone());
                    }
                    filters.retain(|_, s| !s.is_empty());
                    FacetValueView {
                        href: ListingParams::href(path, q, &filters, sort, 1),
                        value: v.value,
                        label: v.label,
                        selected: v.selected,
                        disabled: v.disabled,
                    }
                })
                .collect(),
            key: f.key,
            label: f.label,
            kind: f.kind,
        })
        .collect()
}

struct CategoryHit {
    id: Uuid,
    name: String,
    slug: String,
    description_html: String,
    seo_title: Option<String>,
    seo_description: Option<String>,
}

async fn listing_page(
    tx: &mut TenantTx,
    ctx: &Context,
    search: Option<Search<'_>>,
    path: &str,
    params: &ListingParams,
    category: Option<&CategoryHit>,
) -> Result<(ListingPage, listing::Listing), Error> {
    let sort = params.sort.unwrap_or_default();
    let q = ListingQuery {
        category_id: category.map(|c| c.id),
        search: if category.is_none() {
            params.q.clone().or(Some(String::new()))
        } else {
            None
        },
        filters: params.filters.clone(),
        sort,
        page: params.page,
        per_page: PER_PAGE,
    };
    let found = listing::find(tx, ctx, search, &q).await?;
    let products = cards::cards(tx, ctx, &found.product_ids).await?;
    let query = if category.is_none() {
        params.q.as_deref()
    } else {
        None
    };
    let href = |page: u32| ListingParams::href(path, query, &found.applied, sort, page);
    let sort_options = Sort::ALL
        .into_iter()
        .map(|s| SortOption {
            value: s,
            label: t(ctx, &format!("sort.{}", s.as_str())),
            selected: s == sort,
            href: ListingParams::href(path, query, &found.applied, s, 1),
        })
        .collect();
    let page = ListingPage {
        title: String::new(),
        category: category.map(|c| CategoryView {
            id: c.id,
            slug: c.slug.clone(),
            name: c.name.clone(),
            description_html: c.description_html.clone(),
        }),
        query: query.map(str::to_owned),
        subcategories: Vec::new(),
        breadcrumbs: Vec::new(),
        facets: facet_views(path, query, found.facets.clone(), &found.applied, sort),
        sort: sort_options,
        total: found.total,
        pagination: Pagination {
            page: found.page,
            pages: found.pages,
            prev: (found.page > 1).then(|| href(found.page - 1)),
            next: (found.page < found.pages).then(|| href(found.page + 1)),
        },
        cache: CacheHints::public(
            60,
            products
                .iter()
                .map(|c| format!("product:{}", c.id))
                .chain(category.map(|c| format!("category:{}", c.id)))
                .collect(),
        ),
        products,
        seo: Seo {
            title: String::new(),
            description: String::new(),
            canonical: String::new(),
            alternates: Vec::new(),
            json_ld: Vec::new(),
            robots: None,
        },
    };
    Ok((page, found))
}

/// `GET /pages/category/{slug}`: `None` for an unknown slug.
pub async fn category(
    tx: &mut TenantTx,
    ctx: &Context,
    search: Option<Search<'_>>,
    slug: &str,
    params: &ListingParams,
) -> Result<Option<ListingPage>, Error> {
    let Some(id) = sqlx::query_scalar!(
        "SELECT category_id FROM category_translations WHERE slug = $1
         ORDER BY (locale = $2) DESC LIMIT 1",
        slug,
        ctx.locale
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let Some(cat) = sqlx::query_as!(
        CategoryHit,
        r#"SELECT ct.category_id AS id, ct.name, ct.slug, ct.description_html, ct.seo_title,
                  ct.seo_description
           FROM category_translations ct WHERE ct.category_id = $1
           ORDER BY (ct.locale = $2) DESC, (ct.locale = $3) DESC, ct.locale LIMIT 1"#,
        id,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let path = ctx.path(&format!("/c/{}", cat.slug));
    let (mut page, found) = listing_page(tx, ctx, search, &path, params, Some(&cat)).await?;
    let mut breadcrumbs = vec![home_link(ctx)];
    breadcrumbs.extend(category_trail(tx, ctx, cat.id).await?);
    page.subcategories = sqlx::query!(
        r#"SELECT t.name AS "name!", t.slug AS "slug!" FROM categories c
           CROSS JOIN LATERAL (
               SELECT name, slug FROM category_translations ct WHERE ct.category_id = c.id
               ORDER BY (ct.locale = $2) DESC, (ct.locale = $3) DESC, ct.locale LIMIT 1
           ) t
           WHERE c.parent_id = $1 ORDER BY c.position, c.id"#,
        cat.id,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| Link {
        label: r.name,
        href: ctx.path(&format!("/c/{}", r.slug)),
    })
    .collect();
    let slugs: BTreeMap<String, String> = sqlx::query!(
        "SELECT locale, slug FROM category_translations WHERE category_id = $1",
        cat.id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.locale, r.slug))
    .collect();
    let filtered = !found.applied.is_empty() || params.sort.is_some_and(|s| s != Sort::Recommended);
    page.title = cat.name.clone();
    page.seo = Seo {
        title: cat
            .seo_title
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{} | {}", cat.name, ctx.shop_name)),
        description: cat
            .seo_description
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                let d = plain_excerpt(&cat.description_html, 160);
                if d.is_empty() {
                    messages::format(
                        &ctx.locale,
                        "listing.count",
                        &[("count", &found.total.to_string())],
                    )
                } else {
                    d
                }
            }),
        // Filtered and re-sorted URLs are noindex with a canonical to the category (§9.5);
        // pagination keeps a self-canonical.
        canonical: if filtered {
            ctx.url(&path)
        } else {
            ctx.url(&ListingParams::href(
                &path,
                None,
                &BTreeMap::new(),
                Sort::Recommended,
                found.page,
            ))
        },
        alternates: if filtered || found.page > 1 {
            Vec::new()
        } else {
            alternates(ctx, |m, l| {
                slugs
                    .get(l)
                    .or_else(|| slugs.get(&m.default_locale))
                    .map(|s| format!("/c/{s}"))
            })
        },
        json_ld: vec![breadcrumb_ld(ctx, &breadcrumbs)],
        robots: filtered.then(|| "noindex,follow".into()),
    };
    page.breadcrumbs = breadcrumbs;
    Ok(Some(page))
}

/// `GET /pages/search?q=`: search results (WP7 engine, Postgres name search as fallback).
pub async fn search(
    tx: &mut TenantTx,
    ctx: &Context,
    search: Option<Search<'_>>,
    params: &ListingParams,
) -> Result<ListingPage, Error> {
    let path = ctx.path("/search");
    let (mut page, _) = listing_page(tx, ctx, search, &path, params, None).await?;
    let q = params.q.clone().unwrap_or_default();
    page.title = messages::format(&ctx.locale, "search.results_for", &[("q", &q)]);
    page.breadcrumbs = vec![home_link(ctx)];
    page.seo = Seo {
        title: format!("{} | {}", page.title, ctx.shop_name),
        description: String::new(),
        canonical: ctx.url(&path),
        alternates: Vec::new(),
        json_ld: Vec::new(),
        robots: Some("noindex,follow".into()),
    };
    Ok(page)
}

// ---------------------------------------------------------------------------------------
// Recommendations and suggestions

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Recommendations {
    pub products: Vec<ProductCard>,
    pub cache: CacheHints,
}

/// `context`: `product:<id>` (same category first), `cart` or `home`. Not personalized yet
/// (M2), so the result is public.
pub async fn recommendations(
    tx: &mut TenantTx,
    ctx: &Context,
    context: &str,
) -> Result<Recommendations, Error> {
    let product = context
        .strip_prefix("product:")
        .and_then(|id| Uuid::parse_str(id).ok());
    let ids = sqlx::query_scalar!(
        "SELECT p.id FROM products p
         LEFT JOIN product_categories pc ON pc.product_id = p.id
             AND pc.category_id IN (SELECT category_id FROM product_categories WHERE product_id = $1)
         WHERE p.status = 'active' AND p.id IS DISTINCT FROM $1
         GROUP BY p.id
         ORDER BY count(pc.category_id) DESC, max(p.created_at) DESC, p.id
         LIMIT 12",
        product
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut products = cards::cards(tx, ctx, &ids).await?;
    products.retain(|c| c.stock.purchasable());
    products.truncate(4);
    Ok(Recommendations {
        cache: CacheHints::public(
            300,
            products
                .iter()
                .map(|c| format!("product:{}", c.id))
                .collect(),
        ),
        products,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn params_parse_and_links_roundtrip() {
        let p = ListingParams::from_pairs(&pairs(&[
            ("f.opt.color", "red"),
            ("f.opt.size", "m"),
            ("f.opt.color", "modrá"),
            ("f.param.material", "len"),
            ("sort", "price_asc"),
            ("page", "x"),
            ("utm_source", "x"),
            ("color", "legacy"),
            ("f.nope", "x"),
            ("f.opt.Bad Key", "x"),
            ("f.opt._leading", "x"),
            ("f.opt.size", "  "),
        ]));
        assert_eq!(p.sort, Some(Sort::PriceAsc));
        assert_eq!(p.page, 1);
        assert_eq!(
            p.filters.keys().collect::<Vec<_>>(),
            ["opt.color", "opt.size", "param.material"]
        );
        let href = ListingParams::href("/c/trika", None, &p.filters, Sort::PriceAsc, 2);
        assert_eq!(
            href,
            "/c/trika?f.opt.color=modr%C3%A1&f.opt.color=red&f.opt.size=m&f.param.material=len&sort=price_asc&page=2"
        );
        assert_eq!(
            ListingParams::href("/c/trika", None, &BTreeMap::new(), Sort::Recommended, 1),
            "/c/trika"
        );
        assert_eq!(
            ListingParams::href(
                "/search",
                Some("čepice & co"),
                &BTreeMap::new(),
                Sort::Recommended,
                1
            ),
            "/search?q=%C4%8Depice%20%26%20co"
        );
    }

    #[test]
    fn filter_params_are_bounded() {
        let many: Vec<(String, String)> = (0..100)
            .map(|i| (format!("f.opt.k{i}"), "v".into()))
            .collect();
        assert_eq!(
            ListingParams::from_pairs(&many).filters.len(),
            MAX_FILTER_KEYS
        );
    }

    #[test]
    fn facet_links_toggle_values() {
        let applied = BTreeMap::from([("color".to_owned(), BTreeSet::from(["red".to_owned()]))]);
        let facets = vec![Facet {
            key: "color".into(),
            label: "Barva".into(),
            kind: FacetKind::Option,
            values: vec![
                listing::FacetValue {
                    value: "red".into(),
                    label: "Červená".into(),
                    selected: true,
                    disabled: false,
                },
                listing::FacetValue {
                    value: "blue".into(),
                    label: "Modrá".into(),
                    selected: false,
                    disabled: false,
                },
            ],
        }];
        let views = facet_views("/c/x", None, facets, &applied, Sort::Recommended);
        let hrefs: Vec<&str> = views[0].values.iter().map(|v| v.href.as_str()).collect();
        assert_eq!(hrefs, ["/c/x", "/c/x?f.color=blue&f.color=red"]);
    }
}
