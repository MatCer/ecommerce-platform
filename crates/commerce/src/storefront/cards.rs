//! Product cards (listings, home, recommendations, search) and the catalog loaders the page
//! models share: products with their translation for the request locale, priced variants with
//! stock state, and media.
//!
//! Only active products with at least one variant priced in the market's price list are sold
//! in a market; everything else is invisible to the storefront.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::Context;
use super::images::{self, Image};
use crate::media::AssetVariant;
use crate::money::MoneyView;
use crate::pricing::{self, ShelfPrice};

/// Products newer than this get the `new` badge.
const NEW_FOR_DAYS: i64 = 30;
/// Available units at or below which stock is "low".
pub const LOW_STOCK: i32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StockState {
    InStock,
    LowStock,
    /// Sold out, but orders are accepted (delivered later).
    Backorder,
    OutOfStock,
}

impl StockState {
    /// Same defaults as `inventory`: no level row means tracked with nothing on hand.
    pub fn from_level(
        on_hand: Option<i32>,
        reserved: Option<i32>,
        track: Option<bool>,
        backorder: Option<bool>,
    ) -> Self {
        if !track.unwrap_or(true) {
            return Self::InStock;
        }
        let available = on_hand.unwrap_or(0) - reserved.unwrap_or(0);
        if available > LOW_STOCK {
            Self::InStock
        } else if available > 0 {
            Self::LowStock
        } else if backorder.unwrap_or(false) {
            Self::Backorder
        } else {
            Self::OutOfStock
        }
    }

    pub fn purchasable(self) -> bool {
        self != Self::OutOfStock
    }
}

/// Price per unit of measure (EU price indication directive), e.g. per 1 kg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct UnitPrice {
    pub price: MoneyView,
    /// `kg`, `l`, `m`, `m²` or `ks`.
    pub unit: String,
}

/// A selling price with its Omnibus reference (A18). `reference_price` and
/// `discount_percent` are present only when a reduction may be claimed; they are computed
/// against the lowest price of the 30 days before the reduction, never `compare_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PriceView {
    pub price: MoneyView,
    pub reference_price: Option<MoneyView>,
    pub discount_percent: Option<u32>,
    pub unit_price: Option<UnitPrice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Badge {
    Sale,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ProductCard {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub brand: Option<String>,
    /// Up to two images: the main one and a second (hover) image.
    pub images: Vec<Image>,
    #[serde(flatten)]
    pub price: PriceView,
    /// Variants differ in price: show the price as "from".
    pub price_varies: bool,
    /// The best state over the product's variants.
    pub stock: StockState,
    pub badges: Vec<Badge>,
}

// ---------------------------------------------------------------------------------------
// Loaders

pub(crate) struct ProductRow {
    pub id: Uuid,
    pub brand: Option<String>,
    pub created_at: DateTime<Utc>,
    pub unit_measure: Option<String>,
    pub unit_quantity: Option<f64>,
    pub name: String,
    pub slug: String,
}

/// Active products among `ids` with the translation for the request locale (falling back to
/// the market's default locale, then any).
pub(crate) async fn products(
    tx: &mut TenantTx,
    ctx: &Context,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, ProductRow>, Error> {
    Ok(sqlx::query_as!(
        ProductRow,
        r#"SELECT p.id, p.brand, p.created_at, p.unit_measure,
                  p.unit_quantity::float8 AS unit_quantity, t.name AS "name!", t.slug AS "slug!"
           FROM products p
           CROSS JOIN LATERAL (
               SELECT name, slug FROM product_translations pt WHERE pt.product_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           WHERE p.id = ANY($1) AND p.status = 'active'"#,
        ids,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, r))
    .collect())
}

#[derive(Debug, Clone)]
pub(crate) struct VariantData {
    pub id: Uuid,
    pub product_id: Uuid,
    pub sku: String,
    pub ean: Option<String>,
    pub option_values: BTreeMap<String, String>,
    pub is_default: bool,
    pub stock: StockState,
    pub price: ShelfPrice,
}

/// The variants of `product_ids` that are priced in the market, in position order.
pub(crate) async fn priced_variants(
    tx: &mut TenantTx,
    ctx: &Context,
    product_ids: &[Uuid],
) -> Result<Vec<VariantData>, Error> {
    let Some(list) = ctx.market.price_list_id else {
        return Ok(Vec::new());
    };
    let rows = sqlx::query!(
        r#"SELECT v.id, v.product_id, v.sku, v.ean, v.option_values, v.is_default,
                  l.on_hand AS "on_hand?", l.reserved AS "reserved?", l.track AS "track?",
                  l.allow_backorder AS "allow_backorder?"
           FROM variants v LEFT JOIN inventory_levels l ON l.variant_id = v.id
           WHERE v.product_id = ANY($1)
           ORDER BY v.product_id, v.position"#,
        product_ids
    )
    .fetch_all(&mut **tx)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut prices = pricing::shelf_prices(tx, list, ctx.market.currency, &ids, ctx.now).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let price = prices.remove(&r.id)?;
            Some(VariantData {
                id: r.id,
                product_id: r.product_id,
                sku: r.sku,
                ean: r.ean,
                option_values: serde_json::from_value(r.option_values).unwrap_or_default(),
                is_default: r.is_default,
                stock: StockState::from_level(r.on_hand, r.reserved, r.track, r.allow_backorder),
                price,
            })
        })
        .collect())
}

pub(crate) struct MediaRow {
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
    pub alt_i18n: Value,
    pub variants: Value,
}

/// Ready images of the products, in display order. `per_product` caps the count.
pub(crate) async fn media(
    tx: &mut TenantTx,
    product_ids: &[Uuid],
    per_product: Option<i64>,
) -> Result<Vec<MediaRow>, Error> {
    Ok(sqlx::query_as!(
        MediaRow,
        r#"SELECT product_id AS "product_id!", variant_id, alt_i18n AS "alt_i18n!",
                  variants AS "variants!"
           FROM (
               SELECT pm.product_id, pm.variant_id, pm.alt_i18n, a.variants, pm.position,
                      row_number() OVER (PARTITION BY pm.product_id ORDER BY pm.position) AS n
               FROM product_media pm JOIN assets a ON a.id = pm.asset_id
               WHERE pm.product_id = ANY($1) AND a.status = 'ready'
           ) m
           WHERE $2::bigint IS NULL OR n <= $2
           ORDER BY product_id, position"#,
        product_ids,
        per_product
    )
    .fetch_all(&mut **tx)
    .await?)
}

pub(crate) fn image(ctx: &Context, row: &MediaRow, fallback_alt: &str) -> Option<Image> {
    let variants: Vec<AssetVariant> = serde_json::from_value(row.variants.clone()).ok()?;
    let alt = ctx
        .text(&row.alt_i18n)
        .unwrap_or_else(|| fallback_alt.to_owned());
    images::from_variants(&variants, alt)
}

// ---------------------------------------------------------------------------------------
// Prices

fn unit_label(measure: &str) -> &'static str {
    match measure {
        "kg" => "kg",
        "l" => "l",
        "m" => "m",
        "m2" => "m²",
        _ => "ks",
    }
}

pub(crate) fn price_view(ctx: &Context, product: &ProductRow, price: &ShelfPrice) -> PriceView {
    let o = &price.omnibus;
    let claim = o.claim && o.reference_minor.is_some();
    let unit_price = match (&product.unit_measure, product.unit_quantity) {
        (Some(m), Some(q)) if q > 0.0 => {
            // f64 is exact enough here: display only, rounded to whole minor units.
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let per_unit = (price.amount_minor as f64 / q).round() as i64;
            Some(UnitPrice {
                price: ctx.money(per_unit),
                unit: unit_label(m).to_owned(),
            })
        }
        _ => None,
    };
    PriceView {
        price: ctx.money(price.amount_minor),
        reference_price: o.reference_minor.filter(|_| claim).map(|r| ctx.money(r)),
        discount_percent: o.discount_percent.filter(|_| claim),
        unit_price,
    }
}

// ---------------------------------------------------------------------------------------
// Cards

/// Cards for `ids` in the given order; products not sold in the market are left out.
pub async fn cards(
    tx: &mut TenantTx,
    ctx: &Context,
    ids: &[Uuid],
) -> Result<Vec<ProductCard>, Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let products = products(tx, ctx, ids).await?;
    let mut variants: HashMap<Uuid, Vec<VariantData>> = HashMap::new();
    for v in priced_variants(tx, ctx, ids).await? {
        variants.entry(v.product_id).or_default().push(v);
    }
    let mut media_by: HashMap<Uuid, Vec<MediaRow>> = HashMap::new();
    for m in media(tx, ids, Some(2)).await? {
        media_by.entry(m.product_id).or_default().push(m);
    }
    let new_since = ctx.now - Duration::days(NEW_FOR_DAYS);
    Ok(ids
        .iter()
        .filter_map(|id| {
            let p = products.get(id)?;
            let vs = variants.get(id)?;
            let cheapest = vs.iter().min_by_key(|v| v.price.amount_minor)?;
            let most = vs.iter().map(|v| v.price.amount_minor).max()?;
            let price = price_view(ctx, p, &cheapest.price);
            let mut badges = Vec::new();
            if vs.iter().any(|v| v.price.omnibus.claim) {
                badges.push(Badge::Sale);
            }
            if p.created_at > new_since {
                badges.push(Badge::New);
            }
            Some(ProductCard {
                id: p.id,
                slug: p.slug.clone(),
                name: p.name.clone(),
                brand: p.brand.clone(),
                images: media_by
                    .get(id)
                    .map(|ms| ms.iter().filter_map(|m| image(ctx, m, &p.name)).collect())
                    .unwrap_or_default(),
                price_varies: most != cheapest.price.amount_minor,
                price,
                stock: vs
                    .iter()
                    .map(|v| v.stock)
                    .min()
                    .unwrap_or(StockState::OutOfStock),
                badges,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_states() {
        use StockState::*;
        assert_eq!(StockState::from_level(None, None, None, None), OutOfStock);
        assert_eq!(
            StockState::from_level(Some(0), Some(0), Some(false), None),
            InStock
        );
        assert_eq!(
            StockState::from_level(Some(10), Some(2), Some(true), None),
            InStock
        );
        assert_eq!(
            StockState::from_level(Some(6), Some(1), Some(true), None),
            LowStock
        );
        assert_eq!(
            StockState::from_level(Some(2), Some(2), Some(true), Some(true)),
            Backorder
        );
        assert_eq!(
            StockState::from_level(Some(2), Some(2), Some(true), Some(false)),
            OutOfStock
        );
        // The best state over variants is the minimum.
        assert_eq!(
            [OutOfStock, LowStock, Backorder].into_iter().min(),
            Some(LowStock)
        );
        assert!(!OutOfStock.purchasable() && Backorder.purchasable());
    }
}
