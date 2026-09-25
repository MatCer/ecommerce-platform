//! Promotions Admin API (spec §8.3, §10.2): sales (automatic discounts, recorded in the price
//! intervals incl. future schedules) and coupons. Staff role and up.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use commerce::promotions::coupons::{self, Coupon, CouponInput, CouponPage};
use commerce::promotions::sales::{self, Sale, SaleInput, SalePage};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
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
        .routes(routes!(list_sales, create_sale))
        .routes(routes!(get_sale, update_sale, delete_sale))
        .routes(routes!(list_coupons, create_coupon))
        .routes(routes!(get_coupon, update_coupon, delete_coupon))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------------------
// Sales

/// Sales, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/sales",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, PageQuery),
    responses((status = 200, body = SalePage))
)]
async fn list_sales(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<SalePage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            sales::list(tx, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// Creates a sale, optionally scheduled in the future. The price timelines of the targeted
/// variants get the scheduled start and end. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/sales",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = SaleInput,
    responses(
        (status = 201, body = Sale),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_sale(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: SaleInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/sales",
        &input,
        async |tx| sales::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/sales/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Sale),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_sale(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Sale>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| sales::get(tx, id).await).await?,
    ))
}

/// Replaces a sale. A running sale keeps its start and discount (`422 sale_started`).
#[utoipa::path(
    put,
    path = "/admin/v1/sales/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = SaleInput,
    responses(
        (status = 200, body = Sale),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_sale(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Sale>, Error> {
    let id = path_id(id)?;
    let input: SaleInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            sales::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes a sale; a running one ends now (its past price intervals remain as history).
#[utoipa::path(
    delete,
    path = "/admin/v1/sales/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_sale(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        sales::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Coupons

/// Coupons, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/coupons",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, PageQuery),
    responses((status = 200, body = CouponPage))
)]
async fn list_coupons(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<CouponPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            coupons::list(tx, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// Creates a coupon. `published` coupons count as price reductions for the Omnibus
/// reference. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/coupons",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = CouponInput,
    responses(
        (status = 201, body = Coupon),
        (status = 409, body = platform::Problem, content_type = "application/problem+json",
         description = "`code_taken`"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_coupon(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: CouponInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/coupons",
        &input,
        async |tx| coupons::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/coupons/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Coupon),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_coupon(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Coupon>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| coupons::get(tx, id).await).await?,
    ))
}

/// Replaces a coupon. A started published coupon only accepts a new end and limits
/// (`409 coupon_started`).
#[utoipa::path(
    put,
    path = "/admin/v1/coupons/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CouponInput,
    responses(
        (status = 200, body = Coupon),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_coupon(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Coupon>, Error> {
    let id = path_id(id)?;
    let input: CouponInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            coupons::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes an unused coupon (`409 coupon_in_use` once redeemed or advertised).
#[utoipa::path(
    delete,
    path = "/admin/v1/coupons/{id}",
    tag = "promotions",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_coupon(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        coupons::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
