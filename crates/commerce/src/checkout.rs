//! Checkout and order placement (spec §8.2, §10.3, A3, A10, A12, A13, A15, A16, A20).
//!
//! The checkout state (contact, addresses, shipping method + pickup point, payment method)
//! lives on the cart, reached with the **checkout** capability; every change bumps the cart
//! `version`. [`view`] recomputes everything on every read: goods by `cart::priced`, live
//! shipping rates, the payment fee, and the totals by `pricing::price_cart`.
//!
//! [`place_order`] is one transaction (A12):
//! 1. `Idempotency-Key` per cart: a retry replays the stored result (with a fresh order token,
//!    tokens are never stored in plaintext);
//! 2. the cart row is locked; a converted cart is `409 order_already_placed` (one order per
//!    cart, backed by `orders_cart_unique`); the client's `version` and `total_minor` must
//!    match what it showed (`409 cart_changed` / `409 price_changed`);
//! 3. validation: legal checkboxes, email, addresses, ship-to country (A3), shipping method
//!    and pickup point, payment method (COD only with a COD shipping method, A16);
//! 4. re-price; unavailable lines or a coupon that no longer applies are `409`s;
//! 5. order number, order, lines and charges with their allocations (A15), addresses;
//! 6. stock reservations, in variant order so concurrent placements cannot deadlock (A13);
//! 7. coupon redemption (limits hold under the coupon row lock);
//! 8. the payment attempt (A10); COD orders are `confirmed` right away (A13/A16);
//! 9. optional consents that were ticked (A20), the timeline, outbox `order.created`, the
//!    confirmation email; the cart becomes `converted`.
//!
//! Provider init runs after the commit (`payments::init`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::cart::{self, CartView, Priced, VatRow};
use crate::consent::{self, ConsentChoice, ConsentPurpose, Purposes, Source, Subject};
use crate::customers::{self, AddressInput};
use crate::idempotency;
use crate::inventory::{self, MovementRef};
use crate::markets::invalid;
use crate::money::MoneyView;
use crate::notifications::{self, Brand, Email, Template};
use crate::orders::status::{PaymentKind, order_on_placement};
use crate::orders::{self, MethodSnapshot, OrderView, PickupPoint};
use crate::payments::{self, MethodKind, Payments};
use crate::pricing::cart::{CartInput, ChargeKind, PricedCart, price_cart};
use crate::promotions::coupons;
use crate::shipping::{self, Carrier, ShippingMethod};
use crate::staff::normalize_email;
use crate::storefront::{Context, messages};
use crate::tax;

/// Idempotency operation of order placement (scoped to the cart).
const PLACE_OP: &str = "POST /checkout/place-order";
/// Order numbers start here (six digits; at most ten, they are the variable symbol, A25).
const FIRST_NUMBER: i64 = 100_001;
const MAX_NOTE: usize = 1000;

/// The Packeta pickup-point widget (spec §10.5): loaded on interaction on the checkout origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PacketaWidget {
    /// `library.js` of the widget (Packeta's, or the local mock).
    pub script_url: String,
    /// The widget's public API key.
    pub api_key: String,
}

/// Platform checkout settings.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub payments: Payments,
    pub packeta: Option<PacketaWidget>,
}

// ---------------------------------------------------------------------------------------
// Inputs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckoutAddress {
    pub name: String,
    #[serde(default)]
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2.
    pub country: String,
    #[serde(default)]
    pub phone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ContactInput {
    pub email: String,
    #[serde(default)]
    pub phone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddressesInput {
    pub billing: CheckoutAddress,
    /// Delivery address; `null` = the billing address.
    #[serde(default)]
    pub shipping: Option<CheckoutAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ShippingInput {
    pub method_id: Uuid,
    /// Required for pickup-point carriers: the widget's selection.
    #[serde(default)]
    pub pickup_point: Option<PickupPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentInput {
    pub method: MethodKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceOrderInput {
    /// The cart `version` the summary was shown for.
    pub version: i32,
    /// The total (minor units) the customer agreed to.
    pub total_minor: i64,
    /// Terms and conditions (required).
    pub accept_terms: bool,
    /// Information about the right of withdrawal (required).
    pub accept_withdrawal: bool,
    /// Optional, unchecked by default (A20).
    #[serde(default)]
    pub email_marketing: bool,
    /// Optional, unchecked by default (A20).
    #[serde(default)]
    pub review_invites: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

fn clean_address(a: &CheckoutAddress) -> Result<CheckoutAddress, Error> {
    let c = customers::clean(&AddressInput {
        name: a.name.clone(),
        company: a.company.clone(),
        street: a.street.clone(),
        city: a.city.clone(),
        postal_code: a.postal_code.clone(),
        country: a.country.clone(),
        phone: a.phone.clone(),
        is_default: false,
    })?;
    Ok(CheckoutAddress {
        name: c.name,
        company: c.company,
        street: c.street,
        city: c.city,
        postal_code: c.postal_code,
        country: c.country,
        phone: c.phone,
    })
}

fn clean_phone(phone: Option<&str>) -> Result<Option<String>, Error> {
    let Some(p) = phone.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let ok = p.chars().count() <= 40
        && p.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '/'))
        && p.chars().filter(char::is_ascii_digit).count() >= 6;
    if !ok {
        return Err(invalid("invalid_phone", "not a phone number"));
    }
    Ok(Some(p.to_owned()))
}

fn clean_pickup(p: &PickupPoint) -> Result<PickupPoint, Error> {
    let bad = || invalid("invalid_pickup_point", "the pickup point is incomplete");
    let text = |v: &str, max: usize| -> Result<String, Error> {
        let v = v.trim();
        if v.is_empty() || v.chars().count() > max || v.chars().any(char::is_control) {
            return Err(bad());
        }
        Ok(v.to_owned())
    };
    let id = p.id.trim();
    if !(1..=64).contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(bad());
    }
    let country = p.country.trim().to_ascii_uppercase();
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(bad());
    }
    Ok(PickupPoint {
        id: id.to_owned(),
        name: text(&p.name, 200)?,
        street: text(&p.street, 200)?,
        city: text(&p.city, 100)?,
        zip: text(&p.zip, 20)?,
        country,
    })
}

// ---------------------------------------------------------------------------------------
// State

/// The checkout part of a cart row.
struct State {
    version: i32,
    email: Option<String>,
    phone: Option<String>,
    billing: Option<CheckoutAddress>,
    shipping: Option<CheckoutAddress>,
    shipping_method_id: Option<Uuid>,
    pickup_point: Option<PickupPoint>,
    payment_method: Option<MethodKind>,
}

async fn state(tx: &mut TenantTx, cart_id: Uuid) -> Result<State, Error> {
    let r = sqlx::query!(
        "SELECT version, email, phone, billing_address, shipping_address, shipping_method_id,
                pickup_point, payment_method
         FROM carts WHERE id = $1",
        cart_id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(State {
        version: r.version,
        email: r.email,
        phone: r.phone,
        billing: from_json(r.billing_address)?,
        shipping: from_json(r.shipping_address)?,
        shipping_method_id: r.shipping_method_id,
        pickup_point: from_json(r.pickup_point)?,
        payment_method: r
            .payment_method
            .as_deref()
            .map(MethodKind::parse)
            .transpose()?,
    })
}

fn from_json<T: serde::de::DeserializeOwned>(v: Option<Value>) -> Result<Option<T>, Error> {
    v.map(serde_json::from_value)
        .transpose()
        .map_err(|e| Error::Internal(format!("stored checkout state: {e}")))
}

fn to_json<T: Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// The market's ship-to countries the tax profile covers (A3).
async fn ship_to_countries(tx: &mut TenantTx, ctx: &Context) -> Result<Vec<String>, Error> {
    let profile = tax::require(tx).await?;
    Ok(ctx
        .market
        .country_codes
        .iter()
        .filter(|c| tax::check_ship_to(&profile, &ctx.market.country_codes, c).is_ok())
        .cloned()
        .collect())
}

pub async fn set_contact(
    tx: &mut TenantTx,
    cart: &cart::CartRef,
    input: &ContactInput,
) -> Result<(), Error> {
    let email = normalize_email(&input.email)?;
    let phone = clean_phone(input.phone.as_deref())?;
    sqlx::query!(
        "UPDATE carts SET email = $2, phone = $3, updated_at = now() WHERE id = $1",
        cart.id,
        email,
        phone
    )
    .execute(&mut **tx)
    .await?;
    cart::touch(tx, cart.id).await
}

/// Sets the addresses; the delivery country becomes the cart's ship-to country (VAT per
/// destination, A3) and must be one the market ships to.
pub async fn set_addresses(
    tx: &mut TenantTx,
    ctx: &Context,
    cart: &cart::CartRef,
    input: &AddressesInput,
) -> Result<(), Error> {
    let billing = clean_address(&input.billing)?;
    let shipping = input.shipping.as_ref().map(clean_address).transpose()?;
    let ship_to = shipping.as_ref().unwrap_or(&billing).country.clone();
    let profile = tax::require(tx).await?;
    tax::check_ship_to(&profile, &ctx.market.country_codes, &ship_to)?;
    sqlx::query!(
        "UPDATE carts SET billing_address = $2, shipping_address = $3, ship_to_country = $4,
             updated_at = now()
         WHERE id = $1",
        cart.id,
        to_json(&billing)?,
        shipping.as_ref().map(to_json).transpose()?,
        ship_to
    )
    .execute(&mut **tx)
    .await?;
    cart::touch(tx, cart.id).await
}

/// Selects a shipping method (and the pickup point for pickup carriers). A COD payment
/// selection is dropped when the new method does not allow COD.
pub async fn set_shipping(
    tx: &mut TenantTx,
    ctx: &Context,
    cart: &cart::CartRef,
    input: &ShippingInput,
) -> Result<(), Error> {
    let method = shipping::get(tx, input.method_id)
        .await
        .ok()
        .filter(|m| m.active && m.market_id == ctx.market.id)
        .ok_or_else(|| {
            invalid(
                "unknown_shipping_method",
                "this shipping method is not offered",
            )
        })?;
    let pickup = if method.carrier.needs_pickup_point() {
        let p = input.pickup_point.as_ref().ok_or_else(|| {
            invalid(
                "pickup_point_required",
                "choose a pickup point for this method",
            )
        })?;
        let p = clean_pickup(p)?;
        if !ship_to_countries(tx, ctx).await?.contains(&p.country) {
            return Err(invalid(
                "ship_to_not_allowed",
                format!("the market does not ship to {}", p.country),
            ));
        }
        Some(p)
    } else {
        None
    };
    sqlx::query!(
        "UPDATE carts SET shipping_method_id = $2, pickup_point = $3,
             payment_method = CASE WHEN payment_method = 'cod' AND NOT $4 THEN NULL
                                   ELSE payment_method END,
             updated_at = now()
         WHERE id = $1",
        cart.id,
        method.id,
        pickup.as_ref().map(to_json).transpose()?,
        method.cod_allowed
    )
    .execute(&mut **tx)
    .await?;
    cart::touch(tx, cart.id).await
}

pub async fn set_payment(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &Settings,
    cart: &cart::CartRef,
    input: &PaymentInput,
) -> Result<(), Error> {
    let st = state(tx, cart.id).await?;
    let method = selected_shipping(tx, ctx, &st).await?;
    let option = payments::methods(tx, &settings.payments, ctx.market.id)
        .await?
        .into_iter()
        .find(|m| m.kind == input.method && m.enabled && m.available);
    if option.is_none() {
        return Err(invalid(
            "unknown_payment_method",
            "this payment method is not offered",
        ));
    }
    if input.method.is_cod() && !method.as_ref().is_some_and(|m| m.cod_allowed) {
        return Err(invalid(
            "cod_not_allowed",
            "cash on delivery is not possible with this shipping method",
        ));
    }
    sqlx::query!(
        "UPDATE carts SET payment_method = $2, updated_at = now() WHERE id = $1",
        cart.id,
        input.method.as_str()
    )
    .execute(&mut **tx)
    .await?;
    cart::touch(tx, cart.id).await
}

async fn selected_shipping(
    tx: &mut TenantTx,
    ctx: &Context,
    st: &State,
) -> Result<Option<ShippingMethod>, Error> {
    let Some(id) = st.shipping_method_id else {
        return Ok(None);
    };
    Ok(shipping::get(tx, id)
        .await
        .ok()
        .filter(|m| m.active && m.market_id == ctx.market.id))
}

// ---------------------------------------------------------------------------------------
// The view

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ShippingOption {
    pub id: Uuid,
    pub carrier: Carrier,
    pub name: String,
    pub description: Option<String>,
    /// The live rate for this cart; `null` when the cart cannot use the method (too heavy).
    pub price: Option<MoneyView>,
    pub cod_allowed: bool,
    pub cod_fee: MoneyView,
    /// Pickup-point carriers: the widget to choose the point with.
    pub needs_pickup_point: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PaymentOption {
    pub kind: MethodKind,
    pub name: String,
    /// The fee charged with this method (COD: the shipping method's COD fee).
    pub fee: MoneyView,
    /// False when it cannot be chosen with the current shipping method (COD).
    pub selectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Totals {
    pub subtotal: MoneyView,
    pub discount: MoneyView,
    pub shipping: MoneyView,
    pub payment_fee: MoneyView,
    pub vat: Vec<VatRow>,
    pub vat_total: MoneyView,
    pub total: MoneyView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Legal {
    pub terms_url: String,
    pub withdrawal_url: String,
    pub privacy_url: String,
    /// Recorded with the optional consents (A20).
    pub text_version: String,
}

/// Everything the one-page checkout renders. Recomputed on every read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CheckoutView {
    pub cart: CartView,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub billing_address: Option<CheckoutAddress>,
    pub shipping_address: Option<CheckoutAddress>,
    /// A3: where this market delivers.
    pub ship_to_countries: Vec<String>,
    pub shipping_methods: Vec<ShippingOption>,
    pub shipping_method_id: Option<Uuid>,
    pub pickup_point: Option<PickupPoint>,
    pub packeta: Option<PacketaWidget>,
    pub payment_methods: Vec<PaymentOption>,
    pub payment_method: Option<MethodKind>,
    pub totals: Totals,
    pub legal: Legal,
    /// What is still missing before the order can be placed (`email`, `billing_address`,
    /// `shipping_method`, `pickup_point`, `payment_method`, `cart_unavailable`).
    pub missing: Vec<String>,
}

/// The priced checkout: goods, the selected method's rate and the payment fee.
struct Computed {
    priced: Priced,
    full: PricedCart,
    method: Option<ShippingMethod>,
    shipping_minor: Option<i64>,
}

async fn compute(
    tx: &mut TenantTx,
    ctx: &Context,
    cart_id: Uuid,
    st: &State,
) -> Result<Computed, Error> {
    let priced = cart::priced(tx, ctx, cart_id).await?;
    let method = selected_shipping(tx, ctx, st).await?;
    let shipping_minor = method
        .as_ref()
        .and_then(|m| m.quote(priced.goods_minor, priced.weight_g));
    let fee_minor = match (st.payment_method, &method) {
        (Some(MethodKind::Cod), Some(m)) if m.cod_allowed => Some(m.cod_fee_minor),
        _ => None,
    };
    let full = price_cart(&CartInput {
        shipping_minor,
        payment_fee_minor: fee_minor,
        ..priced.input.clone()
    })?;
    Ok(Computed {
        priced,
        full,
        method,
        shipping_minor,
    })
}

fn charge(full: &PricedCart, kind: ChargeKind) -> i64 {
    full.charges
        .iter()
        .find(|c| c.kind == kind)
        .map_or(0, |c| c.gross_minor)
}

fn goods_discount(full: &PricedCart) -> i64 {
    full.lines.iter().map(|l| l.discount_minor).sum()
}

fn legal(ctx: &Context) -> Legal {
    Legal {
        terms_url: ctx.page_url("/pages/obchodni-podminky"),
        withdrawal_url: ctx.page_url("/pages/odstoupeni-od-smlouvy"),
        privacy_url: ctx.page_url("/pages/ochrana-osobnich-udaju"),
        text_version: consent::TEXT_VERSION.into(),
    }
}

fn payment_name(ctx: &Context, m: &payments::PaymentMethod) -> String {
    let own = serde_json::to_value(&m.name_i18n)
        .ok()
        .and_then(|v| ctx.text(&v));
    own.unwrap_or_else(|| {
        messages::text(&ctx.locale, &format!("payment.{}", m.kind.as_str())).to_owned()
    })
}

fn i18n_text(ctx: &Context, v: &BTreeMap<String, String>) -> Option<String> {
    serde_json::to_value(v).ok().and_then(|v| ctx.text(&v))
}

/// The checkout of the cart behind `cart`.
pub async fn view(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &Settings,
    cart: &cart::CartRef,
) -> Result<CheckoutView, Error> {
    let st = state(tx, cart.id).await?;
    let c = compute(tx, ctx, cart.id, &st).await?;
    let methods = shipping::active(tx, ctx.market.id).await?;
    let shipping_methods = methods
        .iter()
        .map(|m| ShippingOption {
            id: m.id,
            carrier: m.carrier,
            name: i18n_text(ctx, &m.name_i18n).unwrap_or_default(),
            description: i18n_text(ctx, &m.description_i18n),
            price: m
                .quote(c.priced.goods_minor, c.priced.weight_g)
                .map(|p| ctx.money(p)),
            cod_allowed: m.cod_allowed,
            cod_fee: ctx.money(m.cod_fee_minor),
            needs_pickup_point: m.carrier.needs_pickup_point(),
        })
        .collect();
    let payment_methods = payments::methods(tx, &settings.payments, ctx.market.id)
        .await?
        .into_iter()
        .filter(|m| m.enabled && m.available)
        .map(|m| PaymentOption {
            kind: m.kind,
            name: payment_name(ctx, &m),
            fee: ctx.money(if m.kind.is_cod() {
                c.method.as_ref().map_or(0, |s| s.cod_fee_minor)
            } else {
                0
            }),
            selectable: !m.kind.is_cod() || c.method.as_ref().is_some_and(|s| s.cod_allowed),
        })
        .collect();
    let mut missing = Vec::new();
    if st.email.is_none() {
        missing.push("email");
    }
    if st.billing.is_none() {
        missing.push("billing_address");
    }
    match &c.method {
        None => missing.push("shipping_method"),
        Some(m) if m.carrier.needs_pickup_point() && st.pickup_point.is_none() => {
            missing.push("pickup_point");
        }
        Some(_) if c.shipping_minor.is_none() => missing.push("shipping_method"),
        Some(_) => {}
    }
    if st.payment_method.is_none() {
        missing.push("payment_method");
    }
    if c.priced.lines.iter().any(|l| !l.available) || c.priced.lines.is_empty() {
        missing.push("cart_unavailable");
    }
    let full = &c.full;
    Ok(CheckoutView {
        cart: c.priced.view.clone(),
        email: st.email,
        phone: st.phone,
        billing_address: st.billing,
        shipping_address: st.shipping,
        ship_to_countries: ship_to_countries(tx, ctx).await?,
        shipping_methods,
        shipping_method_id: c.method.as_ref().map(|m| m.id),
        pickup_point: st.pickup_point,
        packeta: settings.packeta.clone(),
        payment_methods,
        payment_method: st.payment_method,
        totals: Totals {
            subtotal: ctx.money(c.priced.goods_before_coupon),
            discount: ctx.money(goods_discount(full)),
            shipping: ctx.money(charge(full, ChargeKind::Shipping)),
            payment_fee: ctx.money(charge(full, ChargeKind::PaymentFee)),
            vat: full
                .vat_recap
                .iter()
                .map(|r| VatRow {
                    rate: r.tax_rate.to_string(),
                    net: ctx.money(r.net_minor),
                    vat: ctx.money(r.vat_minor),
                    gross: ctx.money(r.gross_minor),
                })
                .collect(),
            vat_total: ctx.money(full.vat_minor),
            total: ctx.money(full.total_minor),
        },
        legal: legal(ctx),
        missing: missing.into_iter().map(str::to_owned).collect(),
    })
}

// ---------------------------------------------------------------------------------------
// Placement

/// What the placement returns (and, without the token, what an idempotent replay replays).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredPlacement {
    order_id: Uuid,
    number: String,
    attempt_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub order_id: Uuid,
    pub number: String,
    pub attempt_id: Uuid,
    /// Order capability for `/o/<token>` (fresh on every call, A4).
    pub token: String,
    /// True when this was an idempotent replay of an earlier placement.
    pub replayed: bool,
}

/// Who places the order.
#[derive(Debug, Clone, Default)]
pub struct Placer<'a> {
    /// The signed-in customer (checkout-origin session), if any.
    pub customer_id: Option<Uuid>,
    /// Salted client IP hash (consent evidence).
    pub ip_hash: Option<&'a [u8]>,
}

fn conflict(code: &'static str, detail: &str) -> Error {
    Error::Conflict {
        code,
        detail: detail.into(),
    }
}

/// Places the order for the cart behind the checkout capability `token` (A12). `key` is the
/// client's `Idempotency-Key`, `request_hash` the hash of the request body.
#[allow(clippy::too_many_arguments)]
pub async fn place_order(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &Settings,
    token: &str,
    key: &str,
    request_hash: &str,
    input: &PlaceOrderInput,
    placer: &Placer<'_>,
) -> Result<Placement, Error> {
    idempotency::validate_key(key)?;
    let (cart_ref, open) = cart::find_for_order(tx, ctx, token).await?;
    let op = format!("{PLACE_OP} cart:{}", cart_ref.id);
    if let Some(stored) = idempotency::begin(tx, &op, key, request_hash).await? {
        let s: StoredPlacement = serde_json::from_value(stored.body)
            .map_err(|e| Error::Internal(format!("stored placement: {e}")))?;
        return Ok(Placement {
            token: orders::issue_token(tx, s.order_id).await?,
            order_id: s.order_id,
            number: s.number,
            attempt_id: s.attempt_id,
            replayed: true,
        });
    }
    if !open {
        return Err(conflict(
            "order_already_placed",
            "an order was already placed from this cart",
        ));
    }
    let st = state(tx, cart_ref.id).await?;
    if st.version != input.version {
        return Err(conflict(
            "cart_changed",
            "the cart changed; review the order again",
        ));
    }
    if !input.accept_terms || !input.accept_withdrawal {
        return Err(invalid(
            "legal_consent_required",
            "accept the terms and the withdrawal information",
        ));
    }
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    if notes.as_ref().is_some_and(|n| n.chars().count() > MAX_NOTE) {
        return Err(invalid(
            "invalid_notes",
            "notes are at most 1000 characters",
        ));
    }

    // Validation (A3, §10.3 step 1).
    let missing = |what: &str| invalid("checkout_incomplete", format!("{what} is missing"));
    let email = st.email.clone().ok_or_else(|| missing("email"))?;
    let billing = st
        .billing
        .clone()
        .ok_or_else(|| missing("billing_address"))?;
    let delivery = st.shipping.clone();
    let ship_to = delivery.as_ref().unwrap_or(&billing).country.clone();
    let profile = tax::require(tx).await?;
    tax::check_ship_to(&profile, &ctx.market.country_codes, &ship_to)?;
    let kind = st.payment_method.ok_or_else(|| missing("payment_method"))?;
    let pay = payments::methods(tx, &settings.payments, ctx.market.id)
        .await?
        .into_iter()
        .find(|m| m.kind == kind && m.enabled && m.available)
        .ok_or_else(|| {
            conflict(
                "payment_method_unavailable",
                "choose another payment method",
            )
        })?;

    // Re-price (step 2) with the selected method's live rate.
    let c = compute(tx, ctx, cart_ref.id, &st).await?;
    let method = c.method.clone().ok_or_else(|| missing("shipping_method"))?;
    if method.carrier.needs_pickup_point() {
        let point = st
            .pickup_point
            .as_ref()
            .ok_or_else(|| missing("pickup_point"))?;
        if point.country != ship_to {
            return Err(invalid(
                "pickup_point_country",
                "the pickup point is in another country than the address",
            ));
        }
    }
    if kind.is_cod() && !method.cod_allowed {
        return Err(conflict(
            "cod_not_allowed",
            "cash on delivery is not possible with this shipping method",
        ));
    }
    let shipping_minor = c.shipping_minor.ok_or_else(|| {
        conflict(
            "shipping_unavailable",
            "the shipping method cannot carry this cart",
        )
    })?;
    if c.priced.lines.is_empty() {
        return Err(conflict("cart_empty", "the cart is empty"));
    }
    if c.priced.lines.iter().any(|l| !l.available) {
        return Err(conflict(
            "cart_unavailable",
            "some items are no longer available",
        ));
    }
    // The coupon's own reason (`coupon_exhausted`, `coupon_min_subtotal`, ...) as a 409.
    if let Some((_, Err(reason))) = &c.priced.coupon {
        return Err(conflict(
            reason,
            "the coupon no longer applies to this order",
        ));
    }
    let full = &c.full;
    if full.total_minor != input.total_minor {
        return Err(conflict(
            "price_changed",
            "the total changed; review the order again",
        ));
    }

    // Stock and coupon first, number and rows after: the per-tenant number row lock is
    // held only for the rest of the transaction. Lock order everywhere: cart, stock levels
    // (by variant), coupon, order number, so concurrent placements cannot deadlock.
    let order_id = crate::id::new_id();
    let now = Utc::now();
    let coupon = match &c.priced.coupon {
        Some((id, Ok(()))) => Some((*id, full.coupon_code.clone())),
        _ => None,
    };
    // Stock (A13): variant order, one movement per variant and order (replays are no-ops).
    let mut units: BTreeMap<Uuid, i32> = BTreeMap::new();
    for l in &c.priced.lines {
        *units.entry(l.variant_id).or_default() += i32::try_from(l.quantity).unwrap_or(i32::MAX);
    }
    let ref_id = order_id.to_string();
    let movement = MovementRef {
        ref_type: "order",
        ref_id: &ref_id,
    };
    for (variant, qty) in units {
        inventory::reserve(tx, &movement, variant, qty).await?;
    }

    // Coupon (A12: limits checked under the coupon row lock). Per-customer limits count by
    // email, the identity every order has.
    if let Some((coupon_id, _)) = &coupon {
        coupons::redeem(tx, *coupon_id, &email, &ref_id, now).await?;
    }

    // The order (steps 5-6 of §10.3, A15 allocations persisted).
    let number = sqlx::query_scalar!(
        "INSERT INTO order_numbers (tenant_id, last) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET last = order_numbers.last + 1
         RETURNING last",
        tx.tenant_id(),
        FIRST_NUMBER
    )
    .fetch_one(&mut **tx)
    .await?;
    let payment_kind = if kind.is_cod() {
        PaymentKind::CashOnDelivery
    } else {
        PaymentKind::Prepaid
    };
    let (status, _) = order_on_placement(payment_kind);
    let expires_at: Option<DateTime<Utc>> = if kind.is_cod() {
        None
    } else {
        pay.timeout().map(|t| now + t)
    };
    let snapshot = MethodSnapshot {
        id: method.id,
        carrier: method.carrier,
        name: i18n_text(ctx, &method.name_i18n).unwrap_or_default(),
        price_minor: shipping_minor,
    };
    sqlx::query!(
        "INSERT INTO orders (id, tenant_id, number, market_id, cart_id, customer_id, email, phone,
             locale, currency, status, payment_status, fulfillment_status, ship_to_country,
             vat_payer, subtotal_minor, discount_minor, shipping_minor, payment_fee_minor,
             tax_minor, rounding_minor, total_minor, vat_recap, coupon_id, coupon_code,
             shipping_method_id, shipping_method_snapshot, payment_method, pickup_point, notes,
             payment_expires_at, placed_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'unpaid', 'unfulfilled', $12, $13,
                 $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29,
                 $30)",
        order_id,
        tx.tenant_id(),
        number,
        ctx.market.id,
        cart_ref.id,
        placer.customer_id,
        email,
        st.phone,
        ctx.locale,
        ctx.market.currency.code(),
        status.as_str(),
        ship_to,
        c.priced.vat_payer,
        c.priced.goods_before_coupon,
        goods_discount(full),
        charge(full, ChargeKind::Shipping),
        charge(full, ChargeKind::PaymentFee),
        full.vat_minor,
        charge(full, ChargeKind::Rounding),
        full.total_minor,
        to_json(&full.vat_recap)?,
        coupon.as_ref().map(|c| c.0),
        coupon.as_ref().and_then(|c| c.1.clone()),
        method.id,
        to_json(&snapshot)?,
        kind.as_str(),
        st.pickup_point.as_ref().map(to_json).transpose()?,
        notes,
        expires_at,
        now
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        if crate::unique_violation(&e) {
            conflict(
                "order_already_placed",
                "an order was already placed from this cart",
            )
        } else {
            e.into()
        }
    })?;
    let by_line: BTreeMap<Uuid, &crate::pricing::cart::PricedLine> =
        full.lines.iter().map(|l| (l.id, l)).collect();
    for (position, meta) in c.priced.lines.iter().enumerate() {
        let pl = by_line
            .get(&meta.id)
            .ok_or_else(|| Error::Internal("priced line missing".into()))?;
        sqlx::query!(
            "INSERT INTO order_lines (id, tenant_id, order_id, position, variant_id, product_id,
                 sku, name, options_label, quantity, unit_gross_minor, base_minor, discount_minor,
                 total_minor, tax_rate, tax_minor, net_minor)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
            crate::id::new_id(),
            tx.tenant_id(),
            order_id,
            i32::try_from(position).unwrap_or(i32::MAX),
            meta.variant_id,
            meta.product_id,
            meta.sku,
            meta.name,
            meta.label,
            i32::try_from(pl.quantity).unwrap_or(i32::MAX),
            pl.unit_price_minor,
            pl.base_minor,
            pl.discount_minor,
            pl.gross_minor,
            pl.tax_rate.to_string(),
            pl.vat_minor,
            pl.net_minor
        )
        .execute(&mut **tx)
        .await?;
    }
    for ch in &full.charges {
        sqlx::query!(
            "INSERT INTO order_charges (tenant_id, order_id, kind, base_minor, discount_minor,
                 total_minor, tax_minor, net_minor, portions)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            tx.tenant_id(),
            order_id,
            match ch.kind {
                ChargeKind::Shipping => "shipping",
                ChargeKind::PaymentFee => "payment_fee",
                ChargeKind::Rounding => "rounding",
            },
            ch.base_minor,
            ch.discount_minor,
            ch.gross_minor,
            ch.vat_minor,
            ch.net_minor,
            to_json(&ch.portions)?
        )
        .execute(&mut **tx)
        .await?;
    }
    for (kind_name, a) in [("billing", Some(&billing)), ("shipping", delivery.as_ref())] {
        let Some(a) = a else { continue };
        sqlx::query!(
            "INSERT INTO order_addresses (tenant_id, order_id, kind, name, company, street, city,
                 postal_code, country, phone)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            tx.tenant_id(),
            order_id,
            kind_name,
            a.name,
            a.company,
            a.street,
            a.city,
            a.postal_code,
            a.country,
            a.phone
        )
        .execute(&mut **tx)
        .await?;
    }

    // Payment attempt (A10); a bank transfer gets its account, variable symbol and QR (A25).
    let bank = if kind == MethodKind::BankTransfer {
        Some(
            payments::bank::prepare(
                tx,
                ctx.market.id,
                number,
                full.total_minor,
                ctx.market.currency.code(),
                &ctx.shop_name,
            )
            .await?,
        )
    } else {
        None
    };
    let attempt_id = payments::create_attempt(
        tx,
        order_id,
        kind,
        full.total_minor,
        ctx.market.currency.code(),
        expires_at,
        bank.as_ref(),
    )
    .await?;

    // Timeline, consents, outbox, email.
    orders::event(
        tx,
        order_id,
        "placed",
        &json!({ "number": number.to_string(), "total_minor": full.total_minor,
                 "payment_method": kind, "attempt_id": attempt_id, "status": status }),
        "customer",
    )
    .await?;
    orders::event(
        tx,
        order_id,
        "legal_accepted",
        &json!({ "terms": true, "withdrawal_information": true,
                 "text_version": consent::TEXT_VERSION }),
        "customer",
    )
    .await?;
    record_consents(tx, input, &email, placer).await?;
    platform::queue::publish(
        &mut **tx,
        orders::CREATED_EVENT,
        &json!({ "order_id": order_id, "number": number.to_string(),
                 "total_minor": full.total_minor, "currency": ctx.market.currency.code() }),
    )
    .await?;
    let token = orders::issue_token(tx, order_id).await?;
    let view = orders::view(tx, order_id).await?;
    send_confirmation(tx, ctx, &view, &token).await?;
    sqlx::query!(
        "UPDATE carts SET status = 'converted', updated_at = now() WHERE id = $1",
        cart_ref.id
    )
    .execute(&mut **tx)
    .await?;
    let stored = StoredPlacement {
        order_id,
        number: number.to_string(),
        attempt_id,
    };
    idempotency::finish(tx, &op, key, 201, &to_json(&stored)?).await?;
    Ok(Placement {
        order_id,
        number: stored.number,
        attempt_id,
        token,
        replayed: false,
    })
}

/// A20: the optional checkboxes are unchecked by default; only a ticked one is a choice, so
/// only those are recorded (for the email address and the signed-in customer).
async fn record_consents(
    tx: &mut TenantTx,
    input: &PlaceOrderInput,
    email: &str,
    placer: &Placer<'_>,
) -> Result<(), Error> {
    let purposes = Purposes {
        email_marketing: input.email_marketing.then_some(true),
        review_invites: input.review_invites.then_some(true),
        ..Purposes::default()
    };
    if purposes.get(ConsentPurpose::EmailMarketing).is_none()
        && purposes.get(ConsentPurpose::ReviewInvites).is_none()
    {
        return Ok(());
    }
    let choice = ConsentChoice {
        purposes,
        text_version: consent::TEXT_VERSION.into(),
        source: Source::Checkout,
    };
    consent::record(
        tx,
        &Subject::Email(email.to_owned()),
        &choice,
        placer.ip_hash,
    )
    .await?;
    if let Some(customer) = placer.customer_id {
        consent::record(tx, &Subject::Customer(customer), &choice, placer.ip_hash).await?;
    }
    Ok(())
}

fn mail_text(locale: &str, key: &str) -> String {
    notifications::label(locale, key)
}

/// The order summary every order email shows (`order.mjml`).
fn order_vars(o: &OrderView, url: String) -> Value {
    let l = o.locale.as_str();
    let mut totals = vec![
        json!({ "label": mail_text(l, "order_confirmation.subtotal"), "amount": o.subtotal.formatted }),
    ];
    if o.discount.amount_minor > 0 {
        totals.push(json!({ "label": mail_text(l, "order_confirmation.discount"), "amount": format!("−{}", o.discount.formatted) }));
    }
    totals.push(json!({ "label": mail_text(l, "order_confirmation.shipping"), "amount": o.shipping_total.formatted }));
    if o.payment_fee.amount_minor != 0 {
        totals.push(json!({ "label": mail_text(l, "order_confirmation.payment_fee"), "amount": o.payment_fee.formatted }));
    }
    if o.rounding.amount_minor != 0 {
        totals.push(json!({ "label": mail_text(l, "order_confirmation.rounding"), "amount": o.rounding.formatted }));
    }
    json!({
        "number": o.number,
        "lines": o.lines.iter().map(|x| json!({
            "name": x.name, "detail": x.options_label, "quantity": x.quantity,
            "total": x.total.formatted,
        })).collect::<Vec<_>>(),
        "totals": totals,
        "total": o.total.formatted,
        "url": url,
    })
}

/// Bank-transfer instructions for `bank_transfer.mjml` (null for other methods).
fn bank_vars(o: &OrderView) -> Value {
    let Some(b) = &o.payment.bank_transfer else {
        return Value::Null;
    };
    let due = o.payment.expires_at.map(|d| match o.locale.as_str() {
        "en" => d.format("%Y-%m-%d").to_string(),
        _ => d.format("%-d. %-m. %Y").to_string(),
    });
    json!({
        "iban": b.iban, "bic": b.bic, "account_name": b.account_name,
        "variable_symbol": b.variable_symbol, "amount": b.amount.formatted,
        "message": b.message, "qr_svg": b.qr_svg, "due": due,
    })
}

/// The order confirmation (lines, totals, VAT recap, delivery, payment), in the order's
/// locale. `sensitive`: the body carries the order capability link, so it is dropped once the
/// message is final (like sign-in links).
async fn send_confirmation(
    tx: &mut TenantTx,
    ctx: &Context,
    o: &OrderView,
    token: &str,
) -> Result<(), Error> {
    let l = o.locale.as_str();
    let point = o
        .shipping
        .pickup_point
        .as_ref()
        .map(|p| format!("{}, {}, {} {}", p.name, p.street, p.zip, p.city));
    let address = o
        .shipping_address
        .as_ref()
        .or(o.billing_address.as_ref())
        .map(|a| {
            format!(
                "{}, {}, {} {}, {}",
                a.name, a.street, a.postal_code, a.city, a.country
            )
        });
    let vars = json!({
        "order": order_vars(o, ctx.checkout_url(&format!("/o/{token}"))),
        "vat": o.vat.iter().map(|r| json!({
            "rate": r.rate, "net": r.net.formatted, "vat": r.vat.formatted,
        })).collect::<Vec<_>>(),
        "shipping": {
            "method": o.shipping.name,
            "pickup_point": point,
            "address": if point.is_some() { None } else { address },
        },
        "payment": {
            "method": mail_text(l, &format!("payment.{}", o.payment.method.as_str())),
            "bank_transfer": o.payment.method == MethodKind::BankTransfer,
            "cod": o.payment.method == MethodKind::Cod,
        },
        "bank": bank_vars(o),
    });
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::OrderConfirmation,
            stream: Stream::Transactional,
            to: &o.email,
            locale: l,
            vars,
            idempotency_key: format!("order_confirmation:{}", o.id),
            sensitive: true,
        },
    )
    .await?;
    Ok(())
}

/// Bank-transfer reminders (spec §10.3: day 3 and day 6 of the default 7-day window): each
/// due reminder is claimed under the order lock and its email enqueued in the same
/// transaction, with a fresh order link. Returns how many were sent.
pub async fn send_payment_reminders(
    db: &sqlx::PgPool,
    urls: &crate::storefront::PublicUrls,
    max: i32,
) -> Result<usize, Error> {
    let mut sent = 0;
    for due in payments::bank::due_reminders(db, max).await? {
        let mut tx = platform::db::tenant_tx(db, due.tenant_id).await?;
        let Some((order_id, n)) =
            payments::bank::claim_reminder(&mut tx, due.attempt_id, Utc::now()).await?
        else {
            continue;
        };
        let o = orders::view(&mut tx, order_id).await?;
        let market = sqlx::query_scalar!("SELECT market_id FROM orders WHERE id = $1", order_id)
            .fetch_one(&mut *tx)
            .await?;
        let ctx =
            crate::storefront::context(&mut tx, urls, market, Some(&o.locale), Utc::now()).await?;
        let token = orders::issue_token(&mut tx, order_id).await?;
        let brand = Brand::load(&mut tx, ctx.base_url.clone()).await?;
        notifications::enqueue(
            &mut tx,
            &brand,
            Email {
                template: Template::PaymentReminder,
                stream: Stream::Transactional,
                to: &o.email,
                locale: &o.locale,
                vars: json!({
                    "order": order_vars(&o, ctx.checkout_url(&format!("/o/{token}"))),
                    "bank": bank_vars(&o),
                    "reminder": n,
                }),
                idempotency_key: format!("payment_reminder:{}:{n}", due.attempt_id),
                sensitive: true,
            },
        )
        .await?;
        tx.commit().await?;
        sent += 1;
    }
    Ok(sent)
}

/// The order behind a capability token (the `/o/<token>` page).
pub async fn order_by_token(tx: &mut TenantTx, token: &str) -> Result<OrderView, Error> {
    let id = orders::by_token(tx, token).await?;
    orders::view(tx, id).await
}

/// A10 payment timeouts: cancels unpaid orders whose payment window closed, releasing their
/// stock and coupon. Each order runs in its own tenant transaction. Returns how many expired.
pub async fn expire_due(db: &sqlx::PgPool, max: i32) -> Result<usize, Error> {
    let due = sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", order_id AS "order_id!"
           FROM platform.due_payment_expiries($1)"#,
        max
    )
    .fetch_all(db)
    .await?;
    let mut expired = 0;
    for d in due {
        let mut tx = platform::db::tenant_tx(db, d.tenant_id).await?;
        let mut o = orders::lock(&mut tx, d.order_id).await?;
        // Re-checked under the lock: a payment may have confirmed it meanwhile.
        if o.status != "pending" || o.payment_expires_at.is_none_or(|e| e > Utc::now()) {
            continue;
        }
        orders::event(
            &mut tx,
            o.id,
            "payment_window_closed",
            &json!({ "expired_at": o.payment_expires_at }),
            "system",
        )
        .await?;
        orders::expire_unpaid(&mut tx, &mut o, "system").await?;
        tx.commit().await?;
        expired += 1;
    }
    Ok(expired)
}
