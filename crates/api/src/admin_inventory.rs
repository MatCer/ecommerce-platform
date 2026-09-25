//! Inventory Admin API (spec §8.3, A13): stock levels, settings, manual adjustments and the
//! movement ledger. Staff role and up. Reservations/commits come from orders (WP10+).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use commerce::inventory::{self, Adjustment, Level, LevelPage, LevelSettings, MovementPage};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdempotencyHeader, TenantHeader, create_idempotent, idempotency_key, in_tx, parse_json,
    query_params,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_levels))
        .routes(routes!(update_settings))
        .routes(routes!(adjust))
        .routes(routes!(list_movements))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LevelQuery {
    /// Only this product's variants.
    pub product_id: Option<Uuid>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// Stock of every variant (defaults where nothing was recorded yet), by variant id.
#[utoipa::path(
    get,
    path = "/admin/v1/inventory",
    tag = "inventory",
    security(("staff_jwt" = [])),
    params(TenantHeader, LevelQuery),
    responses((status = 200, body = LevelPage))
)]
async fn list_levels(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<LevelQuery>, QueryRejection>,
) -> Result<Json<LevelPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            inventory::list(tx, q.product_id, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// `{variant_id}` path parameter (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[allow(dead_code)]
struct VariantPath {
    variant_id: Uuid,
}

fn variant_id(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, Error> {
    path.map(|Path(id)| id).map_err(|_| Error::NotFound)
}

/// Whether the variant's stock is tracked and may be sold below zero.
#[utoipa::path(
    put,
    path = "/admin/v1/inventory/{variant_id}",
    tag = "inventory",
    security(("staff_jwt" = [])),
    params(TenantHeader, VariantPath),
    request_body = LevelSettings,
    responses(
        (status = 200, body = Level),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json",
         description = "`below_reserved`: stock is oversold"),
    )
)]
async fn update_settings(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Level>, Error> {
    let id = variant_id(path)?;
    let input: LevelSettings = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            inventory::update_settings(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[derive(Serialize, ToSchema)]
pub struct AdjustmentResult {
    pub level: Level,
    /// False when this `Idempotency-Key` was already applied (or the change was zero).
    pub applied: bool,
}

/// A manual stock correction: `delta` units, or the counted `on_hand`. Honors
/// `Idempotency-Key` (A12): a retry replays the first response and the key is also the
/// movement's identity, so a correction is never applied twice.
#[utoipa::path(
    post,
    path = "/admin/v1/inventory/{variant_id}/adjustments",
    tag = "inventory",
    security(("staff_jwt" = [])),
    params(TenantHeader, VariantPath, IdempotencyHeader),
    request_body = Adjustment,
    responses(
        (status = 201, body = AdjustmentResult),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json",
         description = "`below_reserved`, `idempotency_conflict`"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn adjust(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let id = variant_id(path)?;
    let input: Adjustment = parse_json(&body)?;
    let ref_id = idempotency_key(&headers)?.unwrap_or_else(|| commerce::id::new_id().to_string());
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/inventory/adjustments",
        &(id, &input),
        async |tx| {
            let moved = inventory::adjust(tx, &actor, id, &ref_id, &input).await?;
            Ok(AdjustmentResult {
                level: moved.level,
                applied: moved.applied,
            })
        },
    )
    .await
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MovementQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// The variant's stock movements, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/inventory/{variant_id}/movements",
    tag = "inventory",
    security(("staff_jwt" = [])),
    params(TenantHeader, VariantPath, MovementQuery),
    responses(
        (status = 200, body = MovementPage),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_movements(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<MovementQuery>, QueryRejection>,
) -> Result<Json<MovementPage>, Error> {
    let id = variant_id(path)?;
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            inventory::movements(tx, id, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}
