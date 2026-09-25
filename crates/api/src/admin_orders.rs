//! Checkout settings and orders in the Admin API (spec §8.3, §10.4-10.6): shipping methods
//! per market, payment methods per market (payment settings: admin role and a fresh login,
//! A9), and the read-only order list and detail (order management is WP12).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use commerce::orders::{self, AdminOrder, OrderPage, status::OrderStatus};
use commerce::payments::{self, MethodKind, PaymentMethod, PaymentMethodInput};
use commerce::shipping::{self, ShippingMethod, ShippingMethodInput};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
    query_params,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_shipping, create_shipping))
        .routes(routes!(get_shipping, update_shipping, delete_shipping))
        .routes(routes!(list_payment_methods))
        .routes(routes!(configure_payment_method))
        .routes(routes!(list_orders))
        .routes(routes!(get_order))
}

// ---------------------------------------------------------------------------------------
// Shipping methods

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MarketQuery {
    /// Only this market's methods.
    pub market_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ShippingMethodList {
    pub items: Vec<ShippingMethod>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/shipping-methods",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, MarketQuery),
    responses((status = 200, body = ShippingMethodList))
)]
async fn list_shipping(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<MarketQuery>, QueryRejection>,
) -> Result<Json<ShippingMethodList>, Error> {
    let q = query_params(query)?;
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        shipping::list(tx, q.market_id).await
    })
    .await?;
    Ok(Json(ShippingMethodList { items }))
}

/// Creates a shipping method (admin). Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/shipping-methods",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = ShippingMethodInput,
    responses(
        (status = 201, body = ShippingMethod),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_shipping(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: ShippingMethodInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/shipping-methods",
        &input,
        async |tx| shipping::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/shipping-methods/{id}",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = ShippingMethod),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_shipping(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ShippingMethod>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| shipping::get(tx, id).await).await?,
    ))
}

/// Replaces a shipping method (admin). The market cannot change.
#[utoipa::path(
    put,
    path = "/admin/v1/shipping-methods/{id}",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ShippingMethodInput,
    responses(
        (status = 200, body = ShippingMethod),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_shipping(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<ShippingMethod>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: ShippingMethodInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            shipping::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes a shipping method (admin). Orders keep their snapshot of it.
#[utoipa::path(
    delete,
    path = "/admin/v1/shipping-methods/{id}",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_shipping(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        shipping::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Payment methods

#[derive(Serialize, ToSchema)]
pub struct PaymentMethodList {
    pub items: Vec<PaymentMethod>,
}

/// Every payment method kind of a market with its configuration and whether the platform
/// can take payments with it yet (`available`: Stripe and bank transfer arrive with WP11).
#[utoipa::path(
    get,
    path = "/admin/v1/markets/{id}/payment-methods",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses((status = 200, body = PaymentMethodList))
)]
async fn list_payment_methods(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<PaymentMethodList>, Error> {
    let market = path_id(id)?;
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        payments::methods(tx, &s.checkout.payments, market).await
    })
    .await?;
    Ok(Json(PaymentMethodList { items }))
}

/// Configures a payment method of a market (payment settings: admin role and a login within
/// the last 15 minutes, else `401 reauth_required`, A9).
#[utoipa::path(
    put,
    path = "/admin/v1/markets/{id}/payment-methods/{kind}",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam, ("kind" = MethodKind, Path)),
    request_body = PaymentMethodInput,
    responses(
        (status = 200, body = PaymentMethod),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn configure_payment_method(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<(Uuid, MethodKind)>, PathRejection>,
    body: Bytes,
) -> Result<Json<PaymentMethod>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let Path((market, kind)) = path.map_err(|_| Error::NotFound)?;
    let input: PaymentMethodInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            payments::configure(tx, actor, &s.checkout.payments, market, kind, &input).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Orders (read-only in WP10)

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OrderQuery {
    pub status: Option<OrderStatus>,
    pub customer_id: Option<Uuid>,
    /// Only orders with an exception (money to refund, A10).
    pub exception: Option<bool>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// Orders, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/orders",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, OrderQuery),
    responses((status = 200, body = OrderPage))
)]
async fn list_orders(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<OrderQuery>, QueryRejection>,
) -> Result<Json<OrderPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            let filter = orders::OrderFilter {
                customer_id: q.customer_id,
                status: q.status,
                exception: q.exception.unwrap_or(false),
            };
            orders::list(tx, &filter, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// An order with its lines, totals, addresses, payment attempts and timeline.
#[utoipa::path(
    get,
    path = "/admin/v1/orders/{id}",
    tag = "checkout",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = AdminOrder),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_order(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<AdminOrder>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            orders::admin_detail(tx, id).await
        })
        .await?,
    ))
}
