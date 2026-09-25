//! Media Admin API (spec §8.3, A21): `POST /assets/uploads` -> presigned PUT into the private
//! bucket -> `POST /assets/{id}/complete` -> the worker publishes variants to the public bucket.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use commerce::media::{self, Asset, AssetPage, AssetStatus, NewUpload, Upload};
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
        .routes(routes!(create_upload))
        .routes(routes!(complete_upload))
        .routes(routes!(list_assets))
        .routes(routes!(get_asset, delete_asset))
}

/// Starts an image upload: returns a pending asset and a presigned `PUT` URL (15 minutes)
/// into private storage. Upload the file there with the returned headers, then call
/// `complete`. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/assets/uploads",
    tag = "media",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewUpload,
    responses(
        (status = 201, body = Upload),
        (status = 422, body = platform::Problem, content_type = "application/problem+json",
         description = "`unsupported_type`, `file_too_large`"),
    )
)]
async fn create_upload(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewUpload = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    let storage = s.storage.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/assets/uploads",
        &input,
        async |tx| media::create_upload(tx, &storage, &actor, &input).await,
    )
    .await
}

/// Verifies the uploaded file (size, sniffed type, dimensions) and queues the variants.
/// A rejected file is deleted and the asset stays `pending` (upload again, then retry).
/// Calling it again on a processing or ready asset returns it unchanged.
#[utoipa::path(
    post,
    path = "/admin/v1/assets/{id}/complete",
    tag = "media",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Asset),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json",
         description = "`upload_missing`, `asset_failed`"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json",
         description = "`unsupported_type`, `file_too_large`, `image_too_large`, `corrupt_image`, `size_mismatch`"),
    )
)]
async fn complete_upload(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Asset>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            media::complete(tx, &s.storage, actor, id).await
        })
        .await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssetQuery {
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
    pub status: Option<AssetStatus>,
}

/// Assets, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/assets",
    tag = "media",
    security(("staff_jwt" = [])),
    params(TenantHeader, AssetQuery),
    responses(
        (status = 200, body = AssetPage),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_assets(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<AssetQuery>, QueryRejection>,
) -> Result<Json<AssetPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            media::list(tx, &s.storage, q.status, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// An asset with its public variant URLs once `ready`.
#[utoipa::path(
    get,
    path = "/admin/v1/assets/{id}",
    tag = "media",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Asset),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_asset(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Asset>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            media::get(tx, &s.storage, id).await
        })
        .await?,
    ))
}

/// Deletes an asset no product uses (`409 asset_in_use`); its files are removed afterwards.
#[utoipa::path(
    delete,
    path = "/admin/v1/assets/{id}",
    tag = "media",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_asset(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        media::delete(tx, &s.storage, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
