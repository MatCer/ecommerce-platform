//! The order page (spec §9.4, A4, A10): `checkout.<shop>/o/<token>` and its payment status
//! and retries. The order capability token (90 days) is a **read-only** credential: the page
//! works from the confirmation email in any browser. Paying (a new attempt, provider init)
//! additionally needs the checkout cart capability the order was placed with (the same
//! browser) or the session of the order's customer. The edge maps `/_p/orders/<token>/*` here
//! and forwards both.

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::checkout;
use commerce::customers;
use commerce::orders::{self, OrderView, PaymentView};
use commerce::payments;
use platform::Error;
use platform::db::TenantTx;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::checkout::{PaymentStart, start_payment};
use super::customer::{SESSION_HEADER, header_str, no_store};
use super::{CART_HEADER, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_order))
        .routes(routes!(get_payment))
        .routes(routes!(new_attempt))
        .routes(routes!(init_attempt))
}

/// What proves the right to pay (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct PayerHeaders {
    /// The checkout cart capability the order was placed with (`__Host-cart`).
    #[param(rename = "X-Cart-Token")]
    x_cart_token: Option<String>,
    /// The session of the order's customer (`__Host-sid`).
    #[param(rename = "X-Customer-Session")]
    x_customer_session: Option<String>,
}

fn token(path: Result<Path<String>, PathRejection>) -> Result<String, Error> {
    path.map(|Path(t)| t).map_err(|_| Error::NotFound)
}

/// Whether the requester may pay `order` (see the module docs).
pub(super) async fn may_pay(
    tx: &mut TenantTx,
    headers: &HeaderMap,
    order: Uuid,
) -> Result<bool, Error> {
    let customer = match header_str(headers, SESSION_HEADER) {
        Some(t) => customers::authenticate(tx, t).await?.map(|s| s.customer_id),
        None => None,
    };
    orders::may_pay(tx, order, header_str(headers, CART_HEADER), customer).await
}

/// The order behind the token, its payment view narrowed to what this requester may do.
async fn read(tx: &mut TenantTx, headers: &HeaderMap, t: &str) -> Result<OrderView, Error> {
    let mut view = checkout::order_by_token(tx, t).await?;
    let allowed = may_pay(tx, headers, view.id).await?;
    view.payment.can_pay = allowed;
    view.payment.can_retry &= allowed;
    Ok(view)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 200, body = OrderView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_order(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, _| read(tx, &headers, &t).await).await?;
    Ok(no_store(Json(view).into_response()))
}

/// The payment status (A10), polled by the order page while a payment is under way.
#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}/payment",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 200, body = PaymentView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_payment(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, _| {
        Ok(read(tx, &headers, &t).await?.payment)
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

fn not_allowed() -> Error {
    Error::Forbidden {
        code: "payment_not_allowed",
    }
}

/// A new payment attempt after a failed one, within the order's payment window (A10). Needs
/// the right to pay (`403 payment_not_allowed`).
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts",
    tag = "storefront",
    params(StorefrontHeaders, PayerHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 201, body = PaymentStart),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "retry_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn new_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let attempt = with_ctx(&s, &shopper, async |tx, _| {
        let order = orders::by_token(tx, &t).await?;
        if !may_pay(tx, &headers, order).await? {
            return Err(not_allowed());
        }
        payments::retry(tx, &s.checkout.payments, order).await
    })
    .await?;
    let start = start_payment(&s, shopper.tenant_id, attempt, &format!("/o/{t}")).await;
    Ok(no_store((StatusCode::CREATED, Json(start)).into_response()))
}

/// Initializes (again) a pending attempt at its provider (A10): idempotent, for when the
/// automatic init after placement failed or the customer comes back to pay. Needs the right
/// to pay; `409 payment_window_closed` after the order's payment deadline.
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts/{attempt}/init",
    tag = "storefront",
    params(
        StorefrontHeaders,
        PayerHeaders,
        ("token" = String, Path, description = "Order capability token"),
        ("attempt" = Uuid, Path),
    ),
    responses(
        (status = 200, body = PaymentStart),
        (status = 403, description = "payment_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_not_pending | payment_window_closed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn init_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(String, Uuid)>, PathRejection>,
) -> Result<Response, Error> {
    let Path((t, attempt)) = path.map_err(|_| Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, _| {
        let order = orders::by_token(tx, &t).await?;
        if payments::attempt(tx, attempt).await?.order_id != order {
            return Err(Error::NotFound);
        }
        if !may_pay(tx, &headers, order).await? {
            return Err(not_allowed());
        }
        Ok(())
    })
    .await?;
    let action = payments::init(
        &s.db,
        shopper.tenant_id,
        &s.checkout.payments,
        attempt,
        &format!("/o/{t}"),
    )
    .await?;
    Ok(no_store(
        Json(PaymentStart {
            attempt_id: attempt,
            action: Some(action),
        })
        .into_response(),
    ))
}
