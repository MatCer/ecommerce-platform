//! SEO and agent files per market (= per shop host, spec §9.5): `robots.txt`, `llms.txt` and
//! XML sitemaps (an index plus chunks of at most 10 000 URLs with hreflang alternates). The
//! edge passes `/robots.txt`, `/llms.txt` and `/sitemap*.xml` through to the API.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use uuid::Uuid;

use super::{Context, MarketCtx, alternates};

pub const SITEMAP_CHUNK: usize = 10_000;

pub fn robots(ctx: &Context) -> String {
    format!(
        "User-agent: *\nAllow: /\nDisallow: /_p/\nDisallow: /search\n\nSitemap: {}\n",
        ctx.url("/sitemap.xml")
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// One sitemap URL: the page's path per locale (for hreflang) and when it last changed.
struct Entry {
    /// Locale -> path (`/p/<slug>`); the entry is listed for markets whose locale has one.
    paths: BTreeMap<String, String>,
    /// Products: the price lists that sell it now (a market lists it only if its list does).
    lists: Option<BTreeSet<Uuid>>,
    lastmod: Option<DateTime<Utc>>,
}

impl Entry {
    fn path_for(&self, m: &MarketCtx) -> Option<String> {
        let sold = match &self.lists {
            None => true,
            Some(lists) => m.price_list_id.is_some_and(|l| lists.contains(&l)),
        };
        self.paths.get(&m.default_locale).filter(|_| sold).cloned()
    }
}

async fn entries(tx: &mut TenantTx, ctx: &Context) -> Result<Vec<Entry>, Error> {
    let mut out = vec![Entry {
        paths: ctx
            .markets
            .iter()
            .map(|m| (m.default_locale.clone(), "/".to_owned()))
            .collect(),
        lists: None,
        lastmod: None,
    }];
    let mut categories: BTreeMap<Uuid, Entry> = BTreeMap::new();
    for r in sqlx::query!(
        "SELECT ct.category_id, ct.locale, ct.slug, c.updated_at FROM category_translations ct
         JOIN categories c ON c.id = ct.category_id ORDER BY ct.category_id"
    )
    .fetch_all(&mut **tx)
    .await?
    {
        categories
            .entry(r.category_id)
            .or_insert(Entry {
                paths: BTreeMap::new(),
                lists: None,
                lastmod: Some(r.updated_at),
            })
            .paths
            .insert(r.locale, format!("/c/{}", r.slug));
    }
    out.extend(categories.into_values());
    // Active products and the price lists that sell them now; each market (this one and the
    // hreflang alternates) lists a product only where it has a price.
    let mut sold: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for r in sqlx::query!(
        "SELECT DISTINCT v.product_id, pi.price_list_id FROM variants v
         JOIN products p ON p.id = v.product_id AND p.status = 'active'
         JOIN price_intervals pi ON pi.variant_id = v.id
          AND pi.valid_from <= $1 AND (pi.valid_to IS NULL OR pi.valid_to > $1)",
        ctx.now
    )
    .fetch_all(&mut **tx)
    .await?
    {
        sold.entry(r.product_id)
            .or_default()
            .insert(r.price_list_id);
    }
    let ids: Vec<Uuid> = sold.keys().copied().collect();
    let mut products: BTreeMap<Uuid, Entry> = BTreeMap::new();
    for r in sqlx::query!(
        "SELECT pt.product_id, pt.locale, pt.slug, p.updated_at FROM product_translations pt
         JOIN products p ON p.id = pt.product_id
         WHERE pt.product_id = ANY($1)
         ORDER BY pt.product_id",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        products
            .entry(r.product_id)
            .or_insert(Entry {
                paths: BTreeMap::new(),
                lists: sold.get(&r.product_id).cloned(),
                lastmod: Some(r.updated_at),
            })
            .paths
            .insert(r.locale, format!("/p/{}", r.slug));
    }
    out.extend(products.into_values());
    // Only what exists in this market's language.
    out.retain(|e| e.path_for(&ctx.market).is_some());
    Ok(out)
}

/// `sitemap.xml`: the index of this market's chunks.
pub async fn sitemap_index(tx: &mut TenantTx, ctx: &Context) -> Result<String, Error> {
    let n = entries(tx, ctx).await?.len().div_ceil(SITEMAP_CHUNK).max(1);
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for i in 1..=n {
        let _ = writeln!(
            xml,
            "  <sitemap><loc>{}</loc></sitemap>",
            xml_escape(&ctx.url(&format!("/sitemap-{i}.xml")))
        );
    }
    xml.push_str("</sitemapindex>\n");
    Ok(xml)
}

/// `sitemap-<n>.xml` (1-based); `None` past the last chunk.
pub async fn sitemap_chunk(
    tx: &mut TenantTx,
    ctx: &Context,
    n: usize,
) -> Result<Option<String>, Error> {
    let all = entries(tx, ctx).await?;
    if n == 0 || (n - 1) * SITEMAP_CHUNK >= all.len().max(1) {
        return Ok(None);
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    for e in all.iter().skip((n - 1) * SITEMAP_CHUNK).take(SITEMAP_CHUNK) {
        let Some(path) = e.path_for(&ctx.market) else {
            continue;
        };
        let _ = write!(xml, "  <url><loc>{}</loc>", xml_escape(&ctx.url(&path)));
        if let Some(t) = e.lastmod {
            let _ = write!(xml, "<lastmod>{}</lastmod>", t.format("%Y-%m-%d"));
        }
        for a in alternates(ctx, |m| e.path_for(m)) {
            let _ = write!(
                xml,
                "<xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>",
                xml_escape(&a.locale),
                xml_escape(&a.href)
            );
        }
        xml.push_str("</url>\n");
    }
    xml.push_str("</urlset>\n");
    Ok(Some(xml))
}

/// `llms.txt` (llmstxt.org): what the shop is, its main categories and where the machine
/// readable data lives.
pub async fn llms(tx: &mut TenantTx, ctx: &Context) -> Result<String, Error> {
    let cats = sqlx::query!(
        r#"SELECT t.name AS "name!", t.slug AS "slug!" FROM categories c
           CROSS JOIN LATERAL (
               SELECT name, slug FROM category_translations ct WHERE ct.category_id = c.id
               ORDER BY (ct.locale = $1) DESC, (ct.locale = $2) DESC, ct.locale LIMIT 1
           ) t
           WHERE c.parent_id IS NULL ORDER BY c.position, c.id"#,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?;
    let other: HashMap<&str, &str> = ctx
        .markets
        .iter()
        .filter(|m| m.id != ctx.market.id)
        .filter_map(|m| Some((m.name.as_str(), m.base_url.as_deref()?)))
        .collect();
    let mut out = format!(
        "# {}\n\n> Online shop ({}, prices in {} incl. VAT).\n\n## Categories\n\n",
        ctx.shop_name,
        ctx.market.name,
        ctx.market.currency.code()
    );
    for c in cats {
        let _ = writeln!(
            out,
            "- [{}]({})",
            c.name,
            ctx.url(&format!("/c/{}", c.slug))
        );
    }
    out.push_str("\n## Machine-readable\n\n");
    let _ = writeln!(out, "- [Sitemap]({})", ctx.url("/sitemap.xml"));
    if !other.is_empty() {
        out.push_str("\n## Other markets\n\n");
        let mut other: Vec<_> = other.into_iter().collect();
        other.sort_unstable();
        for (name, base) in other {
            let _ = writeln!(out, "- [{name}]({base}/)");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped() {
        assert_eq!(xml_escape("/c/a&b<\"'>"), "/c/a&amp;b&lt;&quot;&apos;&gt;");
    }
}
