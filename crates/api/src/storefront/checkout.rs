//! Checkout (spec §8.2, §10.3, A1, A4, A10, A12), reached only from the checkout origin: the
//! edge maps `checkout.<shop>/_p/checkout/*` here with the checkout cart capability
//! (`__Host-cart` → `X-Cart-Token`), the session and the client IP, and enforces same-origin
//! JSON. `/_p/fake-pay/*` (the fake gateway's page, `PAYMENTS_FAKE=1`) lands here too.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use commerce::cart::{self, Scope};
use commerce::checkout::{
    self, AddressesInput, CheckoutView, ContactInput, PaymentInput, PlaceOrderInput, Placer,
    ShippingInput,
};
use commerce::customers;
use commerce::idempotency;
use commerce::money::MoneyView;
use commerce::payments::{self, AttemptStatus, FakeEvent, NextAction, Outcome};
use commerce::storefront::Context;
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::customer::{
    CLIENT_UA_HEADER, CONSENT_SUBJECT_HEADER, SESSION_HEADER, header_str, ip_hash, no_store,
};
use super::{CART_HEADER, CartHeader, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::{IdempotencyHeader, REPLAYED, idempotency_key, parse_json};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_checkout))
        .routes(routes!(put_contact))
        .routes(routes!(put_addresses))
        .routes(routes!(put_shipping))
        .routes(routes!(put_payment))
        .routes(routes!(place_order))
        .routes(routes!(fake_pay_page, fake_pay))
}

fn cart_token(headers: &HeaderMap) -> Result<String, Error> {
    header_str(headers, CART_HEADER)
        .map(str::to_owned)
        .ok_or(Error::NotFound)
}

/// Runs `f` on the checkout-scoped cart and answers with the recomputed checkout.
async fn with_checkout(
    s: &AppState,
    shopper: &Shopper,
    headers: &HeaderMap,
    f: impl AsyncFnOnce(&mut TenantTx, &Context, &cart::CartRef) -> Result<(), Error>,
) -> Result<Response, Error> {
    let token = cart_token(headers)?;
    let view = with_ctx(s, shopper, async |tx, ctx| {
        let c = cart::find(tx, ctx, &token, Some(Scope::Checkout)).await?;
        f(tx, ctx, &c).await?;
        checkout::view(tx, ctx, &s.checkout, &c).await
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

/// The checkout of the handed-off cart: lines and totals, the market's shipping methods with
/// live rates, payment methods, the selections so far and what is still missing.
#[utoipa::path(
    get,
    path = "/storefront/v1/checkout",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    responses(
        (status = 200, body = CheckoutView),
        (status = 404, description = "No checkout cart", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_checkout(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    with_checkout(&s, &shopper, &headers, async |_, _, _| Ok(())).await
}

#[utoipa::path(
    put,
    path = "/storefront/v1/checkout/contact",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = ContactInput,
    responses(
        (status = 200, body = CheckoutView),
        (status = 422, description = "invalid_email | invalid_phone", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_contact(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ContactInput = parse_json(&body)?;
    with_checkout(&s, &shopper, &headers, async |tx, _, c| {
        checkout::set_contact(tx, c, &input).await
    })
    .await
}

/// Billing and delivery address. The delivery country must be one the market ships to and
/// the tax profile covers (A3); VAT follows it.
#[utoipa::path(
    put,
    path = "/storefront/v1/checkout/addresses",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = AddressesInput,
    responses(
        (status = 200, body = CheckoutView),
        (status = 422, description = "invalid_address | ship_to_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_addresses(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: AddressesInput = parse_json(&body)?;
    with_checkout(&s, &shopper, &headers, async |tx, ctx, c| {
        checkout::set_addresses(tx, ctx, c, &input).await
    })
    .await
}

#[utoipa::path(
    put,
    path = "/storefront/v1/checkout/shipping",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = ShippingInput,
    responses(
        (status = 200, body = CheckoutView),
        (status = 422, description = "unknown_shipping_method | pickup_point_required | invalid_pickup_point", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_shipping(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ShippingInput = parse_json(&body)?;
    if let Some(point) = &input.pickup_point {
        verify_pickup_point(&s, &shopper, &point.id).await?;
    }
    with_checkout(&s, &shopper, &headers, async |tx, ctx, c| {
        checkout::set_shipping(tx, ctx, c, &input).await
    })
    .await
}

/// WP12: the widget's choice is re-checked with Packeta (the tenant's key, else the
/// platform's). An unknown point is refused; an unreachable service does not block checkout
/// (the label call validates again).
async fn verify_pickup_point(s: &AppState, shopper: &Shopper, point: &str) -> Result<(), Error> {
    let Some(carriers) = &s.carriers else {
        return Ok(());
    };
    let tenant_key = with_ctx(s, shopper, async |tx, _| {
        commerce::carriers::packeta_public_key(tx).await
    })
    .await?;
    let Some(key) = tenant_key.or_else(|| s.checkout.packeta.as_ref().map(|p| p.api_key.clone()))
    else {
        return Ok(());
    };
    match commerce::carriers::packeta::validate_point(carriers, &key, point).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::Validation {
            code: "invalid_pickup_point",
            detail: "Packeta does not know this pickup point; choose another one".into(),
        }),
        Err(e) => {
            tracing::warn!(error = %e, "pickup point validation unavailable; accepted");
            Ok(())
        }
    }
}

#[utoipa::path(
    put,
    path = "/storefront/v1/checkout/payment",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = PaymentInput,
    responses(
        (status = 200, body = CheckoutView),
        (status = 422, description = "unknown_payment_method | cod_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_payment(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: PaymentInput = parse_json(&body)?;
    with_checkout(&s, &shopper, &headers, async |tx, ctx, c| {
        checkout::set_payment(tx, ctx, &s.checkout, c, &input).await
    })
    .await
}

/// What the client does after placing the order or starting a payment attempt.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PaymentStart {
    pub attempt_id: Uuid,
    /// `null` when the provider could not be reached yet: retry with
    /// `POST /orders/{token}/payment-attempts/{attempt}/init` (A10).
    pub action: Option<NextAction>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct PlacedOrder {
    pub order_id: Uuid,
    pub number: String,
    /// The order page on the checkout origin (`/o/<token>`, a read-only capability, A4).
    pub confirmation_url: String,
    pub payment: PaymentStart,
}

/// Initializes an attempt at its provider after the commit (A10). A failure is logged and
/// left to the client's retry through the init endpoint.
pub(crate) async fn start_payment(
    s: &AppState,
    tenant_id: Uuid,
    attempt_id: Uuid,
    return_path: &str,
) -> PaymentStart {
    let action = match payments::init(
        &s.db,
        tenant_id,
        &s.checkout.payments,
        attempt_id,
        return_path,
    )
    .await
    {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!(attempt = %attempt_id, error = %e, "payment init failed; the client retries");
            None
        }
    };
    PaymentStart { attempt_id, action }
}

/// Places the order (A12): one order per cart, `Idempotency-Key` required (a retry with the
/// same key answers the same order with `Idempotent-Replayed: true` and a fresh order token).
/// `version` and `total_minor` are what the customer saw: `409 cart_changed` /
/// `409 price_changed` when they no longer hold. Stock is reserved and the coupon redeemed in
/// the same transaction; then the payment attempt is initialized at the provider (A10).
#[utoipa::path(
    post,
    path = "/storefront/v1/checkout/place-order",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader, IdempotencyHeader),
    request_body = PlaceOrderInput,
    responses(
        (status = 201, body = PlacedOrder),
        (status = 400, description = "idempotency_key_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, description = "No checkout cart", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "order_already_placed | cart_changed | price_changed | insufficient_stock | out_of_stock | cart_unavailable | coupon_* | idempotency_conflict", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "legal_consent_required | checkout_incomplete | ship_to_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn place_order(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: PlaceOrderInput = parse_json(&body)?;
    let key = idempotency_key(&headers)?.ok_or(Error::BadRequest {
        code: "idempotency_key_required",
        detail: "placing an order needs an Idempotency-Key".into(),
    })?;
    let token = cart_token(&headers)?;
    let session = header_str(&headers, SESSION_HEADER);
    let hash = idempotency::request_hash(&body);
    let placed = with_ctx(&s, &shopper, async |tx, ctx| {
        let customer_id = match session {
            Some(t) => customers::authenticate(tx, t).await?.map(|s| s.customer_id),
            None => None,
        };
        let ip = ip_hash(tx, &headers).await?;
        let placed = checkout::place_order(
            tx,
            ctx,
            &s.checkout,
            &token,
            &key,
            &hash,
            &input,
            &Placer {
                customer_id,
                ip_hash: ip.as_deref(),
            },
        )
        .await?;
        // WP20: the purchase for the ad platforms, in the placement transaction, only while
        // the visitor's consent subject grants `ads` (checked again when sending).
        if let Some(subject) = header_str(&headers, CONSENT_SUBJECT_HEADER) {
            let ua = header_str(&headers, CLIENT_UA_HEADER);
            commerce::adtracking::capture_purchase(tx, placed.order_id, subject, ua).await?;
        }
        Ok(placed)
    })
    .await?;
    if let Some(subject) = header_str(&headers, CONSENT_SUBJECT_HEADER) {
        link_purchase(&s, shopper.tenant_id, placed.order_id, subject).await;
    }
    let confirmation_url = format!("/o/{}", placed.token);
    let payment = start_payment(&s, shopper.tenant_id, placed.attempt_id, &confirmation_url).await;
    let mut res = (
        StatusCode::CREATED,
        Json(PlacedOrder {
            order_id: placed.order_id,
            number: placed.number,
            confirmation_url,
            payment,
        }),
    )
        .into_response();
    if placed.replayed {
        res.headers_mut()
            .insert(REPLAYED, HeaderValue::from_static("true"));
    }
    Ok(no_store(res))
}

/// A20: a consented visitor's purchase joins their analytics session (the funnel's last
/// step). Best effort after the commit: the order stands regardless, and without an
/// `analytics` grant in the consent records nothing is linked.
async fn link_purchase(s: &AppState, tenant: Uuid, order: Uuid, subject: &str) {
    let linked: Result<bool, Error> = async {
        let mut tx = platform::db::tenant_tx(&s.db, tenant).await?;
        let linked =
            commerce::analytics::link_purchase(&mut tx, order, subject, Utc::now()).await?;
        tx.commit().await?;
        Ok(linked)
    }
    .await;
    if let Err(e) = linked {
        tracing::warn!(error = %e, "linking the purchase to analytics failed");
    }
}

// ---------------------------------------------------------------------------------------
// The fake gateway's "provider page" (`PAYMENTS_FAKE=1` only).

#[derive(Debug, Serialize, ToSchema)]
pub struct FakePayment {
    pub attempt_id: Uuid,
    pub order_number: String,
    pub amount: MoneyView,
    pub status: AttemptStatus,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FakePayInput {
    pub outcome: Outcome,
}

fn attempt_id(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, Error> {
    path.map(|Path(id)| id).map_err(|_| Error::NotFound)
}

/// What the fake pay page shows. `404` unless `PAYMENTS_FAKE=1`.
#[utoipa::path(
    get,
    path = "/storefront/v1/checkout/fake-pay/{attempt}",
    tag = "storefront",
    params(StorefrontHeaders, super::orders::PayerHeaders, ("attempt" = Uuid, Path)),
    responses(
        (status = 200, body = FakePayment),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn fake_pay_page(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    attempt: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = attempt_id(attempt)?;
    if s.checkout.payments.fake.is_none() {
        return Err(Error::NotFound);
    }
    let page = with_ctx(&s, &shopper, async |tx, ctx| {
        let a = payments::attempt(tx, id).await?;
        if a.method != payments::MethodKind::Fake {
            return Err(Error::NotFound);
        }
        // Like any payment: the order token alone is read-only (A4).
        if !super::orders::may_pay(tx, &headers, a.order_id).await? {
            return Err(Error::Forbidden {
                code: "payment_not_allowed",
            });
        }
        let o = commerce::orders::view(tx, a.order_id).await?;
        Ok(FakePayment {
            attempt_id: a.id,
            order_number: o.number,
            amount: commerce::money::Money::new(a.amount_minor, o.currency).view(ctx.fmt_locale()),
            status: a.status,
        })
    })
    .await?;
    Ok(no_store(Json(page).into_response()))
}

/// The fake pay page's Succeed/Fail: the fake provider signs an event and it is processed
/// exactly like `POST /webhooks/fake` (signature, amount and currency checked first).
#[utoipa::path(
    post,
    path = "/storefront/v1/checkout/fake-pay/{attempt}",
    tag = "storefront",
    params(StorefrontHeaders, super::orders::PayerHeaders, ("attempt" = Uuid, Path)),
    request_body = FakePayInput,
    responses(
        (status = 200, body = FakePayment),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_finished", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn fake_pay(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    attempt: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Response, Error> {
    let id = attempt_id(attempt)?;
    let input: FakePayInput = parse_json(&body)?;
    let Some(gateway) = s.checkout.payments.fake.as_ref() else {
        return Err(Error::NotFound);
    };
    let event = with_ctx(&s, &shopper, async |tx, _| {
        let a = payments::attempt(tx, id).await?;
        if a.method != payments::MethodKind::Fake {
            return Err(Error::NotFound);
        }
        // Like any payment: the order token alone is read-only (A4).
        if !super::orders::may_pay(tx, &headers, a.order_id).await? {
            return Err(Error::Forbidden {
                code: "payment_not_allowed",
            });
        }
        Ok(FakeEvent {
            id: Uuid::now_v7(),
            tenant_id: shopper.tenant_id,
            attempt_id: a.id,
            outcome: input.outcome,
            amount_minor: a.amount_minor,
            currency: a.currency,
        })
    })
    .await?;
    let raw = serde_json::to_vec(&event).map_err(|e| Error::Internal(e.to_string()))?;
    let signature = gateway.sign(&raw, Utc::now())?;
    crate::webhooks::fake_event(&s, &signature, &raw).await?;
    fake_pay_page(shopper, State(s), headers, Ok(Path(id))).await
}
