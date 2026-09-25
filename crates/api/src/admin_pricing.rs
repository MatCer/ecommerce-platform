//! Pricing Admin API (spec §8.3, §10.1, A3, A18): tax profile, price lists, variant prices and
//! a product's price history with its Omnibus reference. Logic lives in `commerce::tax` and
//! `commerce::pricing`; every mutation is audited there.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::{DateTime, Utc};
use commerce::pricing::{
    self, NewPriceList, PriceList, PriceListUpdate, PriceUpsert, VariantPrice, VariantPriceHistory,
    VariantPricePage,
};
use commerce::tax::{self, TaxProfile, TaxProfileInput};
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
        .routes(routes!(get_tax_profile, put_tax_profile))
        .routes(routes!(list_price_lists, create_price_list))
        .routes(routes!(get_price_list, update_price_list))
        .routes(routes!(list_prices, upsert_prices))
        .routes(routes!(delete_price))
        .routes(routes!(price_history))
}

// ---------------------------------------------------------------------------------------
// Tax profile

/// The tenant's VAT setup (A3). `404` until it has been set.
#[utoipa::path(
    get,
    path = "/admin/v1/tax-profile",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = TaxProfile),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_tax_profile(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<TaxProfile>, Error> {
    in_tx(&s, staff.tenant_id, async |tx| tax::get(tx).await)
        .await?
        .map(Json)
        .ok_or(Error::NotFound)
}

/// Creates or replaces the VAT setup (owner/admin, login at most 15 minutes old, else
/// `401 reauth_required`). Switching to `origin_threshold` needs `confirm_origin_threshold`.
/// The configuration must be confirmed by the merchant's accountant.
#[utoipa::path(
    put,
    path = "/admin/v1/tax-profile",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = TaxProfileInput,
    responses(
        (status = 200, body = TaxProfile),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_tax_profile(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<TaxProfile>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let input: TaxProfileInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            tax::upsert(tx, actor, &input).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Price lists

#[derive(Serialize, ToSchema)]
pub struct PriceListList {
    pub items: Vec<PriceList>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/price-lists",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = PriceListList))
)]
async fn list_price_lists(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<PriceListList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        pricing::list_price_lists(tx).await
    })
    .await?;
    Ok(Json(PriceListList { items }))
}

/// Creates a price list (owner/admin) and optionally assigns markets of the same currency.
/// Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/price-lists",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewPriceList,
    responses(
        (status = 201, body = PriceList),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json",
         description = "`currency_mismatch`, `unknown_market`, validation"),
    )
)]
async fn create_price_list(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewPriceList = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/price-lists",
        &input,
        async |tx| pricing::create_price_list(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/price-lists/{id}",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = PriceList),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_price_list(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<PriceList>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            pricing::get_price_list(tx, id).await
        })
        .await?,
    ))
}

/// Renames the list and sets the complete set of markets using it (owner/admin).
#[utoipa::path(
    put,
    path = "/admin/v1/price-lists/{id}",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = PriceListUpdate,
    responses(
        (status = 200, body = PriceList),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_price_list(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<PriceList>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: PriceListUpdate = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            pricing::update_price_list(tx, actor, id, &input).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Variant prices

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PriceQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-1000 (default 100).
    pub limit: Option<i64>,
}

/// Gross base prices of a list, by variant id.
#[utoipa::path(
    get,
    path = "/admin/v1/price-lists/{id}/prices",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam, PriceQuery),
    responses(
        (status = 200, body = VariantPricePage),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_prices(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<PriceQuery>, QueryRejection>,
) -> Result<Json<VariantPricePage>, Error> {
    let id = path_id(id)?;
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            pricing::list_prices(tx, id, q.cursor, q.limit.unwrap_or(100)).await
        })
        .await?,
    ))
}

#[derive(Serialize, ToSchema)]
pub struct VariantPriceList {
    pub items: Vec<VariantPrice>,
}

/// Bulk upsert of up to 1000 gross base prices. The effective-price timeline (incl. running
/// and scheduled sales) is recomputed and `price.changed` published for changed prices.
/// `reason: tax` marks a VAT-driven repricing; `imported: true` marks prices whose history is
/// unknown (no Omnibus reduction claims for 30 days).
#[utoipa::path(
    put,
    path = "/admin/v1/price-lists/{id}/prices",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = PriceUpsert,
    responses(
        (status = 200, body = VariantPriceList),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn upsert_prices(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<VariantPriceList>, Error> {
    let id = path_id(id)?;
    let input: PriceUpsert = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        pricing::upsert_prices(tx, actor, id, &input).await
    })
    .await?;
    Ok(Json(VariantPriceList { items }))
}

/// `{id}` + `{variant_id}` path parameters (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[allow(dead_code)]
struct PricePath {
    id: Uuid,
    variant_id: Uuid,
}

/// Stops selling a variant from this list (from now on; history stays).
#[utoipa::path(
    delete,
    path = "/admin/v1/price-lists/{id}/prices/{variant_id}",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, PricePath),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_price(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<StatusCode, Error> {
    let (id, variant_id) = path.map(|Path(p)| p).map_err(|_| Error::NotFound)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        pricing::delete_price(tx, actor, id, variant_id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Price history

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    /// Only this price list.
    pub price_list_id: Option<Uuid>,
    /// Evaluate the current price and Omnibus reference at this time (default now), e.g. the
    /// start of a scheduled sale.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct PriceHistory {
    pub items: Vec<VariantPriceHistory>,
}

/// Effective-price intervals (base, sale, tax; including scheduled future changes) per
/// variant and price list, with the Omnibus reference price (A18).
#[utoipa::path(
    get,
    path = "/admin/v1/products/{id}/price-history",
    tag = "pricing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam, HistoryQuery),
    responses(
        (status = 200, body = PriceHistory),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn price_history(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Result<Json<PriceHistory>, Error> {
    let id = path_id(id)?;
    let q = query_params(query)?;
    let at = q.at.unwrap_or_else(Utc::now);
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        pricing::price_history(tx, id, q.price_list_id, at).await
    })
    .await?;
    Ok(Json(PriceHistory { items }))
}
