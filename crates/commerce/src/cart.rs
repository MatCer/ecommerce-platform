//! Carts (spec §7.3, §10.3, A1, A4, A12, A15).
//!
//! A cart is reached only through a capability token (256-bit, hashed at rest):
//! - the **shop** capability lives in the shop origin's `cart` cookie and may read and edit;
//! - at checkout handoff it is revoked and a single-use, 60 s handoff token is minted;
//! - redeeming the handoff on the checkout origin mints the **checkout** capability (checkout
//!   mutations and order placement, `commerce::checkout`).
//!
//! Totals are recomputed on every read by `pricing::price_cart` from the effective prices in the
//! market's price list; the client never sends prices. VAT follows the tax profile for the
//! cart's ship-to country, which defaults to the market's first country until checkout sets
//! it (A3). `version` changes with every mutation (place-order checks it, A12).

use std::collections::{BTreeMap, HashMap};

use chrono::{Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::catalog::tax as tax_categories;
use crate::markets::invalid;
use crate::money::{Currency, MoneyView};
use crate::pricing::cart::{CartInput, LineInput};
use crate::pricing::price_cart;
use crate::promotions::coupons;
use crate::storefront::Context;
use crate::storefront::cards::{self, StockState};
use crate::storefront::images::Image;
use crate::tax::{self, TaxRate};

pub const MAX_LINES: i64 = 100;
pub const MAX_LINE_QUANTITY: i32 = 999;
/// Anonymous carts expire after 30 days without activity (§10.3).
pub const IDLE_DAYS: i64 = 30;
/// A1: handoff tokens are valid for 60 seconds.
pub const HANDOFF_SECS: i64 = 60;
const HANDOFF_SECS_F64: f64 = 60.0;

/// Which capability a request presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Shop,
    Checkout,
}

/// An open cart found by its capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CartRef {
    pub id: Uuid,
    pub market_id: Uuid,
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CartLineView {
    pub id: Uuid,
    pub variant_id: Uuid,
    pub product_id: Uuid,
    /// Product page slug (`/p/<slug>`).
    pub slug: String,
    pub product_name: String,
    /// Option values, e.g. `Zelená / M`.
    pub variant_label: String,
    pub sku: String,
    pub image: Option<Image>,
    pub quantity: u32,
    pub unit_price: MoneyView,
    /// This line's share of the coupon.
    pub discount: MoneyView,
    /// What the customer pays for the line (VAT included, after the coupon).
    pub total: MoneyView,
    /// VAT rate in percent (`"21"`).
    pub tax_rate: String,
    pub stock: StockState,
    /// False when the variant is no longer sold (not priced, archived or sold out); such
    /// lines are not part of the totals.
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CartCoupon {
    pub code: String,
    /// Whether it applies to the cart right now.
    pub applied: bool,
    /// Why not (`coupon_min_subtotal`, `coupon_not_active`, ...).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct VatRow {
    /// Percent (`"21"`).
    pub rate: String,
    pub net: MoneyView,
    pub vat: MoneyView,
    pub gross: MoneyView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CartView {
    pub id: Uuid,
    /// Changes with every modification.
    pub version: i32,
    pub currency: Currency,
    pub lines: Vec<CartLineView>,
    pub item_count: u32,
    pub coupon: Option<CartCoupon>,
    /// Goods before the coupon.
    pub subtotal: MoneyView,
    pub discount: MoneyView,
    /// Goods after the coupon, VAT included (shipping and payment arrive with checkout).
    pub total: MoneyView,
    pub vat: Vec<VatRow>,
    pub vat_total: MoneyView,
    /// Country whose VAT applies (checkout sets it; the market's first country until then).
    pub ship_to_country: String,
    /// What is missing to the market's lowest free-shipping threshold (zero once reached);
    /// `None` when no shipping method has a threshold.
    pub free_shipping_remaining: Option<MoneyView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewLine {
    pub variant_id: Uuid,
    /// 1-999 (added to an existing line of the same variant).
    #[serde(default = "one")]
    pub quantity: i32,
}

fn one() -> i32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LineUpdate {
    /// 0 removes the line.
    pub quantity: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CouponCode {
    pub code: String,
}

fn cart_not_found() -> Error {
    Error::NotFound
}

/// Creates an empty cart in the context's market and returns it with its shop capability.
pub async fn create(tx: &mut TenantTx, ctx: &Context) -> Result<(Uuid, String), Error> {
    let minted = capability::mint();
    let id = sqlx::query_scalar!(
        "INSERT INTO carts (tenant_id, market_id, shop_token_hash, locale, currency)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
        tx.tenant_id(),
        ctx.market.id,
        minted.hash,
        ctx.locale,
        ctx.market.currency.code()
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok((id, minted.token))
}

/// The open, non-expired cart of the context's market behind `token`, locked for the rest of
/// the transaction. `scope` `None` accepts either capability (reads).
pub async fn find(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    scope: Option<Scope>,
) -> Result<CartRef, Error> {
    if !capability::well_formed(token) {
        return Err(cart_not_found());
    }
    let hash = capability::hash(token);
    let row = sqlx::query!(
        "SELECT id, market_id, coalesce(shop_token_hash = $1, false) AS \"shop!\" FROM carts
         WHERE (shop_token_hash = $1 OR checkout_token_hash = $1)
           AND status = 'open' AND last_activity_at > $2
         FOR UPDATE",
        hash,
        Utc::now() - Duration::days(IDLE_DAYS)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(cart_not_found)?;
    // A cart belongs to one market: a capability presented on another market's host is not
    // valid there (prices, currency and VAT would all be wrong).
    if row.market_id != ctx.market.id {
        return Err(cart_not_found());
    }
    let found = if row.shop {
        Scope::Shop
    } else {
        Scope::Checkout
    };
    if scope.is_some_and(|s| s != found) {
        return Err(cart_not_found());
    }
    // Expiry counts from the last use (§10.3), reads included; the content version stays.
    sqlx::query!(
        "UPDATE carts SET last_activity_at = now() WHERE id = $1",
        row.id
    )
    .execute(&mut **tx)
    .await?;
    Ok(CartRef {
        id: row.id,
        market_id: row.market_id,
        scope: found,
    })
}

/// The products in the open shop cart behind `token`, for cross-sell (WP17). Read-only: no
/// lock and no activity bump, so a recommendation read never extends or blocks a cart. An
/// unknown, expired or other-market capability reads as an empty cart.
pub async fn product_ids(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<Vec<Uuid>, Error> {
    if !capability::well_formed(token) {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT v.product_id AS "product_id!"
           FROM carts c
           JOIN cart_lines l ON l.cart_id = c.id
           JOIN variants v ON v.id = l.variant_id
           WHERE c.shop_token_hash = $1 AND c.status = 'open' AND c.market_id = $2
             AND c.last_activity_at > $3"#,
        capability::hash(token),
        ctx.market.id,
        Utc::now() - Duration::days(IDLE_DAYS)
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// The cart behind a **checkout** capability for order placement, locked for the rest of the
/// transaction (A12), whether still open or already converted (an idempotent replay of
/// place-order must find it). Returns the cart and whether it is still open.
pub async fn find_for_order(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<(CartRef, bool), Error> {
    if !capability::well_formed(token) {
        return Err(cart_not_found());
    }
    let row = sqlx::query!(
        "SELECT id, market_id, status FROM carts
         WHERE checkout_token_hash = $1 AND status IN ('open', 'converted')
           AND last_activity_at > $2
         FOR UPDATE",
        capability::hash(token),
        Utc::now() - Duration::days(IDLE_DAYS)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(cart_not_found)?;
    if row.market_id != ctx.market.id {
        return Err(cart_not_found());
    }
    Ok((
        CartRef {
            id: row.id,
            market_id: row.market_id,
            scope: Scope::Checkout,
        },
        row.status == "open",
    ))
}

pub(crate) async fn touch(tx: &mut TenantTx, cart_id: Uuid) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE carts SET version = version + 1, last_activity_at = now(), updated_at = now()
         WHERE id = $1",
        cart_id
    )
    .execute(&mut **tx)
    .await?;
    platform::queue::publish(
        &mut **tx,
        "cart.changed",
        &serde_json::json!({"cart_id":cart_id}),
    )
    .await?;
    Ok(())
}

/// Stock available for sale, `None` when unlimited (untracked or backorder allowed).
async fn sellable(tx: &mut TenantTx, variant_id: Uuid) -> Result<Option<i32>, Error> {
    let l = sqlx::query!(
        "SELECT on_hand, reserved, track, allow_backorder FROM inventory_levels WHERE variant_id = $1",
        variant_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match l {
        Some(l) if !l.track || l.allow_backorder => None,
        Some(l) => Some((l.on_hand - l.reserved).max(0)),
        None => Some(0),
    })
}

fn check_quantity(q: i32, allow_zero: bool) -> Result<(), Error> {
    let min = i32::from(!allow_zero);
    if (min..=MAX_LINE_QUANTITY).contains(&q) {
        Ok(())
    } else {
        Err(invalid(
            "invalid_quantity",
            format!("quantity must be {min}-{MAX_LINE_QUANTITY}"),
        ))
    }
}

async fn ensure_stock(tx: &mut TenantTx, variant_id: Uuid, quantity: i32) -> Result<(), Error> {
    if let Some(available) = sellable(tx, variant_id).await?
        && quantity > available
    {
        return Err(Error::Conflict {
            code: if available == 0 {
                "out_of_stock"
            } else {
                "insufficient_stock"
            },
            detail: format!("only {available} in stock"),
        });
    }
    Ok(())
}

/// Adds `quantity` of a variant sold in the market (merged into an existing line).
pub async fn add_line(
    tx: &mut TenantTx,
    ctx: &Context,
    cart: &CartRef,
    line: &NewLine,
) -> Result<(), Error> {
    check_quantity(line.quantity, false)?;
    let product_id = sqlx::query_scalar!(
        "SELECT v.product_id FROM variants v JOIN products p ON p.id = v.product_id
         WHERE v.id = $1 AND p.status = 'active'",
        line.variant_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| invalid("unknown_variant", "this variant is not for sale"))?;
    let priced = cards::priced_variants(tx, ctx, &[product_id])
        .await?
        .into_iter()
        .any(|v| v.id == line.variant_id);
    if !priced {
        return Err(invalid("unknown_variant", "this variant is not for sale"));
    }
    let existing = sqlx::query_scalar!(
        "SELECT quantity FROM cart_lines WHERE cart_id = $1 AND variant_id = $2",
        cart.id,
        line.variant_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    if existing.is_none() {
        let count = sqlx::query_scalar!(
            "SELECT count(*) AS \"n!\" FROM cart_lines WHERE cart_id = $1",
            cart.id
        )
        .fetch_one(&mut **tx)
        .await?;
        if count >= MAX_LINES {
            return Err(invalid(
                "too_many_lines",
                format!("at most {MAX_LINES} lines"),
            ));
        }
    }
    let quantity = (existing.unwrap_or(0) + line.quantity).min(MAX_LINE_QUANTITY);
    ensure_stock(tx, line.variant_id, quantity).await?;
    sqlx::query!(
        "INSERT INTO cart_lines (tenant_id, cart_id, variant_id, quantity) VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, cart_id, variant_id)
         DO UPDATE SET quantity = EXCLUDED.quantity, updated_at = now()",
        tx.tenant_id(),
        cart.id,
        line.variant_id,
        quantity
    )
    .execute(&mut **tx)
    .await?;
    touch(tx, cart.id).await
}

/// Sets a line's quantity; 0 removes it.
pub async fn update_line(
    tx: &mut TenantTx,
    cart: &CartRef,
    line_id: Uuid,
    update: &LineUpdate,
) -> Result<(), Error> {
    check_quantity(update.quantity, true)?;
    if update.quantity == 0 {
        return remove_line(tx, cart, line_id).await;
    }
    let variant_id = sqlx::query_scalar!(
        "SELECT variant_id FROM cart_lines WHERE id = $1 AND cart_id = $2",
        line_id,
        cart.id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(unknown_line)?;
    ensure_stock(tx, variant_id, update.quantity).await?;
    sqlx::query!(
        "UPDATE cart_lines SET quantity = $3, updated_at = now() WHERE id = $1 AND cart_id = $2",
        line_id,
        cart.id,
        update.quantity
    )
    .execute(&mut **tx)
    .await?;
    touch(tx, cart.id).await
}

pub async fn remove_line(tx: &mut TenantTx, cart: &CartRef, line_id: Uuid) -> Result<(), Error> {
    let removed = sqlx::query!(
        "DELETE FROM cart_lines WHERE id = $1 AND cart_id = $2",
        line_id,
        cart.id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(unknown_line());
    }
    touch(tx, cart.id).await
}

/// 404 means "no such cart" to the edge (it then drops the capability cookie), so errors about
/// a cart's content are 422s.
fn unknown_line() -> Error {
    invalid("unknown_line", "the cart has no such line")
}

/// Applies a coupon (replacing any other: one coupon per cart). `422` with the coupon's reason
/// when it does not apply to the cart as it is.
pub async fn apply_coupon(
    tx: &mut TenantTx,
    ctx: &Context,
    cart: &CartRef,
    code: &str,
) -> Result<(), Error> {
    let coupon = coupons::find_by_code(tx, code)
        .await?
        .ok_or_else(|| invalid("coupon_not_found", "this code does not exist"))?;
    let priced = price(tx, ctx, cart.id).await?;
    coupons::evaluate(
        &coupon,
        ctx.market.currency,
        priced.goods_before_coupon,
        0,
        Utc::now(),
    )?;
    sqlx::query!(
        "INSERT INTO cart_coupons (tenant_id, cart_id, coupon_id) VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, cart_id) DO UPDATE SET coupon_id = EXCLUDED.coupon_id,
             created_at = now()",
        tx.tenant_id(),
        cart.id,
        coupon.id
    )
    .execute(&mut **tx)
    .await?;
    touch(tx, cart.id).await
}

pub async fn remove_coupon(tx: &mut TenantTx, cart: &CartRef, code: &str) -> Result<(), Error> {
    let removed = sqlx::query!(
        "DELETE FROM cart_coupons cc USING coupons c
         WHERE cc.cart_id = $1 AND c.id = cc.coupon_id AND c.code = $2",
        cart.id,
        coupons::normalize_code(code)
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(invalid(
            "coupon_not_applied",
            "this coupon is not applied to the cart",
        ));
    }
    touch(tx, cart.id).await
}

// ---------------------------------------------------------------------------------------
// Checkout handoff (A1, A4)

/// Starts the handoff: revokes the shop capability and mints a single-use handoff token for
/// the checkout origin. Only a shop capability of a non-empty cart can do this.
pub async fn start_handoff(tx: &mut TenantTx, cart: &CartRef) -> Result<String, Error> {
    if cart.scope != Scope::Shop {
        return Err(cart_not_found());
    }
    let lines = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM cart_lines WHERE cart_id = $1",
        cart.id
    )
    .fetch_one(&mut **tx)
    .await?;
    if lines == 0 {
        return Err(Error::Conflict {
            code: "cart_empty",
            detail: "the cart is empty".into(),
        });
    }
    sqlx::query!("DELETE FROM checkout_handoffs WHERE expires_at < now() - interval '1 day'")
        .execute(&mut **tx)
        .await?;
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO checkout_handoffs (token_hash, tenant_id, cart_id, market_id, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
        minted.hash,
        tx.tenant_id(),
        cart.id,
        cart.market_id,
        HANDOFF_SECS_F64
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE carts SET shop_token_hash = NULL, updated_at = now() WHERE id = $1",
        cart.id
    )
    .execute(&mut **tx)
    .await?;
    Ok(minted.token)
}

/// Redeems a handoff token for the market it was minted in: consumed atomically, and a new
/// checkout capability replaces any earlier one. `None` for unknown, used or expired tokens.
pub async fn redeem_handoff(
    tx: &mut TenantTx,
    market_id: Uuid,
    token: &str,
) -> Result<Option<String>, Error> {
    if !capability::well_formed(token) {
        return Ok(None);
    }
    let Some(cart_id) = sqlx::query_scalar!(
        "UPDATE checkout_handoffs SET used_at = now()
         WHERE token_hash = $1 AND market_id = $2 AND used_at IS NULL AND expires_at > now()
         RETURNING cart_id",
        capability::hash(token),
        market_id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let minted = capability::mint();
    let updated = sqlx::query!(
        "UPDATE carts SET checkout_token_hash = $2, last_activity_at = now(), updated_at = now()
         WHERE id = $1 AND status = 'open'",
        cart_id,
        minted.hash
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok((updated == 1).then_some(minted.token))
}

/// Sign-in on the checkout origin (A4): the cart being checked out is attached to the
/// customer, and the customer's other open carts of the same market are merged into it (lines
/// by variant, quantities added up to the per-line cap) and closed as `merged`.
pub async fn attach_to_customer(
    tx: &mut TenantTx,
    cart: &CartRef,
    customer_id: Uuid,
) -> Result<(), Error> {
    let others = sqlx::query_scalar!(
        "SELECT id FROM carts
         WHERE customer_id = $1 AND market_id = $2 AND status = 'open' AND id <> $3
         ORDER BY last_activity_at
         FOR UPDATE",
        customer_id,
        cart.market_id,
        cart.id
    )
    .fetch_all(&mut **tx)
    .await?;
    for other in &others {
        // Existing variants add up; new ones join while the cart has room (MAX_LINES).
        sqlx::query!(
            "UPDATE cart_lines l SET quantity = least(l.quantity + o.quantity, $3), updated_at = now()
             FROM cart_lines o
             WHERE l.cart_id = $1 AND o.cart_id = $2 AND o.variant_id = l.variant_id",
            cart.id,
            other,
            MAX_LINE_QUANTITY
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!(
            "INSERT INTO cart_lines (tenant_id, cart_id, variant_id, quantity)
             SELECT o.tenant_id, $1, o.variant_id, o.quantity FROM cart_lines o
             WHERE o.cart_id = $2
               AND NOT EXISTS (SELECT 1 FROM cart_lines l WHERE l.cart_id = $1 AND l.variant_id = o.variant_id)
             ORDER BY o.created_at
             LIMIT greatest($3 - (SELECT count(*) FROM cart_lines WHERE cart_id = $1), 0)",
            cart.id,
            other,
            MAX_LINES
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!(
            "UPDATE carts SET status = 'merged', shop_token_hash = NULL, checkout_token_hash = NULL,
                 updated_at = now()
             WHERE id = $1",
            other
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE carts SET customer_id = $2 WHERE id = $1",
        cart.id,
        customer_id
    )
    .execute(&mut **tx)
    .await?;
    if !others.is_empty() {
        touch(tx, cart.id).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Pricing and the view

/// A priced cart: the view plus what checkout and order placement build on.
pub(crate) struct Priced {
    pub view: CartView,
    pub goods_before_coupon: i64,
    /// The goods-only pricing input (checkout adds shipping and the payment fee).
    pub input: CartInput,
    /// Goods after the coupon.
    pub goods_minor: i64,
    /// Order-line data of every cart line, in cart order.
    pub lines: Vec<LineMeta>,
    /// The attached coupon: id and whether it applies (`Err(code)` when it does not).
    pub coupon: Option<(Uuid, Result<(), &'static str>)>,
    /// Total weight of the available lines in grams (unknown weights count as 0).
    pub weight_g: i64,
    pub vat_payer: bool,
}

/// A cart line as it becomes an order line.
pub(crate) struct LineMeta {
    pub id: Uuid,
    pub variant_id: Uuid,
    pub product_id: Uuid,
    pub sku: String,
    pub name: String,
    pub label: String,
    pub quantity: u32,
    pub available: bool,
}

/// The liable country's VAT for each product (A3), `ZERO` for a non-VAT-payer, plus the
/// liable country's standard rate for ancillary fees.
async fn rates(
    tx: &mut TenantTx,
    product_ids: &[Uuid],
    ship_to: &str,
) -> Result<(bool, HashMap<Uuid, TaxRate>, TaxRate), Error> {
    let profile = tax::require(tx).await?;
    let today = Utc::now().date_naive();
    let Some(country) = tax::liable_country(&profile, ship_to)?.map(str::to_owned) else {
        return Ok((false, HashMap::new(), TaxRate::ZERO));
    };
    let parse = |c: tax_categories::TaxCategory| {
        c.rate
            .parse::<TaxRate>()
            .map_err(|()| Error::Internal(format!("bad stored rate {}", c.rate)))
    };
    let mut out = HashMap::new();
    let mut categories = tax_categories::product_rates(tx, product_ids, &country, today).await?;
    for id in product_ids {
        let cat = categories
            .remove(id)
            .ok_or_else(|| invalid("no_tax_rate", format!("no VAT rate is known for {country}")))?;
        out.insert(*id, parse(cat)?);
    }
    let standard = tax_categories::rate(tx, &country, "standard", today)
        .await?
        .ok_or_else(|| invalid("no_tax_rate", format!("no VAT rate is known for {country}")))?;
    Ok((true, out, parse(standard)?))
}

async fn price(tx: &mut TenantTx, ctx: &Context, cart_id: Uuid) -> Result<Priced, Error> {
    let cart = sqlx::query!(
        "SELECT version, ship_to_country FROM carts WHERE id = $1",
        cart_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let lines = sqlx::query!(
        "SELECT cl.id, cl.variant_id, cl.quantity, v.product_id, v.sku, v.option_values, v.weight_g
         FROM cart_lines cl JOIN variants v ON v.id = cl.variant_id
         WHERE cl.cart_id = $1 ORDER BY cl.created_at, cl.id",
        cart_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let product_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = lines.iter().map(|l| l.product_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let products = cards::products(tx, ctx, &product_ids).await?;
    let variants: HashMap<Uuid, cards::VariantData> = cards::priced_variants(tx, ctx, &product_ids)
        .await?
        .into_iter()
        .map(|v| (v.id, v))
        .collect();
    // Option value labels: (product, option, value) -> name.
    let mut labels: HashMap<(Uuid, String, String), String> = HashMap::new();
    let mut option_order: HashMap<Uuid, Vec<String>> = HashMap::new();
    for o in sqlx::query!(
        r#"SELECT product_id, code, "values" AS "values!" FROM product_options
           WHERE product_id = ANY($1) ORDER BY product_id, position"#,
        &product_ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        option_order
            .entry(o.product_id)
            .or_default()
            .push(o.code.clone());
        for v in o.values.as_array().into_iter().flatten() {
            if let (Some(code), Some(name)) = (
                v.get("code").and_then(|c| c.as_str()),
                v.get("name_i18n").and_then(|n| ctx.text(n)),
            ) {
                labels.insert((o.product_id, o.code.clone(), code.to_owned()), name);
            }
        }
    }
    let mut images: HashMap<Uuid, Vec<(Option<Uuid>, Image)>> = HashMap::new();
    for m in cards::media(tx, &product_ids, None).await? {
        if let Some(img) = cards::image(ctx, &m, "") {
            images
                .entry(m.product_id)
                .or_default()
                .push((m.variant_id, img));
        }
    }

    let ship_to = cart
        .ship_to_country
        .clone()
        .or_else(|| ctx.market.country_codes.first().cloned())
        .unwrap_or_default();
    let (vat_payer, product_rates, fallback_rate) = if product_ids.is_empty() {
        (false, HashMap::new(), TaxRate::ZERO)
    } else {
        rates(tx, &product_ids, &ship_to).await?
    };

    struct Line {
        id: Uuid,
        variant_id: Uuid,
        product_id: Uuid,
        sku: String,
        label: String,
        quantity: u32,
        unit: Option<i64>,
        stock: StockState,
        rate: TaxRate,
        weight_g: i64,
    }
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|l| {
            let v = variants.get(&l.variant_id);
            let options: BTreeMap<String, String> =
                serde_json::from_value(l.option_values).unwrap_or_default();
            let label = option_order
                .get(&l.product_id)
                .into_iter()
                .flatten()
                .filter_map(|code| {
                    let value = options.get(code)?;
                    Some(
                        labels
                            .get(&(l.product_id, code.clone(), value.clone()))
                            .cloned()
                            .unwrap_or_else(|| value.clone()),
                    )
                })
                .collect::<Vec<_>>()
                .join(" / ");
            let available =
                products.contains_key(&l.product_id) && v.is_some_and(|v| v.stock.purchasable());
            Line {
                id: l.id,
                variant_id: l.variant_id,
                product_id: l.product_id,
                sku: l.sku,
                label,
                quantity: u32::try_from(l.quantity).unwrap_or(1),
                unit: v.filter(|_| available).map(|v| v.price.amount_minor),
                stock: v.map_or(StockState::OutOfStock, |v| v.stock),
                rate: product_rates
                    .get(&l.product_id)
                    .copied()
                    .unwrap_or(TaxRate::ZERO),
                weight_g: i64::from(l.weight_g.unwrap_or(0)),
            }
        })
        .collect();

    let goods_before_coupon: i64 = lines
        .iter()
        .filter_map(|l| l.unit.map(|u| u * i64::from(l.quantity)))
        .sum();
    let coupon_row = sqlx::query_scalar!(
        "SELECT coupon_id FROM cart_coupons WHERE cart_id = $1",
        cart_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let (applied, coupon_view, coupon_state) = match coupon_row {
        None => (None, None, None),
        Some(id) => {
            let c = coupons::get(tx, id).await?;
            match coupons::evaluate(&c, ctx.market.currency, goods_before_coupon, 0, Utc::now()) {
                Ok(a) => (
                    Some(a),
                    Some(CartCoupon {
                        code: c.code,
                        applied: true,
                        reason: None,
                    }),
                    Some((id, Ok(()))),
                ),
                Err(e) => (
                    None,
                    Some(CartCoupon {
                        code: c.code,
                        applied: false,
                        reason: Some(e.code().to_owned()),
                    }),
                    Some((id, Err(e.code()))),
                ),
            }
        }
    };

    let input = CartInput {
        currency: ctx.market.currency,
        vat_payer,
        lines: lines
            .iter()
            .filter_map(|l| {
                Some(LineInput {
                    id: l.id,
                    quantity: l.quantity,
                    unit_price_minor: l.unit?,
                    tax_rate: l.rate,
                })
            })
            .collect(),
        coupon: applied,
        shipping_minor: None,
        payment_fee_minor: None,
        fallback_rate,
        cash_rounding: None,
    };
    let priced = price_cart(&input)?;
    let by_line: HashMap<Uuid, &crate::pricing::cart::PricedLine> =
        priced.lines.iter().map(|l| (l.id, l)).collect();

    let names: HashMap<Uuid, (String, String)> = products
        .iter()
        .map(|(id, p)| (*id, (p.name.clone(), p.slug.clone())))
        .collect();
    let view_lines = lines
        .iter()
        .map(|l| {
            let pl = by_line.get(&l.id);
            let (name, slug) = names.get(&l.product_id).cloned().unwrap_or_default();
            let imgs = images.get(&l.product_id);
            let image = imgs
                .and_then(|is| is.iter().find(|(v, _)| *v == Some(l.variant_id)))
                .or_else(|| imgs.and_then(|is| is.first()))
                .map(|(_, i)| Image {
                    alt: if i.alt.is_empty() {
                        name.clone()
                    } else {
                        i.alt.clone()
                    },
                    ..i.clone()
                });
            CartLineView {
                id: l.id,
                variant_id: l.variant_id,
                product_id: l.product_id,
                slug,
                product_name: name,
                variant_label: l.label.clone(),
                sku: l.sku.clone(),
                image,
                quantity: l.quantity,
                unit_price: ctx.money(l.unit.unwrap_or(0)),
                discount: ctx.money(pl.map_or(0, |p| p.discount_minor)),
                total: ctx.money(pl.map_or(0, |p| p.gross_minor)),
                tax_rate: l.rate.to_string(),
                stock: l.stock,
                available: pl.is_some(),
            }
        })
        .collect();
    let view = CartView {
        id: cart_id,
        version: cart.version,
        currency: ctx.market.currency,
        item_count: lines.iter().map(|l| l.quantity).sum(),
        lines: view_lines,
        coupon: coupon_view,
        subtotal: ctx.money(goods_before_coupon),
        discount: ctx.money(priced.discount_minor),
        total: ctx.money(priced.total_minor),
        vat: priced
            .vat_recap
            .iter()
            .map(|r| VatRow {
                rate: r.tax_rate.to_string(),
                net: ctx.money(r.net_minor),
                vat: ctx.money(r.vat_minor),
                gross: ctx.money(r.gross_minor),
            })
            .collect(),
        vat_total: ctx.money(priced.vat_minor),
        ship_to_country: ship_to,
        free_shipping_remaining: crate::shipping::lowest_free_threshold(tx, ctx.market.id)
            .await?
            .map(|t| ctx.money((t - priced.total_minor).max(0))),
    };
    let meta = lines
        .iter()
        .map(|l| LineMeta {
            id: l.id,
            variant_id: l.variant_id,
            product_id: l.product_id,
            sku: l.sku.clone(),
            name: names
                .get(&l.product_id)
                .map(|(n, _)| n.clone())
                .unwrap_or_default(),
            label: l.label.clone(),
            quantity: l.quantity,
            available: by_line.contains_key(&l.id),
        })
        .collect();
    Ok(Priced {
        view,
        goods_before_coupon,
        goods_minor: priced.total_minor,
        weight_g: lines
            .iter()
            .filter(|l| l.unit.is_some())
            .map(|l| l.weight_g * i64::from(l.quantity))
            .sum(),
        lines: meta,
        coupon: coupon_state,
        input,
        vat_payer,
    })
}

/// The cart priced for checkout (`commerce::checkout`).
pub(crate) async fn priced(
    tx: &mut TenantTx,
    ctx: &Context,
    cart_id: Uuid,
) -> Result<Priced, Error> {
    price(tx, ctx, cart_id).await
}

/// The cart with totals recomputed now.
pub async fn view(tx: &mut TenantTx, ctx: &Context, cart: &CartRef) -> Result<CartView, Error> {
    Ok(price(tx, ctx, cart.id).await?.view)
}
