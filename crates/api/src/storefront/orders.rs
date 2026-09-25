//! The order page (spec §9.4, A4, A10): `checkout.<shop>/o/<token>` and its payment status
//! and retries. The order capability token (read-only, 90 days) is the only credential: the
//! page works from the confirmation email in any browser. The edge maps
//! `/_p/orders/<token>/*` here.

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use commerce::checkout;
use commerce::orders::{self, OrderView, PaymentView};
use commerce::payments;
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::checkout::{PaymentStart, start_payment};
use super::customer::no_store;
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_order))
        .routes(routes!(get_payment))
        .routes(routes!(new_attempt))
        .routes(routes!(init_attempt))
}

fn token(path: Result<Path<String>, PathRejection>) -> Result<String, Error> {
    path.map(|Path(t)| t).map_err(|_| Error::NotFound)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 200, body = OrderView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_order(
    shopper: Shopper,
    State(s): State<AppState>,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, _| {
        checkout::order_by_token(tx, &t).await
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

/// The payment status (A10), polled by the order page while a payment is under way.
#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}/payment",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 200, body = PaymentView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_payment(
    shopper: Shopper,
    State(s): State<AppState>,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let view = with_ctx(&s, &shopper, async |tx, _| {
        Ok(checkout::order_by_token(tx, &t).await?.payment)
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

/// A new payment attempt after a failed one, within the order's payment window (A10).
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 201, body = PaymentStart),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "retry_not_allowed", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn new_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    token_path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(token_path)?;
    let attempt = with_ctx(&s, &shopper, async |tx, _| {
        let order = orders::by_token(tx, &t).await?;
        payments::retry(tx, &s.checkout.payments, order).await
    })
    .await?;
    let start = start_payment(&s, shopper.tenant_id, attempt, &format!("/o/{t}")).await;
    Ok(no_store((StatusCode::CREATED, Json(start)).into_response()))
}

/// Initializes (again) a pending attempt at its provider (A10): idempotent, for when the
/// automatic init after placement failed.
#[utoipa::path(
    post,
    path = "/storefront/v1/orders/{token}/payment-attempts/{attempt}/init",
    tag = "storefront",
    params(
        StorefrontHeaders,
        ("token" = String, Path, description = "Order capability token"),
        ("attempt" = Uuid, Path),
    ),
    responses(
        (status = 200, body = PaymentStart),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_not_pending", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn init_attempt(
    shopper: Shopper,
    State(s): State<AppState>,
    path: Result<Path<(String, Uuid)>, PathRejection>,
) -> Result<Response, Error> {
    let Path((t, attempt)) = path.map_err(|_| Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, _| {
        let order = orders::by_token(tx, &t).await?;
        if payments::attempt(tx, attempt).await?.order_id != order {
            return Err(Error::NotFound);
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
