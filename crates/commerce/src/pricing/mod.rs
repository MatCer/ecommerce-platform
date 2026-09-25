//! Pricing (spec §6, §7.2, §10.1): price lists, gross variant prices, the effective-price
//! timeline with Omnibus references, and the cart pricing engine.

pub mod cart;
pub mod intervals;
pub mod omnibus;

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

pub use cart::{PricedCart, price_cart};
pub use intervals::{Cause, EffectivePrice, Interval, effective_prices};
pub use omnibus::Omnibus;

use crate::audit;
use crate::markets::{all_bytes, invalid};
use crate::money::Currency;
use intervals::Scope;
use omnibus::{CouponEffect, CouponWindow, UnitDiscount};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PriceList {
    pub id: Uuid,
    #[schema(example = "cz-retail")]
    pub code: String,
    pub name: String,
    pub currency: Currency,
    /// Markets selling from this list.
    pub market_ids: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPriceList {
    /// Lowercase identifier, unique per tenant.
    #[schema(example = "cz-retail")]
    pub code: String,
    #[schema(example = "Česko – maloobchod")]
    pub name: String,
    /// Fixed after creation. Prices are gross (VAT included).
    pub currency: Currency,
    /// Markets that sell from this list (their currency must match).
    #[serde(default)]
    pub market_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceListUpdate {
    pub name: String,
    /// The complete set of markets using this list; markets removed from it lose their list.
    pub market_ids: Vec<Uuid>,
}

fn check_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() || name.chars().count() > 200 {
        return Err(invalid("invalid_name", "name must be 1-200 characters"));
    }
    Ok(())
}

impl NewPriceList {
    pub fn validate(&self) -> Result<(), Error> {
        let code_ok = all_bytes(&self.code, 1..=32, |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
        }) && !self.code.starts_with('-');
        if !code_ok {
            return Err(invalid(
                "invalid_code",
                "code must be 1-32 lowercase letters, digits or hyphens",
            ));
        }
        check_name(&self.name)?;
        check_market_ids(&self.market_ids)
    }
}

fn check_market_ids(ids: &[Uuid]) -> Result<(), Error> {
    if ids.len() > 50 {
        return Err(invalid("invalid_market_ids", "at most 50 markets"));
    }
    Ok(())
}

async fn load_list(tx: &mut TenantTx, id: Uuid) -> Result<PriceList, Error> {
    let r = sqlx::query!(
        r#"SELECT id, code, name, currency, created_at, updated_at,
                  ARRAY(SELECT m.id FROM markets m WHERE m.price_list_id = pl.id ORDER BY m.code)
                      AS "market_ids!"
           FROM price_lists pl WHERE id = $1"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(PriceList {
        id: r.id,
        code: r.code,
        name: r.name,
        currency: intervals_currency(&r.currency)?,
        market_ids: r.market_ids,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

fn intervals_currency(code: &str) -> Result<Currency, Error> {
    Currency::parse(code).ok_or_else(|| Error::Internal(format!("unknown currency {code}")))
}

pub async fn list_price_lists(tx: &mut TenantTx) -> Result<Vec<PriceList>, Error> {
    let ids = sqlx::query_scalar!("SELECT id FROM price_lists ORDER BY code")
        .fetch_all(&mut **tx)
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(load_list(tx, id).await?);
    }
    Ok(out)
}

pub async fn get_price_list(tx: &mut TenantTx, id: Uuid) -> Result<PriceList, Error> {
    load_list(tx, id).await
}

/// Points exactly `market_ids` at the list (after checking their currency).
async fn attach_markets(
    tx: &mut TenantTx,
    list_id: Uuid,
    currency: Currency,
    market_ids: &[Uuid],
) -> Result<(), Error> {
    let found = sqlx::query!(
        "SELECT id, currency FROM markets WHERE id = ANY($1)",
        market_ids
    )
    .fetch_all(&mut **tx)
    .await?;
    let unique: BTreeSet<&Uuid> = market_ids.iter().collect();
    if found.len() != unique.len() {
        return Err(invalid("unknown_market", "a market does not exist"));
    }
    if found.iter().any(|m| m.currency != currency.code()) {
        return Err(invalid(
            "currency_mismatch",
            "a market's currency differs from the price list currency",
        ));
    }
    sqlx::query!(
        "UPDATE markets SET price_list_id = NULL WHERE price_list_id = $1 AND NOT (id = ANY($2))",
        list_id,
        market_ids
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE markets SET price_list_id = $1 WHERE id = ANY($2)",
        list_id,
        market_ids
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn create_price_list(
    tx: &mut TenantTx,
    actor: &str,
    input: &NewPriceList,
) -> Result<PriceList, Error> {
    input.validate()?;
    let tenant_id = tx.tenant_id();
    let id = sqlx::query_scalar!(
        "INSERT INTO price_lists (id, tenant_id, code, name, currency) VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
        crate::id::new_id(),
        tenant_id,
        input.code,
        input.name.trim(),
        input.currency.code()
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| {
        if crate::unique_violation(&e) {
            Error::Conflict {
                code: "already_exists",
                detail: format!("price list {} already exists", input.code),
            }
        } else {
            e.into()
        }
    })?;
    attach_markets(tx, id, input.currency, &input.market_ids).await?;
    let list = load_list(tx, id).await?;
    audit::record(
        tx,
        actor,
        "price_list.created",
        "price_list",
        Some(&id.to_string()),
        &json!({ "after": list }),
    )
    .await?;
    platform::queue::publish(
        &mut **tx,
        "price_list.created",
        &json!({ "price_list_id": id }),
    )
    .await?;
    Ok(list)
}

pub async fn update_price_list(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &PriceListUpdate,
) -> Result<PriceList, Error> {
    check_name(&input.name)?;
    check_market_ids(&input.market_ids)?;
    let before = load_list(tx, id).await?;
    sqlx::query!(
        "UPDATE price_lists SET name = $2, updated_at = now() WHERE id = $1",
        id,
        input.name.trim()
    )
    .execute(&mut **tx)
    .await?;
    attach_markets(tx, id, before.currency, &input.market_ids).await?;
    let after = load_list(tx, id).await?;
    audit::record(
        tx,
        actor,
        "price_list.updated",
        "price_list",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    platform::queue::publish(
        &mut **tx,
        "price_list.updated",
        &json!({ "price_list_id": id }),
    )
    .await?;
    Ok(after)
}

// ---------------------------------------------------------------------------------------
// Variant prices

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VariantPrice {
    pub variant_id: Uuid,
    /// Gross base price in minor units.
    pub amount_minor: i64,
    /// Display-only "was"/recommended price, never a reduction basis (A18).
    pub compare_at_minor: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceItem {
    pub variant_id: Uuid,
    pub amount_minor: i64,
    #[serde(default)]
    pub compare_at_minor: Option<i64>,
}

/// Why the base prices change (recorded on the price intervals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum PriceChangeReason {
    #[default]
    Base,
    /// Repricing because a VAT rate changed.
    Tax,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceUpsert {
    #[serde(default)]
    pub reason: PriceChangeReason,
    /// The prices come from an import: for new prices the history before now is unknown, so
    /// no reduction may be claimed for 30 days (A18).
    #[serde(default)]
    pub imported: bool,
    /// 1-1000 items, one per variant.
    pub items: Vec<PriceItem>,
}

pub const MAX_BULK: usize = 1000;

impl PriceUpsert {
    pub fn validate(&self) -> Result<(), Error> {
        if self.items.is_empty() || self.items.len() > MAX_BULK {
            return Err(invalid(
                "invalid_items",
                format!("items must have 1-{MAX_BULK} entries"),
            ));
        }
        let mut seen = BTreeSet::new();
        for i in &self.items {
            if !seen.insert(i.variant_id) {
                return Err(invalid(
                    "duplicate_variant",
                    format!("variant {} is listed twice", i.variant_id),
                ));
            }
            if !(0..=cart::MAX_AMOUNT).contains(&i.amount_minor) {
                return Err(invalid(
                    "invalid_amount",
                    format!("amount_minor must be 0-{}", cart::MAX_AMOUNT),
                ));
            }
            if i.compare_at_minor
                .is_some_and(|c| c <= i.amount_minor || c > cart::MAX_AMOUNT)
            {
                return Err(invalid(
                    "invalid_compare_at",
                    "compare_at_minor must be above amount_minor",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VariantPricePage {
    pub currency: Currency,
    pub items: Vec<VariantPrice>,
    /// Pass as `cursor` for the next page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

pub async fn list_prices(
    tx: &mut TenantTx,
    price_list_id: Uuid,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<VariantPricePage, Error> {
    if !(1..=MAX_BULK as i64).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 1000"));
    }
    let list = load_list(tx, price_list_id).await?;
    let mut items = sqlx::query_as!(
        VariantPrice,
        "SELECT variant_id, amount_minor, compare_at_minor, updated_at FROM variant_prices
         WHERE price_list_id = $1 AND ($2::uuid IS NULL OR variant_id > $2)
         ORDER BY variant_id LIMIT $3",
        price_list_id,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let limit = usize::try_from(limit).unwrap_or(MAX_BULK);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].variant_id);
    items.truncate(limit);
    Ok(VariantPricePage {
        currency: list.currency,
        items,
        next_cursor,
    })
}

/// Creates or replaces the base prices of up to 1000 variants and recomputes their price
/// timelines (publishing `price.changed` for effective changes). Audited.
pub async fn upsert_prices(
    tx: &mut TenantTx,
    actor: &str,
    price_list_id: Uuid,
    input: &PriceUpsert,
) -> Result<Vec<VariantPrice>, Error> {
    input.validate()?;
    load_list(tx, price_list_id).await?;
    intervals::lock(tx).await?;
    let tenant_id = tx.tenant_id();
    let ids: Vec<Uuid> = input.items.iter().map(|i| i.variant_id).collect();
    let known = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM variants WHERE id = ANY($1)",
        &ids
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).unwrap_or(0) != ids.len() {
        return Err(invalid("unknown_variant", "a variant does not exist"));
    }
    let before: HashMap<Uuid, (i64, Option<i64>)> = sqlx::query!(
        "SELECT variant_id, amount_minor, compare_at_minor FROM variant_prices
         WHERE price_list_id = $1 AND variant_id = ANY($2)",
        price_list_id,
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.variant_id, (r.amount_minor, r.compare_at_minor)))
    .collect();
    let amounts: Vec<i64> = input.items.iter().map(|i| i.amount_minor).collect();
    let compare: Vec<Option<i64>> = input.items.iter().map(|i| i.compare_at_minor).collect();
    let saved = sqlx::query_as!(
        VariantPrice,
        "INSERT INTO variant_prices (tenant_id, price_list_id, variant_id, amount_minor,
                                     compare_at_minor)
         SELECT $1, $2, v, a, c FROM UNNEST($3::uuid[], $4::bigint[], $5::bigint[]) AS u (v, a, c)
         ON CONFLICT (tenant_id, price_list_id, variant_id) DO UPDATE SET
             amount_minor = EXCLUDED.amount_minor,
             compare_at_minor = EXCLUDED.compare_at_minor,
             updated_at = now()
         RETURNING variant_id, amount_minor, compare_at_minor, updated_at",
        tenant_id,
        price_list_id,
        &ids,
        &amounts,
        &compare as &[Option<i64>]
    )
    .fetch_all(&mut **tx)
    .await?;
    let cause = match input.reason {
        PriceChangeReason::Base => Cause::Base,
        PriceChangeReason::Tax => Cause::Tax,
    };
    intervals::refresh(
        tx,
        &Scope::Variants {
            variant_ids: ids,
            price_list_id: Some(price_list_id),
        },
        cause,
        input.imported,
    )
    .await?;
    let changed: Vec<_> = input
        .items
        .iter()
        .filter(|i| before.get(&i.variant_id) != Some(&(i.amount_minor, i.compare_at_minor)))
        .map(|i| {
            json!({
                "variant_id": i.variant_id,
                "before": before.get(&i.variant_id).map(|b| json!({"amount_minor": b.0, "compare_at_minor": b.1})),
                "after": {"amount_minor": i.amount_minor, "compare_at_minor": i.compare_at_minor},
            })
        })
        .collect();
    audit::record(
        tx,
        actor,
        "variant_prices.upserted",
        "price_list",
        Some(&price_list_id.to_string()),
        &json!({ "reason": input.reason, "imported": input.imported, "changes": changed }),
    )
    .await?;
    Ok(saved)
}

/// Removes a variant's price from a list: it is no longer sold there from now on.
pub async fn delete_price(
    tx: &mut TenantTx,
    actor: &str,
    price_list_id: Uuid,
    variant_id: Uuid,
) -> Result<(), Error> {
    intervals::lock(tx).await?;
    let before = sqlx::query!(
        "DELETE FROM variant_prices WHERE price_list_id = $1 AND variant_id = $2
         RETURNING amount_minor, compare_at_minor",
        price_list_id,
        variant_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    intervals::refresh(
        tx,
        &Scope::Variants {
            variant_ids: vec![variant_id],
            price_list_id: Some(price_list_id),
        },
        Cause::Base,
        false,
    )
    .await?;
    audit::record(
        tx,
        actor,
        "variant_price.deleted",
        "price_list",
        Some(&price_list_id.to_string()),
        &json!({ "variant_id": variant_id, "before": {
            "amount_minor": before.amount_minor, "compare_at_minor": before.compare_at_minor } }),
    )
    .await?;
    Ok(())
}

/// Recomputes the timelines of a product's variants after a catalog change (variants or
/// category membership affect which sales apply).
pub async fn refresh_product(tx: &mut TenantTx, product_id: Uuid) -> Result<(), Error> {
    intervals::lock(tx).await?;
    let variant_ids =
        sqlx::query_scalar!("SELECT id FROM variants WHERE product_id = $1", product_id)
            .fetch_all(&mut **tx)
            .await?;
    intervals::refresh(
        tx,
        &Scope::Variants {
            variant_ids,
            price_list_id: None,
        },
        Cause::Base,
        false,
    )
    .await?;
    Ok(())
}

/// Recomputes all timelines when a live sale targets categories: a category move or delete
/// changes which products those sales reach.
pub async fn refresh_category_sales(tx: &mut TenantTx) -> Result<(), Error> {
    intervals::lock(tx).await?;
    let affected = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sales WHERE (ends_at IS NULL OR ends_at > now())
                  AND jsonb_array_length(coalesce(targets->'category_ids', '[]')) > 0) AS "x!""#
    )
    .fetch_one(&mut **tx)
    .await?;
    if affected {
        intervals::refresh(tx, &Scope::All, Cause::Base, false).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Price history (Admin API) and Omnibus references

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VariantPriceHistory {
    pub variant_id: Uuid,
    pub sku: String,
    pub price_list_id: Uuid,
    pub currency: Currency,
    /// The current base price entry (absent when the variant is not priced in this list).
    pub price: Option<VariantPrice>,
    /// The Omnibus figures right now.
    pub omnibus: Omnibus,
    /// The full effective-price timeline, oldest first, including scheduled future changes.
    pub intervals: Vec<Interval>,
}

/// Published coupons' windows for the Omnibus reference (fixed ones only in `currency`).
async fn coupon_windows(tx: &mut TenantTx, currency: Currency) -> Result<Vec<CouponWindow>, Error> {
    Ok(sqlx::query!(
        "SELECT kind, value, currency, min_subtotal_minor, coalesce(starts_at, created_at) AS \"from!\",
                ends_at
         FROM coupons WHERE published AND kind IN ('percent', 'fixed')"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter_map(|r| {
        let value = r.value?;
        // Amounts in another currency (fixed value, minimum subtotal) do not apply here.
        if r.currency.as_deref().is_some_and(|c| c != currency.code()) {
            return None;
        }
        let discount = if r.kind == "percent" {
            UnitDiscount::Percent {
                basis_points: u32::try_from(value).ok()?,
            }
        } else {
            UnitDiscount::Fixed {
                amount_minor: value,
            }
        };
        let effect = CouponEffect {
            discount,
            min_subtotal_minor: r.min_subtotal_minor,
        };
        Some(CouponWindow {
            from: r.from,
            to: r.ends_at,
            effect,
        })
    })
    .collect())
}

/// Price timelines and Omnibus references of a product's variants at `at`, per price list
/// (or only `price_list_id`).
pub async fn price_history(
    tx: &mut TenantTx,
    product_id: Uuid,
    price_list_id: Option<Uuid>,
    at: DateTime<Utc>,
) -> Result<Vec<VariantPriceHistory>, Error> {
    let exists = sqlx::query_scalar!("SELECT id FROM products WHERE id = $1", product_id)
        .fetch_optional(&mut **tx)
        .await?;
    if exists.is_none() {
        return Err(Error::NotFound);
    }
    let pairs = sqlx::query!(
        "SELECT v.id AS variant_id, v.sku, pl.id AS price_list_id, pl.currency
         FROM variants v CROSS JOIN price_lists pl
         WHERE v.product_id = $1 AND ($2::uuid IS NULL OR pl.id = $2)
           AND (EXISTS (SELECT 1 FROM variant_prices vp
                        WHERE vp.variant_id = v.id AND vp.price_list_id = pl.id)
                OR EXISTS (SELECT 1 FROM price_intervals pi
                           WHERE pi.variant_id = v.id AND pi.price_list_id = pl.id))
         ORDER BY v.position, pl.code",
        product_id,
        price_list_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut coupons: HashMap<Currency, Vec<CouponWindow>> = HashMap::new();
    let mut out = Vec::with_capacity(pairs.len());
    for p in pairs {
        let currency = intervals_currency(&p.currency)?;
        // ponytail: whole history per pair; bound it to the last N months if it grows large.
        let history: Vec<Interval> = sqlx::query!(
            "SELECT id, amount_minor, valid_from, valid_to, cause, sale_id, imported
             FROM price_intervals WHERE price_list_id = $1 AND variant_id = $2
             ORDER BY valid_from",
            p.price_list_id,
            p.variant_id
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|r| Interval {
            id: r.id,
            amount_minor: r.amount_minor,
            valid_from: r.valid_from,
            valid_to: r.valid_to,
            cause: Cause::parse(&r.cause),
            sale_id: r.sale_id,
            imported: r.imported,
        })
        .collect();
        let price = sqlx::query_as!(
            VariantPrice,
            "SELECT variant_id, amount_minor, compare_at_minor, updated_at FROM variant_prices
             WHERE price_list_id = $1 AND variant_id = $2",
            p.price_list_id,
            p.variant_id
        )
        .fetch_optional(&mut **tx)
        .await?;
        if let std::collections::hash_map::Entry::Vacant(e) = coupons.entry(currency) {
            e.insert(coupon_windows(tx, currency).await?);
        }
        let windows = coupons.get(&currency).map_or(&[][..], Vec::as_slice);
        out.push(VariantPriceHistory {
            variant_id: p.variant_id,
            sku: p.sku,
            price_list_id: p.price_list_id,
            currency,
            price,
            omnibus: omnibus::reference(&history, windows, at),
            intervals: history,
        });
    }
    Ok(out)
}
