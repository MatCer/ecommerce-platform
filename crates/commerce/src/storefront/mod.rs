//! Storefront page models (spec §8.2, §9, A4): what themes render, built from the catalog,
//! pricing and inventory of one tenant and one market.
//!
//! Every page model carries `seo` and `cache` hints. The edge reads `cache` to decide whether
//! a rendered page may be cached (A2); themes can only lower it. Money is `{amount_minor,
//! currency, formatted}`, formatted for the request locale.
//!
//! The market comes from the edge, the tenant from the storefront token; [`context`] loads the
//! market inside the tenant's transaction, so a market of another tenant is simply not found
//! (RLS) and the request fails with `403 market_mismatch`.

pub mod cards;
pub mod files;
pub mod images;
pub mod listing;
pub mod messages;
pub mod pages;
pub mod product;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::money::{Currency, Locale, Money, MoneyView};

pub use listing::{Listing, ListingQuery, listing};

/// The search engine for listings (WP7): category and search pages use it and fall back to
/// the Postgres [`listing`] when it is degraded (A27: search is not part of core readiness).
#[derive(Clone, Copy)]
pub struct Search<'a> {
    pub meili: &'a crate::search::Meili,
    pub storage: &'a platform::storage::Storage,
}

/// The search scope (WP7) of a storefront context: its market and locale.
pub async fn search_scope(
    tx: &mut TenantTx,
    ctx: &Context,
) -> Result<crate::search::query::Scope, Error> {
    crate::search::query::scope(tx, ctx.market.id, &ctx.locale)
        .await?
        .ok_or(Error::NotFound)
}

/// How public storefront URLs are built (canonicals, hreflang, sitemaps). Never taken from
/// request headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrls {
    /// `https` in production, `http` for local `*.localhost`.
    pub scheme: String,
    /// A non-default port, for local development (`8080`).
    pub port: Option<u16>,
}

impl PublicUrls {
    pub fn base(&self, host: &str) -> String {
        match self.port {
            Some(p) => format!("{}://{host}:{p}", self.scheme),
            None => format!("{}://{host}", self.scheme),
        }
    }
}

impl Default for PublicUrls {
    fn default() -> Self {
        Self {
            scheme: "https".into(),
            port: None,
        }
    }
}

/// One market of the tenant as seen from the storefront.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketCtx {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub currency: Currency,
    pub default_locale: String,
    pub locales: Vec<String>,
    pub country_codes: Vec<String>,
    pub price_list_id: Option<Uuid>,
    pub is_default: bool,
    /// `https://shop.example` of the market's primary verified domain, if it has one.
    pub base_url: Option<String>,
}

impl MarketCtx {
    /// `cs-CZ`: the market's default language in its first country (hreflang value).
    pub fn hreflang(&self) -> String {
        let lang = self
            .default_locale
            .split('-')
            .next()
            .unwrap_or(&self.default_locale);
        match self.country_codes.first() {
            Some(c) => format!("{lang}-{c}"),
            None => lang.to_owned(),
        }
    }
}

/// Everything a page model needs about the request: tenant, market, locale, URLs, time.
#[derive(Debug, Clone)]
pub struct Context {
    pub tenant_id: Uuid,
    pub shop_name: String,
    pub market: MarketCtx,
    /// All markets of the tenant (for hreflang alternates), the default one first.
    pub markets: Vec<MarketCtx>,
    pub locale: String,
    pub base_url: String,
    pub now: DateTime<Utc>,
}

/// The checkout origin (A1) of a shop base URL: `http://demo.localhost:8080` →
/// `http://checkout.demo.localhost:8080`.
pub fn checkout_base(shop_base: &str) -> String {
    match shop_base.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://checkout.{rest}"),
        None => format!("checkout.{shop_base}"),
    }
}

impl Context {
    /// A URL on the market's checkout origin (A1).
    pub fn checkout_url(&self, path: &str) -> String {
        format!("{}{path}", checkout_base(&self.base_url))
    }

    pub fn fmt_locale(&self) -> Locale {
        Locale::from_tag(&self.locale)
    }

    pub fn money(&self, minor: i64) -> MoneyView {
        Money::new(minor, self.market.currency).view(self.fmt_locale())
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The price list of the market, or `409 market_not_priced` (set up by the merchant).
    pub fn price_list(&self) -> Result<Uuid, Error> {
        self.market.price_list_id.ok_or(Error::Conflict {
            code: "market_not_priced",
            detail: "the market has no price list".into(),
        })
    }

    /// Picks the text for the request locale from an i18n object (`{"cs": "..."}`), falling
    /// back to the market's default locale, then to any translation.
    pub fn text(&self, i18n: &Value) -> Option<String> {
        let obj = i18n.as_object()?;
        [self.locale.as_str(), self.market.default_locale.as_str()]
            .iter()
            .find_map(|l| obj.get(*l))
            .or_else(|| obj.values().next())
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// Loads the storefront context for `market_id` of the tenant in `tx`. `locale` is a hint
/// (the edge's market locale); anything the market does not offer falls back to its default.
pub async fn context(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    market_id: Uuid,
    locale: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Context, Error> {
    let rows = sqlx::query!(
        r#"SELECT m.id, m.code, m.name, m.currency, m.default_locale, m.locales, m.country_codes,
                  m.price_list_id, m.is_default,
                  (SELECT d.hostname FROM platform.domains d
                   WHERE d.market_id = m.id AND d.verified_at IS NOT NULL
                   ORDER BY d.is_primary DESC, d.hostname LIMIT 1) AS host
           FROM markets m ORDER BY m.is_default DESC, m.code"#
    )
    .fetch_all(&mut **tx)
    .await?;
    let markets: Vec<MarketCtx> = rows
        .into_iter()
        .filter_map(|r| {
            Some(MarketCtx {
                id: r.id,
                code: r.code,
                name: r.name,
                currency: Currency::parse(&r.currency)?,
                default_locale: r.default_locale,
                locales: r.locales,
                country_codes: r.country_codes,
                price_list_id: r.price_list_id,
                is_default: r.is_default,
                base_url: r.host.map(|h| urls.base(&h)),
            })
        })
        .collect();
    let market = markets
        .iter()
        .find(|m| m.id == market_id)
        .cloned()
        .ok_or(Error::Forbidden {
            code: "market_mismatch",
        })?;
    // A market without a verified domain is not published: nothing is revealed about it.
    let base_url = market.base_url.clone().ok_or(Error::NotFound)?;
    let shop_name = sqlx::query_scalar!(
        "SELECT name FROM platform.tenants WHERE id = $1",
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    let locale = locale
        .filter(|l| market.locales.iter().any(|m| m == l))
        .unwrap_or(&market.default_locale)
        .to_owned();
    Ok(Context {
        tenant_id: tx.tenant_id(),
        shop_name,
        market,
        markets,
        locale,
        base_url,
        now,
    })
}

// ---------------------------------------------------------------------------------------
// Shapes shared by the page models

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Link {
    pub label: String,
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Alternate {
    /// hreflang value (`cs-CZ`, `x-default`).
    pub locale: String,
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Seo {
    pub title: String,
    pub description: String,
    pub canonical: String,
    pub alternates: Vec<Alternate>,
    /// JSON-LD documents for `<script type="application/ld+json">`.
    pub json_ld: Vec<Value>,
    /// `noindex,follow` on filtered listings (spec §9.5); absent otherwise.
    pub robots: Option<String>,
}

/// Cache hints the edge reads from every page model (spec §8.2, A2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CacheHints {
    pub public: bool,
    /// Seconds.
    pub max_age: u32,
    /// Purge tags (`product:<id>`, `category:<id>`, `shop`, ...).
    pub tags: Vec<String>,
}

impl CacheHints {
    pub fn public(max_age: u32, tags: Vec<String>) -> Self {
        Self {
            public: true,
            max_age,
            tags,
        }
    }
}

/// hreflang alternates: the path of the same page in every market that has one, plus
/// `x-default` for the default market.
pub fn alternates(
    ctx: &Context,
    path_for: impl Fn(&MarketCtx) -> Option<String>,
) -> Vec<Alternate> {
    let mut out = Vec::new();
    for m in &ctx.markets {
        let (Some(base), Some(path)) = (&m.base_url, path_for(m)) else {
            continue;
        };
        out.push(Alternate {
            locale: m.hreflang(),
            href: format!("{base}{path}"),
        });
        if m.is_default {
            out.push(Alternate {
                locale: "x-default".into(),
                href: format!("{base}{path}"),
            });
        }
    }
    // A lone market has nothing to alternate with.
    if out.len() <= 2 && ctx.markets.len() <= 1 {
        out.clear();
    }
    out
}

/// Plain text of sanitized HTML, cut at `max` characters on a word boundary (meta
/// descriptions).
pub fn plain_excerpt(html: &str, max: usize) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    for w in words {
        if out.chars().count() + w.chars().count() + 1 > max {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(code: &str, locale: &str, country: &str, default: bool) -> MarketCtx {
        MarketCtx {
            id: Uuid::now_v7(),
            code: code.into(),
            name: code.into(),
            currency: Currency::Czk,
            default_locale: locale.into(),
            locales: vec![locale.into()],
            country_codes: vec![country.into()],
            price_list_id: None,
            is_default: default,
            base_url: Some(format!("https://{code}.example")),
        }
    }

    fn ctx(markets: Vec<MarketCtx>) -> Context {
        Context {
            tenant_id: Uuid::nil(),
            shop_name: "Shop".into(),
            market: markets[0].clone(),
            locale: markets[0].default_locale.clone(),
            base_url: markets[0].base_url.clone().unwrap_or_default(),
            markets,
            now: Utc::now(),
        }
    }

    #[test]
    fn hreflang_alternates_cover_markets_and_default() {
        let c = ctx(vec![
            market("cz", "cs", "CZ", true),
            market("sk", "sk", "SK", false),
        ]);
        let alts = alternates(&c, |m| Some(format!("/p/{}", m.code)));
        let got: Vec<(&str, &str)> = alts
            .iter()
            .map(|a| (a.locale.as_str(), a.href.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("cs-CZ", "https://cz.example/p/cz"),
                ("x-default", "https://cz.example/p/cz"),
                ("sk-SK", "https://sk.example/p/sk"),
            ]
        );
        // A page missing in a market has no alternate there.
        let alts = alternates(&c, |m| (m.code == "cz").then(|| "/x".to_owned()));
        assert_eq!(alts.len(), 2);
        // One market alone: nothing to alternate with.
        assert!(
            alternates(&ctx(vec![market("cz", "cs", "CZ", true)]), |_| Some(
                "/".into()
            ))
            .is_empty()
        );
    }

    #[test]
    fn text_falls_back_to_default_locale_then_any() {
        let mut c = ctx(vec![market("sk", "sk", "SK", true)]);
        c.locale = "en".into();
        assert_eq!(
            c.text(&serde_json::json!({"sk": "Farba", "cs": "Barva"}))
                .as_deref(),
            Some("Farba")
        );
        assert_eq!(
            c.text(&serde_json::json!({"cs": "Barva"})).as_deref(),
            Some("Barva")
        );
        assert_eq!(c.text(&serde_json::json!("x")), None);
    }

    #[test]
    fn excerpts_strip_tags_and_cut_on_words() {
        assert_eq!(
            plain_excerpt("<p>Měkké <b>tričko</b> z&nbsp;bavlny.</p>", 100),
            "Měkké tričko z bavlny."
        );
        assert_eq!(plain_excerpt("<p>one two three</p>", 8), "one two…");
    }

    #[test]
    fn public_urls() {
        let u = PublicUrls {
            scheme: "http".into(),
            port: Some(8080),
        };
        assert_eq!(u.base("demo.localhost"), "http://demo.localhost:8080");
        assert_eq!(PublicUrls::default().base("shop.cz"), "https://shop.cz");
    }
}
