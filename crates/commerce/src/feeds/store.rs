//! What a merchant's current shop says about itself (SHOP-43): its name and logo, a product
//! feed it links to, and its products from schema.org `Product` JSON-LD on the pages its sitemap
//! lists. Every fetch goes through [`SafeClient::get`]. The crawl reads the homepage, at most
//! `MAX_SITEMAPS` sitemap documents and at most [`MAX_PRODUCT_PAGES`] product pages, each within
//! its [`Limits`], all within `CRAWL_DEADLINE`. Only same-site URLs are followed.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use platform::http::{FetchError, Limits, SafeClient};
use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::import::{ItemProblem, sku_ok};
use super::parse::MAX_IMAGES;
use super::{FeedItem, Source, slugify};

pub const PAGE_LIMITS: Limits = Limits {
    max_bytes: 5 * 1024 * 1024,
    timeout: Duration::from_secs(15),
};
pub const SITEMAP_LIMITS: Limits = Limits {
    max_bytes: 20 * 1024 * 1024,
    timeout: Duration::from_secs(30),
};
pub const MAX_PRODUCT_PAGES: usize = 200;
const MAX_SITEMAPS: usize = 5;
const CRAWL_DEADLINE: Duration = Duration::from_secs(600);
/// The sitemap protocol's own maximum per file.
const MAX_LOCS: usize = 50_000;
const MAX_LINKS: usize = 2_000;
const MAX_LD_BLOCKS: usize = 20;
const MAX_NAME: usize = 200;

/// The crawl's result and the run's snapshot: both steps derive the report from it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Crawl {
    pub site: SiteReport,
    pub items: Vec<FeedItem>,
    /// Per-page errors (`item_id` is the page URL): `page_failed`, `invalid_json_ld`,
    /// `sitemap_failed`, `sitemap_invalid`.
    pub problems: Vec<ItemProblem>,
}

/// What the analysis found on the shop itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SiteReport {
    /// The homepage after redirects.
    pub home: String,
    /// Suggested shop name (plain text, at most 200 characters).
    pub name: Option<String>,
    pub logo_url: Option<String>,
    /// The logo in the media library, imported by the analysis; applied only on request.
    pub logo_asset_id: Option<Uuid>,
    /// A product feed the shop links to: the feed importer reads it more completely.
    pub feed: Option<FeedHint>,
    /// Same-site page URLs read from the sitemap.
    pub product_urls: u32,
    pub pages_fetched: u32,
    pub pages_without_product: u32,
    pub pages_failed: u32,
    /// The sitemap lists more than 200 pages; the first 200 were read.
    pub capped: bool,
    /// The crawl stopped at its time limit.
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FeedHint {
    pub url: String,
    pub source: Source,
}

/// Crawls the shop at `url`. `Err` only when the homepage cannot be read; everything after
/// that is reported per page.
pub async fn crawl(fetch: &SafeClient, url: &str) -> Result<Crawl, FetchError> {
    let deadline = Instant::now() + CRAWL_DEADLINE;
    let home = fetch.get(url, PAGE_LIMITS).await?;
    let site = home.url.clone();
    let scan = blocking(move || scan_html(&home.bytes))
        .await
        .map_err(FetchError::Request)?;
    let (name, logo) = shop_brand(&scan, &site);
    let mut c = Crawl {
        site: SiteReport {
            home: site.to_string(),
            name,
            logo_url: logo.map(String::from),
            feed: feed_hint(&scan, &site),
            ..SiteReport::default()
        },
        ..Crawl::default()
    };
    let urls = page_urls(fetch, &site, deadline, &mut c).await;
    for u in urls {
        if Instant::now() >= deadline {
            c.site.timed_out = true;
            break;
        }
        c.site.pages_fetched += 1;
        let fetched = match fetch.get(u.as_str(), PAGE_LIMITS).await {
            Ok(f) => f,
            Err(e) => {
                c.site.pages_failed += 1;
                c.problem(&u, "page_failed", e);
                continue;
            }
        };
        match blocking(move || product_from_page(&fetched.bytes, &fetched.url)).await {
            Ok(Ok(Some(item))) => c.items.push(item),
            Ok(Ok(None)) => c.site.pages_without_product += 1,
            Ok(Err(detail)) => c.problem(&u, "invalid_json_ld", detail),
            Err(detail) => {
                c.site.pages_failed += 1;
                c.problem(&u, "page_failed", detail);
            }
        }
    }
    Ok(c)
}

impl Crawl {
    fn problem(&mut self, url: &Url, code: &str, detail: impl ToString) {
        self.problems.push(ItemProblem {
            item_id: url.to_string(),
            code: code.into(),
            detail: detail.to_string(),
        });
    }
}

async fn page_urls(fetch: &SafeClient, site: &Url, deadline: Instant, c: &mut Crawl) -> Vec<Url> {
    let mut queue: VecDeque<Url> = site.join("/sitemap.xml").into_iter().collect();
    let mut pages = Vec::new();
    let mut seen = HashSet::new();
    let mut docs = 0;
    while let Some(doc) = queue.pop_front() {
        if docs == MAX_SITEMAPS || c.site.capped {
            break;
        }
        if Instant::now() >= deadline {
            c.site.timed_out = true;
            break;
        }
        docs += 1;
        let fetched = match fetch.get(doc.as_str(), SITEMAP_LIMITS).await {
            Ok(f) => f,
            Err(e) => {
                c.problem(&doc, "sitemap_failed", e);
                continue;
            }
        };
        let base = fetched.url;
        let parsed = blocking(move || parse_sitemap(&fetched.bytes))
            .await
            .and_then(|p| p);
        let resolve = |locs: Vec<String>| -> Vec<Url> {
            locs.iter()
                .filter_map(|l| base.join(l).ok())
                .filter(|u| same_site(u, site))
                .collect()
        };
        match parsed {
            Err(e) => c.problem(&doc, "sitemap_invalid", e),
            Ok(Sitemap::Index(locs)) => {
                let children = resolve(locs);
                let products: Vec<Url> = children
                    .iter()
                    .filter(|u| u.path().contains("product"))
                    .cloned()
                    .collect();
                queue.extend(if products.is_empty() {
                    children
                } else {
                    products
                });
            }
            Ok(Sitemap::Urls(locs)) => {
                for u in resolve(locs) {
                    if u.path().to_ascii_lowercase().ends_with(".xml")
                        && let Some(hint) = feed_at(&u)
                    {
                        c.site.feed.get_or_insert(hint);
                        continue;
                    }
                    if u.path() == "/" || !seen.insert(u.clone()) {
                        continue;
                    }
                    c.site.product_urls += 1;
                    if pages.len() < MAX_PRODUCT_PAGES {
                        pages.push(u);
                    } else {
                        c.site.capped = true;
                    }
                }
            }
        }
    }
    pages
}

#[derive(Debug, PartialEq, Eq)]
enum Sitemap {
    Index(Vec<String>),
    Urls(Vec<String>),
}

fn parse_sitemap(bytes: &[u8]) -> Result<Sitemap, String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut buf = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut index = None;
    let mut locs = Vec::new();
    let mut text = String::new();
    let in_loc = |path: &[Vec<u8>]| {
        matches!(
            path,
            [a, b, c] if c == b"loc"
                && ((a == b"urlset" && b == b"url") || (a == b"sitemapindex" && b == b"sitemap"))
        )
    };
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?
        {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_vec();
                if path.is_empty() {
                    if index.is_some() {
                        return Err("more than one root element".into());
                    }
                    index = match name.as_slice() {
                        b"sitemapindex" => Some(true),
                        b"urlset" => Some(false),
                        _ => return Err("not a sitemap".into()),
                    };
                }
                path.push(name);
                text.clear();
            }
            Event::End(_) => {
                if in_loc(&path) && locs.len() < MAX_LOCS {
                    let loc = text.trim();
                    if !loc.is_empty() {
                        locs.push(loc.to_owned());
                    }
                }
                path.pop();
                text.clear();
            }
            Event::Text(t) if in_loc(&path) => {
                text.push_str(&t.decode().map_err(|e| e.to_string())?)
            }
            Event::CData(t) if in_loc(&path) => {
                text.push_str(&t.decode().map_err(|e| e.to_string())?)
            }
            Event::GeneralRef(r) if in_loc(&path) => {
                if let Some(ch) = r.resolve_char_ref().map_err(|e| e.to_string())? {
                    text.push(ch);
                } else if let Some(v) =
                    resolve_predefined_entity(&r.decode().map_err(|e| e.to_string())?)
                {
                    text.push_str(v);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    match index {
        _ if !path.is_empty() => Err("unexpected end of the document".into()),
        Some(true) => Ok(Sitemap::Index(locs)),
        Some(false) => Ok(Sitemap::Urls(locs)),
        None => Err("not a sitemap".into()),
    }
}

fn same_site(u: &Url, site: &Url) -> bool {
    let host = |u: &Url| {
        u.host_str()
            .map(|h| h.strip_prefix("www.").unwrap_or(h).to_owned())
    };
    u.scheme() == site.scheme()
        && u.port_or_known_default() == site.port_or_known_default()
        && host(u).is_some()
        && host(u) == host(site)
}

#[derive(Debug, Default)]
struct Scan {
    json_ld: Vec<String>,
    title: Option<String>,
    og_site_name: Option<String>,
    hrefs: Vec<String>,
    logo_srcs: Vec<String>,
}

fn scan_html(bytes: &[u8]) -> Scan {
    let text = String::from_utf8_lossy(bytes);
    let tokenizer = Tokenizer::new(Sink::default(), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(&text));
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner().scan
}

#[derive(Default)]
struct Sink(RefCell<SinkState>);

#[derive(Default)]
struct SinkState {
    scan: Scan,
    capture: Option<Capture>,
    buf: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    JsonLd,
    Title,
}

fn attr(tag: &Tag, name: &str) -> Option<String> {
    tag.attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| a.value.to_string())
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut st = self.0.borrow_mut();
        match token {
            Token::TagToken(t) if t.kind == TagKind::StartTag => match &*t.name {
                "script" => {
                    let json_ld = attr(&t, "type")
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/ld+json"));
                    st.capture = json_ld.then_some(Capture::JsonLd);
                    st.buf.clear();
                    return TokenSinkResult::RawData(RawKind::ScriptData);
                }
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                    return TokenSinkResult::RawData(RawKind::Rawtext);
                }
                "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
                "title" => {
                    st.capture = Some(Capture::Title);
                    st.buf.clear();
                    return TokenSinkResult::RawData(RawKind::Rcdata);
                }
                "meta" => {
                    let key = attr(&t, "property").or_else(|| attr(&t, "name"));
                    if key.is_some_and(|k| k.eq_ignore_ascii_case("og:site_name"))
                        && st.scan.og_site_name.is_none()
                    {
                        st.scan.og_site_name = attr(&t, "content");
                    }
                }
                "a" | "link" => {
                    if let Some(href) = attr(&t, "href")
                        && st.scan.hrefs.len() < MAX_LINKS
                    {
                        st.scan.hrefs.push(href);
                    }
                }
                "img" => {
                    let logo = ["src", "class", "id", "alt"]
                        .iter()
                        .any(|n| attr(&t, n).is_some_and(|v| v.to_lowercase().contains("logo")));
                    if logo
                        && let Some(src) = attr(&t, "src")
                        && st.scan.logo_srcs.len() < MAX_LINKS
                    {
                        st.scan.logo_srcs.push(src);
                    }
                }
                _ => {}
            },
            Token::TagToken(t) if t.kind == TagKind::EndTag => {
                let st = &mut *st;
                match (st.capture, &*t.name) {
                    (Some(Capture::JsonLd), "script") => {
                        if st.scan.json_ld.len() < MAX_LD_BLOCKS {
                            st.scan.json_ld.push(std::mem::take(&mut st.buf));
                        }
                        st.capture = None;
                    }
                    (Some(Capture::Title), "title") => {
                        if st.scan.title.is_none() {
                            st.scan.title = Some(std::mem::take(&mut st.buf));
                        }
                        st.capture = None;
                    }
                    _ => {}
                }
            }
            Token::CharacterTokens(s) if st.capture.is_some() => st.buf.push_str(&s),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn product_from_page(html: &[u8], page: &Url) -> Result<Option<FeedItem>, String> {
    let scan = scan_html(html);
    let mut bad = None;
    for block in &scan.json_ld {
        match serde_json::from_str::<Value>(block.trim()) {
            Err(e) => {
                bad.get_or_insert(e.to_string());
            }
            Ok(v) => {
                if let Some(p) = top_level_nodes(&v).find(|n| has_type(n, &["Product"])) {
                    return Ok(Some(product_item(p, page)));
                }
            }
        }
    }
    bad.map_or(Ok(None), Err)
}

fn top_level_nodes(v: &Value) -> impl Iterator<Item = &Value> {
    let top: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        v => vec![v],
    };
    top.into_iter().flat_map(|n| {
        let graph = n["@graph"]
            .as_array()
            .map(|g| g.iter())
            .into_iter()
            .flatten();
        std::iter::once(n).chain(graph)
    })
}

fn has_type(n: &Value, types: &[&str]) -> bool {
    let is = |t: &Value| {
        t.as_str().is_some_and(|t| {
            let t = ["https://schema.org/", "http://schema.org/", "schema:"]
                .iter()
                .find_map(|p| t.strip_prefix(p))
                .unwrap_or(t);
            types.contains(&t)
        })
    };
    match &n["@type"] {
        Value::Array(a) => a.iter().any(is),
        t => is(t),
    }
}

fn product_item(p: &Value, page: &Url) -> FeedItem {
    let sku = text(&p["sku"]).filter(|s| sku_ok(s));
    let gtin = ["gtin13", "gtin", "gtin14", "gtin12", "gtin8"]
        .iter()
        .find_map(|k| text(&p[*k]))
        .filter(|g| (8..=14).contains(&g.len()) && g.bytes().all(|b| b.is_ascii_digit()));
    let offer = match &p["offers"] {
        Value::Array(a) => a.iter().find(|o| o.is_object()),
        o @ Value::Object(_) => Some(o),
        _ => None,
    };
    let (price_minor, currency) = offer.map(price_of).unwrap_or_default();
    FeedItem {
        item_id: sku
            .or_else(|| gtin.clone())
            .unwrap_or_else(|| fallback_sku(page)),
        name: text(&p["name"]).map(|n| clean_name(&n)).unwrap_or_default(),
        description: text(&p["description"]).unwrap_or_default(),
        url: Some(page.to_string()),
        images: images(&p["image"], page),
        price_minor,
        currency,
        ean: gtin,
        brand: text(&p["brand"]).or_else(|| text(&p["brand"]["name"])),
        ..FeedItem::default()
    }
}

fn price_of(offer: &Value) -> (Option<i64>, Option<String>) {
    let raw = match &offer["price"] {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let currency = text(&offer["priceCurrency"])
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()));
    (raw.as_deref().and_then(strict_price), currency)
}

fn strict_price(s: &str) -> Option<i64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits = |d: &str| d.bytes().all(|b| b.is_ascii_digit());
    if !(1..=12).contains(&int.len()) || frac.len() > 2 || !digits(int) || !digits(frac) {
        return None;
    }
    let cents: i64 = format!("{frac:0<2}").parse().ok()?;
    int.parse::<i64>()
        .ok()?
        .checked_mul(100)?
        .checked_add(cents)
}

fn images(v: &Value, page: &Url) -> Vec<String> {
    let all: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        v => vec![v],
    };
    let mut out: Vec<String> = Vec::new();
    for u in all.into_iter().filter_map(|i| image_url(i, page)) {
        if out.len() < MAX_IMAGES && !out.contains(&u.to_string()) {
            out.push(u.into());
        }
    }
    out
}

fn image_url(v: &Value, base: &Url) -> Option<Url> {
    let raw = v
        .as_str()
        .or_else(|| v["url"].as_str())
        .or_else(|| v["contentUrl"].as_str())?;
    resolve(base, raw)
}

fn resolve(base: &Url, raw: &str) -> Option<Url> {
    base.join(raw.trim())
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}

fn fallback_sku(page: &Url) -> String {
    slugify(
        &page.path().replace(|c: char| !c.is_alphanumeric(), " "),
        64,
    )
}

fn text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.iter().find_map(Value::as_str)?.to_owned(),
        _ => return None,
    };
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn clean_name(raw: &str) -> String {
    raw.split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NAME)
        .collect()
}

const ORG_TYPES: &[&str] = &["Organization", "Store", "OnlineStore", "LocalBusiness"];

fn shop_brand(scan: &Scan, home: &Url) -> (Option<String>, Option<Url>) {
    let orgs: Vec<Value> = scan
        .json_ld
        .iter()
        .filter_map(|b| serde_json::from_str::<Value>(b.trim()).ok())
        .collect();
    let org = orgs
        .iter()
        .flat_map(top_level_nodes)
        .find(|n| has_type(n, ORG_TYPES));
    let name = org
        .and_then(|o| text(&o["name"]))
        .into_iter()
        .chain(scan.og_site_name.clone())
        .chain(scan.title.clone())
        .map(|n| clean_name(&n))
        .find(|n| !n.is_empty());
    let logo = org
        .and_then(|o| image_url(&o["logo"], home))
        .or_else(|| scan.logo_srcs.iter().find_map(|s| resolve(home, s)));
    (name, logo)
}

fn feed_hint(scan: &Scan, home: &Url) -> Option<FeedHint> {
    scan.hrefs.iter().find_map(|h| {
        resolve(home, h)
            .filter(|u| same_site(u, home))
            .and_then(|u| feed_at(&u))
    })
}

fn feed_at(u: &Url) -> Option<FeedHint> {
    let s = format!("{}?{}", u.path(), u.query().unwrap_or_default()).to_lowercase();
    if !["xml", "feed", "export"].iter().any(|k| s.contains(k)) {
        return None;
    }
    let source = if s.contains("heureka") || s.contains("zbozi") {
        Source::Heureka
    } else if s.contains("google") || s.contains("merchant") {
        Source::Google
    } else {
        return None;
    };
    Some(FeedHint {
        url: u.to_string(),
        source,
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("the page could not be read: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const INDEX: &str = include_str!("../../../../fixtures/store/index.html");
    const TRICKO: &str = include_str!("../../../../fixtures/store/tricko-basic.html");
    const MIKINA: &str = include_str!("../../../../fixtures/store/mikina-crew.html");
    const TASKA: &str = include_str!("../../../../fixtures/store/taska-platena.html");
    const OSTRY: &str = include_str!("../../../../fixtures/store/ostry-popis.html");
    const ROZBITY: &str = include_str!("../../../../fixtures/store/rozbity-zaznam.html");
    const O_NAS: &str = include_str!("../../../../fixtures/store/o-nas.html");
    const SITEMAP: &str = include_str!("../../../../fixtures/store/sitemap.xml");
    const PRODUCTS: &str = include_str!("../../../../fixtures/store/sitemap-products.xml");

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn page(html: &str, at: &str) -> Result<Option<FeedItem>, String> {
        product_from_page(html.as_bytes(), &url(at))
    }

    fn ld(json: &str) -> String {
        format!("<html><head><script type=\"application/ld+json\">{json}</script></head></html>")
    }

    #[test]
    fn sitemaps_are_read_structurally() {
        assert_eq!(
            parse_sitemap(SITEMAP.as_bytes()).unwrap(),
            Sitemap::Index(vec![
                "http://mocks:4010/sitemap-products.xml".into(),
                "http://mocks:4010/sitemap-pages.xml".into(),
            ])
        );
        let Sitemap::Urls(locs) = parse_sitemap(PRODUCTS.as_bytes()).unwrap() else {
            panic!("a urlset")
        };
        assert_eq!(locs.len(), 5);
        assert_eq!(locs[0], "http://mocks:4010/tricko-basic/");

        let images = r#"<?xml version="1.0"?>
            <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"
                    xmlns:image="http://www.google.com/schemas/sitemap-image/1.1">
              <url><loc> https://shop.example/a/?x=1&amp;y=2 </loc>
                <image:image><image:loc>https://cdn.example/a.jpg</image:loc></image:image>
              </url>
              <loc>https://shop.example/stray/</loc>
            </urlset>"#;
        assert_eq!(
            parse_sitemap(images.as_bytes()).unwrap(),
            Sitemap::Urls(vec!["https://shop.example/a/?x=1&y=2".into()]),
            "image:loc and locs outside <url> never count"
        );

        let prefixed = r#"<sm:sitemapindex xmlns:sm="http://www.sitemaps.org/schemas/sitemap/0.9">
            <sm:sitemap><sm:loc>https://shop.example/s1.xml</sm:loc></sm:sitemap></sm:sitemapindex>"#;
        assert_eq!(
            parse_sitemap(prefixed.as_bytes()).unwrap(),
            Sitemap::Index(vec!["https://shop.example/s1.xml".into()])
        );

        let entities = r#"<?xml version="1.0"?>
            <!DOCTYPE urlset [<!ENTITY a "https://evil.example/aaaaaaaaaaaaaaaa">]>
            <urlset><url><loc>&a;</loc></url><url><loc>https://shop.example/b/</loc></url></urlset>"#;
        assert_eq!(
            parse_sitemap(entities.as_bytes()).unwrap(),
            Sitemap::Urls(vec!["https://shop.example/b/".into()]),
            "custom entities are not expanded"
        );

        for bad in [
            &b"<html><body>not a sitemap</body></html>"[..],
            b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03",
            b"<urlset><url><loc>https://a/</url>",
        ] {
            assert!(parse_sitemap(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_same_site_urls_are_followed() {
        let site = url("https://www.shop.example/");
        assert!(same_site(&url("https://shop.example/a/"), &site));
        assert!(same_site(&url("https://www.shop.example/a/"), &site));
        for foreign in [
            "http://www.shop.example/a/",
            "https://www.shop.example:8443/a/",
            "https://other.example/a/",
            "https://evil.shop.example/a/",
        ] {
            assert!(!same_site(&url(foreign), &site), "{foreign}");
        }
    }

    #[test]
    fn fixture_products_map_to_feed_items() {
        let t = page(TRICKO, "http://mocks:4010/tricko-basic/")
            .unwrap()
            .unwrap();
        assert_eq!(t.item_id, "SK-TRICKO-BASIC");
        assert_eq!(t.name, "Tričko Basic");
        assert_eq!(t.price_minor, Some(39_900));
        assert_eq!(t.currency.as_deref(), Some("CZK"));
        assert_eq!(t.ean.as_deref(), Some("8594000000013"));
        assert_eq!(t.brand.as_deref(), Some("Stará Kočka"));
        assert_eq!(t.url.as_deref(), Some("http://mocks:4010/tricko-basic/"));
        assert_eq!(
            t.images,
            [
                "http://mocks:4010/images/demo/tee-ink.jpg",
                "http://mocks:4010/images/demo/tee-sand.jpg"
            ]
        );
        assert!(t.description.contains("<strong>každý den</strong>"));

        let m = page(MIKINA, "http://mocks:4010/mikina-crew/")
            .unwrap()
            .unwrap();
        assert_eq!(m.item_id, "SK-MIKINA-CREW", "@graph with an array @type");
        assert_eq!(
            m.price_minor,
            Some(129_050),
            "a JSON number in an offers array"
        );
        assert_eq!(m.ean.as_deref(), Some("8594000000020"));
        assert_eq!(m.images, ["http://mocks:4010/images/demo/hoodie-ash.jpg"]);

        let b = page(TASKA, "http://mocks:4010/taska-platena/")
            .unwrap()
            .unwrap();
        assert_eq!(b.item_id, "8594000000037", "no sku: the GTIN");
        assert_eq!(
            b.price_minor, None,
            "an AggregateOffer range is not a price"
        );
        assert_eq!(b.currency.as_deref(), Some("EUR"));

        let o = page(OSTRY, "http://mocks:4010/ostry-popis/")
            .unwrap()
            .unwrap();
        assert_eq!(o.price_minor, Some(24_900));
        assert!(o.description.contains("<script>"), "sanitized when saved");

        assert!(page(ROZBITY, "http://mocks:4010/rozbity-zaznam/").is_err());
        assert_eq!(page(O_NAS, "http://mocks:4010/o-nas/").unwrap(), None);
    }

    #[test]
    fn product_ids_fall_back_to_the_page_path() {
        let p = page(
            &ld(r#"{"@type":"schema:Product","name":"Hrnek","sku":"has space"}"#),
            "https://shop.example/kategorie/Hrnek_modry/",
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.item_id, "kategorie-hrnek-modry");
    }

    #[test]
    fn malformed_and_mistyped_json_ld_never_panics() {
        let both = format!(
            "{}{}",
            ld("{\"@type\":\"Product\",\"name\":"),
            ld(r#"{"@type":"Product","name":"Ok","sku":"A1"}"#)
        );
        assert_eq!(
            page(&both, "https://shop.example/a/")
                .unwrap()
                .unwrap()
                .item_id,
            "A1",
            "a broken block next to a good one"
        );
        let odd = page(
            &ld(r#"{"@type":"Product","name":{},"sku":5,"offers":5,"image":{"x":1},"brand":[]}"#),
            "https://shop.example/odd/",
        )
        .unwrap()
        .unwrap();
        assert_eq!(odd.name, "");
        assert_eq!(odd.item_id, "5");
        assert_eq!(odd.price_minor, None);
        assert!(odd.images.is_empty());
        let list = ld(
            r#"{"@type":"ItemList","itemListElement":[{"@type":"Product","name":"Not this page's"}]}"#,
        );
        assert_eq!(page(&list, "https://shop.example/c/").unwrap(), None);
    }

    #[test]
    fn prices_are_parsed_strictly() {
        let price = |offer: &str| {
            page(
                &ld(&format!(
                    r#"{{"@type":"Product","name":"X","sku":"X","offers":{offer}}}"#
                )),
                "https://shop.example/x/",
            )
            .unwrap()
            .unwrap()
            .price_minor
        };
        assert_eq!(
            price(r#"{"price":"12.90","priceCurrency":"EUR"}"#),
            Some(1_290)
        );
        assert_eq!(price(r#"{"price":12.5}"#), Some(1_250));
        assert_eq!(price(r#"{"price":399}"#), Some(39_900));
        for bad in [
            r#"{"price":"1.299"}"#,
            r#"{"price":"12,90"}"#,
            r#"{"price":"1 299"}"#,
            r#"{"price":"-5"}"#,
            r#"{"price":-5}"#,
            r#"{"price":1e30}"#,
            r#"{"price":"abc"}"#,
            r#"{"lowPrice":"10","highPrice":"10"}"#,
        ] {
            assert_eq!(price(bad), None, "{bad}");
        }
    }

    #[test]
    fn urls_resolve_against_the_page_not_a_base_tag() {
        let html = format!(
            "<base href=\"https://evil.example/\">{}",
            ld(
                r#"{"@type":"Product","name":"X","sku":"X","image":["/a.jpg","b.jpg","javascript:alert(1)"]}"#
            )
        );
        let p = page(&html, "https://shop.example/p/x/").unwrap().unwrap();
        assert_eq!(
            p.images,
            [
                "https://shop.example/a.jpg",
                "https://shop.example/p/x/b.jpg"
            ]
        );
    }

    #[test]
    fn the_shop_brand_comes_from_the_best_source() {
        let home = url("http://mocks:4010/");
        let (name, logo) = shop_brand(&scan_html(INDEX.as_bytes()), &home);
        assert_eq!(name.as_deref(), Some("Stará Kočka"));
        assert_eq!(
            logo.map(String::from).as_deref(),
            Some("http://mocks:4010/images/demo/cap-olive.jpg")
        );

        let og = r#"<title>Shop | Home</title><meta property="og:site_name" content="Kočka &amp; Pes">
            <img class="site-logo" src="/l.png">"#;
        let (name, logo) = shop_brand(&scan_html(og.as_bytes()), &home);
        assert_eq!(name.as_deref(), Some("Kočka & Pes"));
        assert_eq!(
            logo.map(String::from).as_deref(),
            Some("http://mocks:4010/l.png")
        );

        let title = format!(
            "<title>  Kočka\n\t<img src=x onerror=alert(1)>\u{7}{}</title>",
            "x".repeat(300)
        );
        let (name, logo) = shop_brand(&scan_html(title.as_bytes()), &home);
        let name = name.unwrap();
        assert!(
            name.starts_with("Kočka <img src=x onerror=alert(1)>x"),
            "{name}"
        );
        assert_eq!(name.chars().count(), 200);
        assert_eq!(logo, None);

        let org =
            ld(r#"{"@type":"Store","name":"Org","logo":{"@type":"ImageObject","url":"/o.png"}}"#);
        let html = format!("{org}<meta property=\"og:site_name\" content=\"Og\">");
        let (name, logo) = shop_brand(&scan_html(html.as_bytes()), &home);
        assert_eq!(name.as_deref(), Some("Org"));
        assert_eq!(
            logo.map(String::from).as_deref(),
            Some("http://mocks:4010/o.png")
        );
    }

    #[test]
    fn a_linked_product_feed_is_hinted() {
        let home = url("http://mocks:4010/");
        assert_eq!(
            feed_hint(&scan_html(INDEX.as_bytes()), &home),
            Some(FeedHint {
                url: "http://mocks:4010/feeds/heureka-demo.xml".into(),
                source: Source::Heureka,
            })
        );
        let hint = |html: &str| feed_hint(&scan_html(html.as_bytes()), &home);
        assert_eq!(
            hint(r#"<a href="/export/google-merchant.xml">x</a>"#).map(|h| h.source),
            Some(Source::Google)
        );
        assert_eq!(
            hint(r#"<a href="https://obchody.heureka.cz/mocks/recenze/">x</a>"#),
            None
        );
        assert_eq!(hint(r#"<link rel="alternate" href="/feed/">"#), None);
        assert_eq!(
            hint(r#"<script>var a = '<a href="/heureka.xml">';</script>"#),
            None,
            "script contents are not markup"
        );
    }
}
