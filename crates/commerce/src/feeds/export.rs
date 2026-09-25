//! Export feeds per market (spec §10.8, A28): Google Merchant (RSS 2.0 + `g:`), Heureka and
//! Zboží (its own namespace), one serializer each.
//!
//! Only active products with a price in the market's price list and a translation are listed,
//! one entry per sellable variant (`ITEMGROUP_ID`/`g:item_group_id` ties variants together).
//! A reduction (`g:sale_price`) is exported only when Omnibus allows the claim (A18); Heureka
//! and Zboží carry just the current price. Unit prices go out where the channel has fields for
//! them (Google). Comparison sites list only items that can be ordered.
//!
//! The worker regenerates the files hourly and a few minutes after catalog, price or stock
//! changes ([`job_for_event`]), stores them in the private bucket, and the storefront API
//! serves them at `/feeds/<market>/<channel>.xml` through the edge.

use std::collections::HashMap;
use std::fmt::Write as _;

use chrono::{DateTime, Timelike, Utc};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::NewJob;
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::media::AssetVariant;
use crate::storefront::cards::{self, StockState};
use crate::storefront::{self, Context, PublicUrls, plain_excerpt};

/// Regenerates every channel of every market of a tenant.
pub const JOB: &str = "feeds.export";
/// Hourly: queues [`JOB`] for every tenant.
pub const ALL_JOB: &str = "feeds.export_all";
/// Changes within this window regenerate the feeds once, at its end.
const DEBOUNCE_SECS: i64 = 300;
const DESCRIPTION_CHARS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Google,
    Heureka,
    Zbozi,
}

impl Channel {
    pub const ALL: [Self; 3] = [Self::Google, Self::Heureka, Self::Zbozi];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Heureka => "heureka",
            Self::Zbozi => "zbozi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    InStock,
    Backorder,
    OutOfStock,
}

/// One sellable variant as the channels see it (absolute URLs, localized texts).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportItem {
    /// The SKU.
    pub id: String,
    /// Set when the product has several variants.
    pub group_id: Option<String>,
    /// Product name plus the variant's option labels.
    pub title: String,
    pub product_name: String,
    pub description: String,
    pub link: String,
    pub images: Vec<String>,
    pub price_minor: i64,
    /// The Omnibus reference price, only while a reduction may be claimed (A18).
    pub reference_minor: Option<i64>,
    pub currency: String,
    pub availability: Availability,
    pub brand: Option<String>,
    /// GPSR manufacturer (Heureka/Zboží `MANUFACTURER` when there is no brand).
    pub manufacturer: Option<String>,
    pub ean: Option<String>,
    pub category_path: Vec<String>,
    pub heureka_category: Option<String>,
    pub google_category: Option<String>,
    /// Parameters and option values, localized.
    pub params: Vec<(String, String)>,
    /// Content and unit (`kg`, `l`, `m`, `m2`, `pcs`) for unit pricing.
    pub unit: Option<(f64, String)>,
}

pub struct FeedMeta<'a> {
    pub shop_name: &'a str,
    pub base_url: &'a str,
}

// ---------------------------------------------------------------------------------------
// Serializers

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 cannot carry.
            c if c.is_control() && !matches!(c, '\n' | '\t' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn el(out: &mut String, indent: &str, name: &str, value: &str) {
    if !value.is_empty() {
        let _ = writeln!(out, "{indent}<{name}>{}</{name}>", esc(value));
    }
}

/// `29900` -> `299`, `29990` -> `299.90` (Heureka, Zboží).
fn decimal(minor: i64) -> String {
    let (units, cents) = (minor / 100, (minor % 100).abs());
    if cents == 0 {
        units.to_string()
    } else {
        format!("{units}.{cents:02}")
    }
}

/// Google price: `299.00 CZK`.
fn google_price(minor: i64, currency: &str) -> String {
    format!("{}.{:02} {currency}", minor / 100, (minor % 100).abs())
}

fn google_unit(unit: &str) -> &str {
    match unit {
        "m2" => "sqm",
        "pcs" => "ct",
        u => u,
    }
}

pub fn google(meta: &FeedMeta<'_>, items: &[ExportItem]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\" xmlns:g=\"http://base.google.com/ns/1.0\">\n<channel>\n",
    );
    el(&mut out, "", "title", meta.shop_name);
    el(&mut out, "", "link", meta.base_url);
    el(&mut out, "", "description", meta.shop_name);
    for i in items {
        out.push_str("<item>\n");
        let ind = "  ";
        el(&mut out, ind, "g:id", &i.id);
        el(&mut out, ind, "g:title", &i.title);
        el(&mut out, ind, "g:description", &i.description);
        el(&mut out, ind, "g:link", &i.link);
        if let Some((first, rest)) = i.images.split_first() {
            el(&mut out, ind, "g:image_link", first);
            for img in rest.iter().take(10) {
                el(&mut out, ind, "g:additional_image_link", img);
            }
        }
        el(
            &mut out,
            ind,
            "g:availability",
            match i.availability {
                Availability::InStock => "in_stock",
                Availability::Backorder => "backorder",
                Availability::OutOfStock => "out_of_stock",
            },
        );
        match i.reference_minor {
            Some(reference) => {
                el(
                    &mut out,
                    ind,
                    "g:price",
                    &google_price(reference, &i.currency),
                );
                el(
                    &mut out,
                    ind,
                    "g:sale_price",
                    &google_price(i.price_minor, &i.currency),
                );
            }
            None => el(
                &mut out,
                ind,
                "g:price",
                &google_price(i.price_minor, &i.currency),
            ),
        }
        el(&mut out, ind, "g:condition", "new");
        el(
            &mut out,
            ind,
            "g:brand",
            i.brand.as_deref().unwrap_or_default(),
        );
        match &i.ean {
            Some(ean) => el(&mut out, ind, "g:gtin", ean),
            None => el(&mut out, ind, "g:mpn", &i.id),
        }
        if i.ean.is_none() && i.brand.is_none() {
            el(&mut out, ind, "g:identifier_exists", "no");
        }
        el(
            &mut out,
            ind,
            "g:item_group_id",
            i.group_id.as_deref().unwrap_or_default(),
        );
        el(
            &mut out,
            ind,
            "g:product_type",
            &i.category_path.join(" > "),
        );
        el(
            &mut out,
            ind,
            "g:google_product_category",
            i.google_category.as_deref().unwrap_or_default(),
        );
        if let Some((q, unit)) = &i.unit {
            let u = google_unit(unit);
            el(&mut out, ind, "g:unit_pricing_measure", &format!("{q}{u}"));
            el(
                &mut out,
                ind,
                "g:unit_pricing_base_measure",
                &format!("1{u}"),
            );
        }
        for (name, value) in &i.params {
            let _ = writeln!(
                out,
                "{ind}<g:product_detail><g:attribute_name>{}</g:attribute_name><g:attribute_value>{}</g:attribute_value></g:product_detail>",
                esc(name),
                esc(value)
            );
        }
        out.push_str("</item>\n");
    }
    out.push_str("</channel>\n</rss>\n");
    out
}

/// Heureka and Zboží share most of the `SHOPITEM` format.
fn shopitems(out: &mut String, items: &[ExportItem], zbozi: bool) {
    for i in items {
        // Comparison sites list only what can be ordered now.
        let delivery = match i.availability {
            Availability::InStock => "0",
            Availability::Backorder => "14",
            Availability::OutOfStock => continue,
        };
        out.push_str("<SHOPITEM>\n");
        let ind = "  ";
        el(out, ind, "ITEM_ID", &i.id);
        el(out, ind, "PRODUCTNAME", &i.title);
        el(out, ind, "PRODUCT", &i.title);
        el(out, ind, "DESCRIPTION", &i.description);
        el(out, ind, "URL", &i.link);
        if let Some((first, rest)) = i.images.split_first() {
            el(out, ind, "IMGURL", first);
            for img in rest.iter().take(10) {
                el(out, ind, "IMGURL_ALTERNATIVE", img);
            }
        }
        el(out, ind, "PRICE_VAT", &decimal(i.price_minor));
        let manufacturer = i.brand.as_deref().or(i.manufacturer.as_deref());
        el(out, ind, "MANUFACTURER", manufacturer.unwrap_or_default());
        if zbozi {
            el(out, ind, "BRAND", i.brand.as_deref().unwrap_or_default());
        }
        let category = match (&i.heureka_category, zbozi) {
            (Some(h), false) => h.clone(),
            _ => i.category_path.join(" | "),
        };
        el(out, ind, "CATEGORYTEXT", &category);
        el(out, ind, "EAN", i.ean.as_deref().unwrap_or_default());
        if zbozi {
            el(out, ind, "PRODUCTNO", &i.id);
        }
        el(out, ind, "DELIVERY_DATE", delivery);
        el(
            out,
            ind,
            "ITEMGROUP_ID",
            i.group_id.as_deref().unwrap_or_default(),
        );
        for (name, value) in &i.params {
            let _ = writeln!(
                out,
                "{ind}<PARAM><PARAM_NAME>{}</PARAM_NAME><VAL>{}</VAL></PARAM>",
                esc(name),
                esc(value)
            );
        }
        out.push_str("</SHOPITEM>\n");
    }
}

pub fn heureka(items: &[ExportItem]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SHOP>\n");
    shopitems(&mut out, items, false);
    out.push_str("</SHOP>\n");
    out
}

/// Zboží.cz offer feed (`http://www.zbozi.cz/ns/offer/1.0`).
pub fn zbozi(items: &[ExportItem]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<SHOP xmlns=\"http://www.zbozi.cz/ns/offer/1.0\">\n",
    );
    shopitems(&mut out, items, true);
    out.push_str("</SHOP>\n");
    out
}

pub fn render(channel: Channel, meta: &FeedMeta<'_>, items: &[ExportItem]) -> String {
    match channel {
        Channel::Google => google(meta, items),
        Channel::Heureka => heureka(items),
        Channel::Zbozi => zbozi(items),
    }
}

// ---------------------------------------------------------------------------------------
// Loading

/// The largest JPEG/PNG variant (channels want big, widely supported images).
fn feed_image(ctx: &Context, variants: &Value) -> Option<String> {
    let v: Vec<AssetVariant> = serde_json::from_value(variants.clone()).ok()?;
    v.iter()
        .filter(|v| v.format == "jpeg" || v.format == "png")
        .max_by_key(|v| v.width)
        .map(|v| ctx.url(&format!("/{}", v.key)))
}

fn param_text(ctx: &Context, kind: &str, value: &Value, unit: Option<&str>) -> Option<String> {
    let yes_no = |b: bool| {
        let (y, n) = match ctx.locale.split('-').next() {
            Some("cs") => ("ano", "ne"),
            Some("sk") => ("áno", "nie"),
            _ => ("yes", "no"),
        };
        if b { y } else { n }.to_owned()
    };
    let text = match kind {
        "number" => value.as_f64().map(|n| match unit {
            Some(u) => format!("{n} {u}"),
            None => n.to_string(),
        }),
        "bool" => value.as_bool().map(yes_no),
        _ => ctx.text(value),
    }?;
    (!text.is_empty()).then_some(text)
}

/// Every exportable variant of the market in `ctx` (see the module docs for the rules).
pub async fn items(tx: &mut TenantTx, ctx: &Context) -> Result<Vec<ExportItem>, Error> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar!("SELECT id FROM products WHERE status = 'active' ORDER BY id")
            .fetch_all(&mut **tx)
            .await?;
    let mut out = Vec::new();
    // ponytail: batches of 500 products keep memory flat; stream to storage if feeds get huge.
    for chunk in ids.chunks(500) {
        out.extend(items_of(tx, ctx, chunk).await?);
    }
    Ok(out)
}

/// (product, variant or `None` for product level).
type MediaKey = (Uuid, Option<Uuid>);
/// Option code, option name, value code -> value name.
type OptionLabels = (String, String, HashMap<String, String>);

async fn items_of(
    tx: &mut TenantTx,
    ctx: &Context,
    ids: &[Uuid],
) -> Result<Vec<ExportItem>, Error> {
    let products = cards::products(tx, ctx, ids).await?;
    let variants = cards::priced_variants(tx, ctx, ids).await?;
    let extra: HashMap<Uuid, _> = sqlx::query!(
        r#"SELECT p.id, p.gpsr -> 'manufacturer' ->> 'name' AS manufacturer, p.heureka_category,
                  p.google_category,
                  (SELECT pt.description_html FROM product_translations pt WHERE pt.product_id = p.id
                   ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1) AS description,
                  (SELECT pc.category_id FROM product_categories pc WHERE pc.product_id = p.id
                   ORDER BY pc.category_id LIMIT 1) AS category_id
           FROM products p WHERE p.id = ANY($1)"#,
        ids,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect();
    let mut images: HashMap<(Uuid, Option<Uuid>), Vec<String>> = HashMap::new();
    for m in cards::media(tx, ids, Some(11)).await? {
        if let Some(url) = feed_image(ctx, &m.variants) {
            images
                .entry((m.product_id, m.variant_id))
                .or_default()
                .push(url);
        }
    }
    let mut params: HashMap<MediaKey, Vec<(String, String)>> = HashMap::new();
    for r in sqlx::query!(
        "SELECT v.product_id, v.variant_id, v.value, p.name_i18n, p.kind, p.unit
         FROM product_parameter_values v JOIN parameters p ON p.id = v.parameter_id
         WHERE v.product_id = ANY($1) ORDER BY v.product_id, v.position",
        ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let (Some(name), Some(value)) = (
            ctx.text(&r.name_i18n),
            param_text(ctx, &r.kind, &r.value, r.unit.as_deref()),
        ) else {
            continue;
        };
        params
            .entry((r.product_id, r.variant_id))
            .or_default()
            .push((name, value));
    }
    // Option labels: product -> option code -> (option name, value code -> value name).
    let mut options: HashMap<Uuid, Vec<OptionLabels>> = HashMap::new();
    for r in sqlx::query!(
        r#"SELECT product_id, code, name_i18n, "values" AS vals FROM product_options
           WHERE product_id = ANY($1) ORDER BY product_id, position"#,
        ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let values: Vec<crate::catalog::products::OptionValue> =
            serde_json::from_value(r.vals).unwrap_or_default();
        let labels = values
            .into_iter()
            .filter_map(|v| {
                let label = ctx.text(&serde_json::to_value(&v.name_i18n).ok()?)?;
                Some((v.code, label))
            })
            .collect();
        let name = ctx.text(&r.name_i18n).unwrap_or_else(|| r.code.clone());
        options
            .entry(r.product_id)
            .or_default()
            .push((r.code, name, labels));
    }
    let paths = category_paths(tx, ctx).await?;
    let mut counts: HashMap<Uuid, usize> = HashMap::new();
    for v in &variants {
        *counts.entry(v.product_id).or_default() += 1;
    }

    let mut out = Vec::with_capacity(variants.len());
    for v in variants {
        let Some(p) = products.get(&v.product_id) else {
            continue;
        };
        let x = extra.get(&v.product_id);
        let mut labels = vec![];
        let mut item_params = params
            .get(&(v.product_id, None))
            .cloned()
            .unwrap_or_default();
        item_params.extend(
            params
                .get(&(v.product_id, Some(v.id)))
                .cloned()
                .unwrap_or_default(),
        );
        for (code, name, values) in options.get(&v.product_id).into_iter().flatten() {
            if let Some(label) = v.option_values.get(code).and_then(|c| values.get(c)) {
                labels.push(label.clone());
                item_params.push((name.clone(), label.clone()));
            }
        }
        let mut imgs = images
            .get(&(v.product_id, Some(v.id)))
            .cloned()
            .unwrap_or_default();
        imgs.extend(
            images
                .get(&(v.product_id, None))
                .cloned()
                .unwrap_or_default(),
        );
        imgs.dedup();
        let o = &v.price.omnibus;
        let claim = o.claim && o.reference_minor.is_some();
        let grouped = counts.get(&v.product_id).copied().unwrap_or(0) > 1;
        // Each variant lands on its own offer: the product page preselects `?variant=<sku>`.
        let link = {
            let page = ctx.page_url(&format!("/p/{}", p.slug));
            match reqwest::Url::parse(&page) {
                Ok(mut u) if grouped => {
                    u.query_pairs_mut().append_pair("variant", &v.sku);
                    u.to_string()
                }
                _ => page,
            }
        };
        out.push(ExportItem {
            id: v.sku.clone(),
            group_id: grouped.then(|| v.product_id.to_string()),
            title: if labels.is_empty() {
                p.name.clone()
            } else {
                format!("{} {}", p.name, labels.join(" "))
            },
            product_name: p.name.clone(),
            description: x
                .and_then(|x| x.description.as_deref())
                .map(|d| plain_excerpt(d, DESCRIPTION_CHARS))
                .unwrap_or_default(),
            link,
            images: imgs,
            price_minor: v.price.amount_minor,
            reference_minor: o.reference_minor.filter(|_| claim),
            currency: ctx.market.currency.code().to_owned(),
            availability: match v.stock {
                StockState::InStock | StockState::LowStock => Availability::InStock,
                StockState::Backorder => Availability::Backorder,
                StockState::OutOfStock => Availability::OutOfStock,
            },
            brand: p.brand.clone(),
            manufacturer: x.and_then(|x| x.manufacturer.clone()),
            ean: v.ean.clone(),
            category_path: x
                .and_then(|x| x.category_id)
                .and_then(|c| paths.get(&c).cloned())
                .unwrap_or_default(),
            heureka_category: x.and_then(|x| x.heureka_category.clone()),
            google_category: x.and_then(|x| x.google_category.clone()),
            params: item_params,
            unit: p
                .unit_measure
                .clone()
                .zip(p.unit_quantity)
                .map(|(m, q)| (q, m)),
        });
    }
    Ok(out)
}

/// Category id -> names from the root, in the request locale.
async fn category_paths(
    tx: &mut TenantTx,
    ctx: &Context,
) -> Result<HashMap<Uuid, Vec<String>>, Error> {
    let rows = sqlx::query!(
        r#"SELECT c.id, c.parent_id, t.name AS "name!" FROM categories c
           CROSS JOIN LATERAL (
               SELECT name FROM category_translations ct WHERE ct.category_id = c.id
               ORDER BY (ct.locale = $1) DESC, (ct.locale = $2) DESC, ct.locale LIMIT 1
           ) t"#,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?;
    let nodes: HashMap<Uuid, (Option<Uuid>, String)> = rows
        .into_iter()
        .map(|r| (r.id, (r.parent_id, r.name)))
        .collect();
    Ok(nodes
        .keys()
        .map(|id| {
            let mut path = vec![];
            let mut cur = Some(*id);
            while let Some(c) = cur {
                let Some((parent, name)) = nodes.get(&c) else {
                    break;
                };
                if path.len() > 16 {
                    break;
                }
                path.push(name.clone());
                cur = *parent;
            }
            path.reverse();
            (*id, path)
        })
        .collect())
}

// ---------------------------------------------------------------------------------------
// Generation, storage, serving

fn key(tenant_id: Uuid, market_id: Uuid, channel: Channel) -> Path {
    Path::from(format!(
        "feeds/{tenant_id}/{market_id}/{}.xml",
        channel.as_str()
    ))
}

/// Regenerates all channels for every market that has a price list and a verified domain.
/// Returns the number of files written.
pub async fn generate(
    db: &PgPool,
    storage: &Storage,
    urls: &PublicUrls,
    tenant_id: Uuid,
) -> Result<usize, Error> {
    // One generation per tenant at a time (hourly, debounced and manual jobs may overlap):
    // a slower, older run must not overwrite a newer file. Held until this function returns.
    let mut guard = db.begin().await?;
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('feeds:' || $1::text, 0))",
        tenant_id.to_string()
    )
    .fetch_one(&mut *guard)
    .await?;
    let mut tx = tenant_tx(db, tenant_id).await?;
    let markets: Vec<Uuid> =
        sqlx::query_scalar!("SELECT id FROM markets WHERE price_list_id IS NOT NULL ORDER BY code")
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    let mut written = 0;
    for market in markets {
        let mut tx = tenant_tx(db, tenant_id).await?;
        let now = Utc::now();
        let ctx = match storefront::context(&mut tx, urls, market, None, now).await {
            Ok(ctx) => ctx,
            // No verified domain: no public URLs to put in a feed yet.
            Err(Error::NotFound) => continue,
            Err(e) => return Err(e),
        };
        let items = items(&mut tx, &ctx).await?;
        tx.commit().await?;
        let meta = FeedMeta {
            shop_name: &ctx.shop_name,
            base_url: &ctx.base_url,
        };
        for channel in Channel::ALL {
            let xml = render(channel, &meta, &items);
            let bytes = i64::try_from(xml.len()).unwrap_or(i64::MAX);
            let path = key(tenant_id, market, channel);
            storage
                .private
                .put(&path, PutPayload::from(xml.into_bytes()))
                .await?;
            // Comparison sites list only orderable items.
            let listed = match channel {
                Channel::Google => items.len(),
                Channel::Heureka | Channel::Zbozi => items
                    .iter()
                    .filter(|i| i.availability != Availability::OutOfStock)
                    .count(),
            };
            let count = i32::try_from(listed).unwrap_or(i32::MAX);
            let mut tx = tenant_tx(db, tenant_id).await?;
            sqlx::query!(
                "INSERT INTO feed_exports (tenant_id, market_id, channel, object_key, items, bytes,
                                           generated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, now())
                 ON CONFLICT (tenant_id, market_id, channel) DO UPDATE
                 SET object_key = $4, items = $5, bytes = $6, generated_at = now()",
                tenant_id,
                market,
                channel.as_str(),
                path.as_ref(),
                count,
                bytes
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            written += 1;
        }
    }
    guard.commit().await?;
    Ok(written)
}

/// The stored feed of a market (by code) and channel, if generated.
pub async fn stored(
    tx: &mut TenantTx,
    storage: &Storage,
    market_code: &str,
    channel: Channel,
) -> Result<Option<Vec<u8>>, Error> {
    let Some(key) = sqlx::query_scalar!(
        "SELECT f.object_key FROM feed_exports f JOIN markets m ON m.id = f.market_id
         WHERE m.code = $1 AND f.channel = $2",
        market_code,
        channel.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    match storage.private.get(&Path::from(key)).await {
        Ok(r) => Ok(Some(r.bytes().await?.to_vec())),
        Err(object_store::Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeedFile {
    pub market_id: Uuid,
    pub market_code: String,
    pub channel: Channel,
    /// Public URL (`https://<shop>/feeds/<market>/<channel>.xml`); `None` without a domain.
    pub url: Option<String>,
    pub items: Option<i32>,
    pub bytes: Option<i64>,
    pub generated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeedFileList {
    pub items: Vec<FeedFile>,
}

/// Every market x channel with its generation state and public URL.
pub async fn list(tx: &mut TenantTx, urls: &PublicUrls) -> Result<FeedFileList, Error> {
    let markets = sqlx::query!(
        "SELECT m.id, m.code,
                (SELECT d.hostname FROM platform.domains d
                 WHERE d.market_id = m.id AND d.verified_at IS NOT NULL
                 ORDER BY d.is_primary DESC, d.hostname LIMIT 1) AS host
         FROM markets m ORDER BY m.is_default DESC, m.code"
    )
    .fetch_all(&mut **tx)
    .await?;
    let files: HashMap<(Uuid, String), (i32, i64, DateTime<Utc>)> =
        sqlx::query!("SELECT market_id, channel, items, bytes, generated_at FROM feed_exports")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| ((r.market_id, r.channel), (r.items, r.bytes, r.generated_at)))
            .collect();
    let mut items = vec![];
    for m in markets {
        for channel in Channel::ALL {
            let f = files.get(&(m.id, channel.as_str().to_owned()));
            items.push(FeedFile {
                market_id: m.id,
                url: m
                    .host
                    .as_ref()
                    .map(|h| format!("{}/feeds/{}/{}.xml", urls.base(h), m.code, channel.as_str())),
                market_code: m.code.clone(),
                channel,
                items: f.map(|f| f.0),
                bytes: f.map(|f| f.1),
                generated_at: f.map(|f| f.2),
            });
        }
    }
    Ok(FeedFileList { items })
}

/// A regeneration of the tenant's feeds. `slot`: the debounce window (one job per window).
pub fn job(tenant_id: Uuid, now: DateTime<Utc>, debounce: bool) -> NewJob<'static> {
    let mut j = NewJob::new(JOB, json!({}));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 5;
    if debounce {
        let slot = now.timestamp().div_euclid(DEBOUNCE_SECS);
        j.idempotency_key = Some(format!("{JOB}:{tenant_id}:{slot}"));
        j.run_at = DateTime::from_timestamp((slot + 1) * DEBOUNCE_SECS, 0);
    } else {
        j.idempotency_key = Some(format!(
            "{JOB}:{tenant_id}:now:{}",
            now.with_nanosecond(0).unwrap_or(now).timestamp()
        ));
    }
    j
}

/// Catalog, price and stock changes regenerate the tenant's feeds at the end of the current
/// debounce window.
pub fn job_for_event(
    tenant_id: Option<Uuid>,
    event_type: &str,
    now: DateTime<Utc>,
) -> Option<NewJob<'static>> {
    let affects = matches!(
        event_type,
        "product.created"
            | "product.updated"
            | "product.deleted"
            | "price.changed"
            | "inventory.changed"
            | "category.updated"
            | "category.moved"
            | "category.deleted"
            | "asset.ready"
    );
    affects.then_some(())?;
    Some(job(tenant_id?, now, true))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn money_formats() {
        assert_eq!(decimal(29_900), "299");
        assert_eq!(decimal(29_990), "299.90");
        assert_eq!(decimal(5), "0.05");
        assert_eq!(google_price(29_900, "CZK"), "299.00 CZK");
    }

    #[test]
    fn events_debounce_into_one_job_per_window() {
        let t = Uuid::now_v7();
        let now = DateTime::from_timestamp(1_000_000_123, 0).unwrap();
        let a = job_for_event(Some(t), "price.changed", now).unwrap();
        let b = job_for_event(
            Some(t),
            "product.updated",
            now + chrono::Duration::seconds(60),
        )
        .unwrap();
        assert_eq!(a.idempotency_key, b.idempotency_key);
        assert!(a.run_at.unwrap() > now);
        assert!(job_for_event(Some(t), "coupon.created", now).is_none());
        assert!(job_for_event(None, "price.changed", now).is_none());
    }

    fn sample() -> Vec<ExportItem> {
        let base = ExportItem {
            id: "TB-M".into(),
            group_id: Some("0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into()),
            title: "Tričko Basic černá M".into(),
            product_name: "Tričko Basic".into(),
            description: "Klasické tričko ze 100% bavlny & více.".into(),
            link: "https://demo.example/p/tricko-basic".into(),
            images: vec![
                "https://demo.example/media/t/a/1.jpg".into(),
                "https://demo.example/media/t/a/2.jpg".into(),
            ],
            price_minor: 26_910,
            reference_minor: Some(29_900),
            currency: "CZK".into(),
            availability: Availability::InStock,
            brand: Some("Basic".into()),
            manufacturer: Some("Basic Textil s.r.o.".into()),
            ean: Some("8594001021499".into()),
            category_path: vec!["Oblečení".into(), "Trička".into()],
            heureka_category: Some("Heureka.cz | Oblečení a móda | Trička".into()),
            google_category: Some("212".into()),
            params: vec![
                ("Materiál".into(), "bavlna".into()),
                ("Barva".into(), "černá".into()),
            ],
            unit: None,
        };
        vec![
            base.clone(),
            ExportItem {
                id: "SV-1".into(),
                group_id: None,
                title: "Svíčka <levandule>".into(),
                product_name: "Svíčka <levandule>".into(),
                description: String::new(),
                link: "https://demo.example/p/svicka".into(),
                images: vec![],
                price_minor: 29_990,
                reference_minor: None,
                availability: Availability::Backorder,
                brand: None,
                manufacturer: Some("Světlo".into()),
                ean: None,
                category_path: vec!["Domácnost".into()],
                heureka_category: None,
                google_category: None,
                params: vec![],
                unit: Some((0.2, "kg".into())),
                ..base.clone()
            },
            ExportItem {
                id: "GONE".into(),
                availability: Availability::OutOfStock,
                reference_minor: None,
                ..base
            },
        ]
    }

    /// Element paths and texts per item (plus the document root and its namespace), order-
    /// insensitive within an item: what a channel reads, not how it is formatted.
    fn semantic(xml: &str) -> (String, Vec<Vec<(String, String)>>) {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut root = String::new();
        let mut items: Vec<Vec<(String, String)>> = vec![];
        let mut path: Vec<String> = vec![];
        let mut in_item = false;
        loop {
            match reader.read_event().unwrap() {
                Event::Start(e) => {
                    let name = String::from_utf8(e.name().as_ref().to_vec()).unwrap();
                    if root.is_empty() {
                        let ns: Vec<String> = e
                            .attributes()
                            .map(|a| {
                                let a = a.unwrap();
                                format!(
                                    "{}={}",
                                    String::from_utf8_lossy(a.key.as_ref()),
                                    a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                        .unwrap()
                                )
                            })
                            .collect();
                        root = format!("{name} {}", ns.join(" "));
                    }
                    if name == "item" || name == "SHOPITEM" {
                        in_item = true;
                        items.push(vec![]);
                        path.clear();
                    } else if in_item {
                        path.push(name);
                    }
                }
                Event::End(e) => {
                    let name = String::from_utf8(e.name().as_ref().to_vec()).unwrap();
                    if name == "item" || name == "SHOPITEM" {
                        in_item = false;
                        items.last_mut().unwrap().sort();
                    } else if in_item {
                        path.pop();
                    }
                }
                Event::Text(t) if in_item => {
                    let text = t.xml10_content().unwrap().into_owned();
                    items.last_mut().unwrap().push((path.join("/"), text));
                }
                Event::GeneralRef(r) if in_item => {
                    let name = r.decode().unwrap();
                    let c = quick_xml::escape::resolve_predefined_entity(&name).unwrap();
                    let last = items.last_mut().unwrap();
                    // An entity continues the text of the current element.
                    match last.last_mut() {
                        Some((p, t)) if *p == path.join("/") => t.push_str(c),
                        _ => last.push((path.join("/"), c.to_owned())),
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        (root, items)
    }

    #[test]
    fn serializers_match_the_semantic_fixtures() {
        let items = sample();
        let meta = FeedMeta {
            shop_name: "Demo & spol.",
            base_url: "https://demo.example",
        };
        for (channel, fixture) in [
            (
                Channel::Google,
                include_str!("../../../../fixtures/feeds/export/google.xml"),
            ),
            (
                Channel::Heureka,
                include_str!("../../../../fixtures/feeds/export/heureka.xml"),
            ),
            (
                Channel::Zbozi,
                include_str!("../../../../fixtures/feeds/export/zbozi.xml"),
            ),
        ] {
            let xml = render(channel, &meta, &items);
            if std::env::var("WRITE_FEED_FIXTURES").is_ok() {
                std::fs::write(
                    format!("../../fixtures/feeds/export/{}.xml", channel.as_str()),
                    &xml,
                )
                .unwrap();
            }
            assert_eq!(semantic(&xml), semantic(fixture), "{channel:?}");
        }
    }

    #[test]
    fn channel_rules() {
        let items = sample();
        let meta = FeedMeta {
            shop_name: "Demo",
            base_url: "https://demo.example",
        };
        let (_, google_items) = semantic(&google(&meta, &items));
        assert_eq!(google_items.len(), 3, "Google lists unavailable items too");
        let first = &google_items[0];
        let get = |k: &str| first.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
        assert_eq!(get("g:price"), Some("299.00 CZK"), "the Omnibus reference");
        assert_eq!(get("g:sale_price"), Some("269.10 CZK"));
        let third = &google_items[2];
        assert!(
            !third.iter().any(|(p, _)| p == "g:sale_price"),
            "no reduction without a claim"
        );
        let (_, heureka_items) = semantic(&heureka(&items));
        assert_eq!(heureka_items.len(), 2, "out of stock is left out");
        assert!(heureka_items[0].contains(&("PRICE_VAT".into(), "269.10".into())));
    }

    #[test]
    fn escaping_drops_invalid_characters() {
        assert_eq!(
            esc("a & <b> \"c\" 'd'\u{1}"),
            "a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;"
        );
    }
}
