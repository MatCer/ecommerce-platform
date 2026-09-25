//! Product detail page model (`GET /storefront/v1/pages/product/{slug}`, spec §8.2, §9.2):
//! variants with options, media, effective price + Omnibus reference, unit price, stock state,
//! parameters, GPSR, breadcrumbs, JSON-LD (Product, Offer, BreadcrumbList).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::cards::{self, PriceView, StockState, VariantData};
use super::images::Image;
use super::listing::param_value;
use super::{CacheHints, Context, Link, Seo, alternates, messages, plain_excerpt};
use crate::catalog::products::{Gpsr, GpsrParty};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OptionValueView {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OptionView {
    pub code: String,
    pub name: String,
    pub values: Vec<OptionValueView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct VariantView {
    pub id: Uuid,
    pub sku: String,
    /// Option code -> value code (`{"color": "red", "size": "m"}`).
    pub options: BTreeMap<String, String>,
    #[serde(flatten)]
    pub price: PriceView,
    pub stock: StockState,
    /// Index into `product.images` of this variant's first image.
    pub image_index: Option<u32>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ParameterView {
    pub key: String,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GpsrPartyView {
    pub name: String,
    pub address: String,
    pub email: Option<String>,
    pub url: Option<String>,
    pub phone: Option<String>,
}

/// General Product Safety Regulation information (Regulation (EU) 2023/988, art. 19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct GpsrView {
    pub manufacturer: Option<GpsrPartyView>,
    pub eu_responsible_person: Option<GpsrPartyView>,
    pub safety_info: Option<String>,
    pub warnings: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ProductDetail {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub brand: Option<String>,
    /// Sanitized server-side; the only HTML a theme may render unescaped.
    pub description_html: String,
    pub short_description: String,
    pub images: Vec<Image>,
    pub options: Vec<OptionView>,
    /// Variants sold in this market, in the merchant's order.
    pub variants: Vec<VariantView>,
    pub parameters: Vec<ParameterView>,
    pub gpsr: Option<GpsrView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DeliveryEstimate {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ProductPage {
    pub product: ProductDetail,
    pub breadcrumbs: Vec<Link>,
    /// Absent when nothing is in stock.
    pub delivery_estimate: Option<DeliveryEstimate>,
    /// Published reviews, their summary and the verification disclosure link (WP16).
    pub reviews: crate::reviews::ProductReviews,
    pub seo: Seo,
    pub cache: CacheHints,
}

/// `days` business days (Mon-Fri) after `from`. ponytail: no public holidays yet (WP10
/// shipping brings carrier calendars).
pub fn add_business_days(from: NaiveDate, days: u32) -> NaiveDate {
    let mut d = from;
    let mut left = days;
    while left > 0 {
        d += Duration::days(1);
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            left -= 1;
        }
    }
    d
}

fn party(p: GpsrParty) -> GpsrPartyView {
    GpsrPartyView {
        name: p.name,
        address: p.address,
        email: p.email,
        url: p.url,
        phone: p.phone,
    }
}

fn gpsr_view(ctx: &Context, raw: Value) -> Option<GpsrView> {
    let g: Gpsr = serde_json::from_value(raw).ok()?;
    let pick = |m: &crate::catalog::I18n| ctx.text(&serde_json::to_value(m).unwrap_or(Value::Null));
    let view = GpsrView {
        safety_info: pick(&g.safety_info),
        warnings: pick(&g.warnings),
        manufacturer: g.manufacturer.map(party),
        eu_responsible_person: g.eu_responsible_person.map(party),
    };
    (view.manufacturer.is_some()
        || view.eu_responsible_person.is_some()
        || view.safety_info.is_some()
        || view.warnings.is_some())
    .then_some(view)
}

/// `12900` -> `"129.00"` (schema.org prices are plain decimals).
pub(crate) fn decimal(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

pub(crate) fn availability(s: StockState) -> &'static str {
    match s {
        StockState::InStock => "https://schema.org/InStock",
        StockState::LowStock => "https://schema.org/LimitedAvailability",
        StockState::Backorder => "https://schema.org/BackOrder",
        StockState::OutOfStock => "https://schema.org/OutOfStock",
    }
}

/// Root-first path of a category as breadcrumb links (`/c/<slug>`).
pub(crate) async fn category_trail(
    tx: &mut TenantTx,
    ctx: &Context,
    category_id: Uuid,
) -> Result<Vec<Link>, Error> {
    Ok(sqlx::query!(
        r#"WITH RECURSIVE up AS (
               SELECT id, parent_id, 0 AS depth FROM categories WHERE id = $1
               UNION ALL
               SELECT c.id, c.parent_id, up.depth + 1 FROM categories c JOIN up ON c.id = up.parent_id
               WHERE up.depth < 32
           )
           SELECT t.name AS "name!", t.slug AS "slug!"
           FROM up CROSS JOIN LATERAL (
               SELECT name, slug FROM category_translations ct WHERE ct.category_id = up.id
               ORDER BY (ct.locale = $2) DESC, (ct.locale = $3) DESC, ct.locale LIMIT 1
           ) t
           ORDER BY up.depth DESC"#,
        category_id,
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
    .collect())
}

pub(crate) fn home_link(ctx: &Context) -> Link {
    Link {
        label: messages::text(&ctx.locale, "nav.home").to_owned(),
        href: ctx.path("/"),
    }
}

pub(crate) fn breadcrumb_ld(ctx: &Context, trail: &[Link]) -> Value {
    json!({
        "@context": "https://schema.org",
        "@type": "BreadcrumbList",
        "itemListElement": trail.iter().enumerate().map(|(i, l)| json!({
            "@type": "ListItem",
            "position": i + 1,
            "name": l.label,
            "item": ctx.url(&l.href),
        })).collect::<Vec<_>>(),
    })
}

/// The product page for `slug` (in the request locale, else any locale), or `None` when the
/// product does not exist, is not active or is not sold in this market.
pub async fn product_page(
    tx: &mut TenantTx,
    ctx: &Context,
    slug: &str,
) -> Result<Option<ProductPage>, Error> {
    let Some(product_id) = sqlx::query_scalar!(
        "SELECT pt.product_id FROM product_translations pt
         JOIN products p ON p.id = pt.product_id
         WHERE pt.slug = $1 AND p.status = 'active'
         ORDER BY (pt.locale = $2) DESC LIMIT 1",
        slug,
        ctx.locale
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let variants: Vec<VariantData> = cards::priced_variants(tx, ctx, &[product_id]).await?;
    if variants.is_empty() {
        return Ok(None);
    }
    let Some(p) = sqlx::query!(
        r#"SELECT p.brand, p.gpsr, p.unit_measure, p.unit_quantity::float8 AS unit_quantity,
                  p.created_at, t.name AS "name!", t.slug AS "slug!",
                  t.description_html AS "description_html!",
                  t.short_description AS "short_description!", t.seo_title, t.seo_description
           FROM products p CROSS JOIN LATERAL (
               SELECT * FROM product_translations pt WHERE pt.product_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           WHERE p.id = $1"#,
        product_id,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let row = cards::ProductRow {
        id: product_id,
        brand: p.brand.clone(),
        created_at: p.created_at,
        unit_measure: p.unit_measure.clone(),
        unit_quantity: p.unit_quantity,
        name: p.name.clone(),
        slug: p.slug.clone(),
    };

    let media = cards::media(tx, &[product_id], None).await?;
    let images: Vec<(Option<Uuid>, Image)> = media
        .iter()
        .filter_map(|m| Some((m.variant_id, cards::image(ctx, m, &p.name)?)))
        .collect();

    let options: Vec<OptionView> = sqlx::query!(
        r#"SELECT code, name_i18n, "values" AS "values!" FROM product_options
           WHERE product_id = $1 ORDER BY position"#,
        product_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|o| OptionView {
        name: ctx.text(&o.name_i18n).unwrap_or_else(|| o.code.clone()),
        values: o
            .values
            .as_array()
            .map(|vs| {
                vs.iter()
                    .filter_map(|v| {
                        let code = v.get("code")?.as_str()?.to_owned();
                        let name = v
                            .get("name_i18n")
                            .and_then(|n| ctx.text(n))
                            .unwrap_or_else(|| code.clone());
                        Some(OptionValueView { code, name })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        code: o.code,
    })
    .collect();

    let parameters: Vec<ParameterView> = sqlx::query!(
        "SELECT pa.key, pa.kind, pa.unit, pa.name_i18n, ppv.value
         FROM product_parameter_values ppv JOIN parameters pa ON pa.id = ppv.parameter_id
         WHERE ppv.product_id = $1 AND ppv.variant_id IS NULL
         ORDER BY ppv.position",
        product_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter_map(|r| {
        let (_, value) = param_value(ctx, &r.kind, r.unit.as_deref(), &r.value)?;
        Some(ParameterView {
            name: ctx.text(&r.name_i18n).unwrap_or_else(|| r.key.clone()),
            key: r.key,
            value,
        })
    })
    .collect();

    let variant_views: Vec<VariantView> = variants
        .iter()
        .map(|v| VariantView {
            id: v.id,
            sku: v.sku.clone(),
            options: v.option_values.clone(),
            price: cards::price_view(ctx, &row, &v.price),
            stock: v.stock,
            image_index: images
                .iter()
                .position(|(vid, _)| *vid == Some(v.id))
                .and_then(|i| u32::try_from(i).ok()),
            is_default: v.is_default,
        })
        .collect();

    // Breadcrumbs: the product's first category (by listing position).
    let category = sqlx::query_scalar!(
        "SELECT category_id FROM product_categories WHERE product_id = $1
         ORDER BY position, category_id LIMIT 1",
        product_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let mut breadcrumbs = vec![home_link(ctx)];
    if let Some(c) = category {
        breadcrumbs.extend(category_trail(tx, ctx, c).await?);
    }
    breadcrumbs.push(Link {
        label: p.name.clone(),
        href: ctx.path(&format!("/p/{}", p.slug)),
    });

    let slugs: BTreeMap<String, String> = sqlx::query!(
        "SELECT locale, slug FROM product_translations WHERE product_id = $1",
        product_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.locale, r.slug))
    .collect();
    // hreflang only points at markets that sell the product (a price in their list now).
    let sold_in: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT DISTINCT pi.price_list_id FROM price_intervals pi
         JOIN variants v ON v.id = pi.variant_id
         WHERE v.product_id = $1 AND pi.valid_from <= $2
           AND (pi.valid_to IS NULL OR pi.valid_to > $2)",
        product_id,
        ctx.now
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();

    let path = ctx.path(&format!("/p/{}", p.slug));
    let canonical = ctx.url(&path);
    let best = variant_views
        .iter()
        .map(|v| v.stock)
        .min()
        .unwrap_or(StockState::OutOfStock);
    let today = ctx.now.date_naive();
    let delivery_estimate =
        matches!(best, StockState::InStock | StockState::LowStock).then(|| DeliveryEstimate {
            from: add_business_days(today, 1),
            to: add_business_days(today, 3),
        });

    let default_variant = variant_views
        .iter()
        .find(|v| v.is_default)
        .or(variant_views.first());
    let description = p
        .seo_description
        .clone()
        .filter(|d| !d.trim().is_empty())
        .or_else(|| Some(p.short_description.clone()).filter(|d| !d.trim().is_empty()))
        .unwrap_or_else(|| plain_excerpt(&p.description_html, 160));
    let product_ld = json!({
        "@context": "https://schema.org",
        "@type": "Product",
        "name": p.name,
        "description": description,
        "sku": default_variant.map(|v| v.sku.clone()),
        "gtin": variants.iter().find(|v| v.is_default).and_then(|v| v.ean.clone()),
        "brand": p.brand.as_ref().map(|b| json!({ "@type": "Brand", "name": b })),
        "image": images.iter().map(|(_, i)| ctx.url(&i.src)).collect::<Vec<_>>(),
        "url": canonical,
        "offers": variant_views.iter().map(|v| json!({
            "@type": "Offer",
            "sku": v.sku,
            "price": decimal(v.price.price.amount_minor),
            "priceCurrency": ctx.market.currency.code(),
            "availability": availability(v.stock),
            "itemCondition": "https://schema.org/NewCondition",
            "url": canonical,
        })).collect::<Vec<_>>(),
    });
    let reviews = crate::reviews::for_product(tx, ctx, product_id).await?;
    let mut product_ld = product_ld;
    if let Some((aggregate, items)) = crate::reviews::json_ld(&reviews) {
        product_ld["aggregateRating"] = aggregate;
        product_ld["review"] = items;
    }
    let product_ld = strip_nulls(product_ld);

    let title = p
        .seo_title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("{} | {}", p.name, ctx.shop_name));
    let seo = Seo {
        title,
        description,
        alternates: alternates(ctx, |m, l| {
            let sold = m.price_list_id.is_some_and(|list| sold_in.contains(&list));
            slugs
                .get(l)
                .or_else(|| slugs.get(&m.default_locale))
                .filter(|_| sold)
                .map(|s| format!("/p/{s}"))
        }),
        json_ld: vec![product_ld, breadcrumb_ld(ctx, &breadcrumbs)],
        canonical,
        robots: None,
    };

    Ok(Some(ProductPage {
        product: ProductDetail {
            id: product_id,
            slug: p.slug,
            name: p.name,
            brand: p.brand,
            description_html: p.description_html,
            short_description: p.short_description,
            images: images.into_iter().map(|(_, i)| i).collect(),
            options,
            variants: variant_views,
            parameters,
            gpsr: gpsr_view(ctx, p.gpsr),
        },
        breadcrumbs,
        delivery_estimate,
        reviews,
        seo,
        cache: CacheHints::public(60, vec![format!("product:{product_id}")]),
    }))
}

/// Drops `null` members (JSON-LD validators flag them).
fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_days_skip_weekends() {
        let fri = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap_or_default();
        assert_eq!(
            add_business_days(fri, 1),
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap_or_default()
        );
        assert_eq!(
            add_business_days(fri, 3),
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap_or_default()
        );
        let sat = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap_or_default();
        assert_eq!(
            add_business_days(sat, 1),
            NaiveDate::from_ymd_opt(2026, 9, 28).unwrap_or_default()
        );
    }

    #[test]
    fn schema_org_decimals() {
        assert_eq!(decimal(12900), "129.00");
        assert_eq!(decimal(5), "0.05");
        assert_eq!(decimal(-150), "-1.50");
    }

    #[test]
    fn nulls_are_stripped() {
        assert_eq!(
            strip_nulls(json!({"a": null, "b": [{"c": null, "d": 1}]})),
            json!({"b": [{"d": 1}]})
        );
    }
}
