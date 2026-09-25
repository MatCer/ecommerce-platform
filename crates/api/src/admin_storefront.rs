//! Storefront Admin API: redirects (spec §7.5, §9.5) and the public storefront token (§5.5).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use commerce::redirects::{self, Redirect, RedirectInput, RedirectPage};
use commerce::tenancy::{self, Role};
use platform::Error;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
    query_params,
};
use crate::admin_promotions::PageQuery;
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_redirects, create_redirect))
        .routes(routes!(get_redirect, update_redirect, delete_redirect))
        .routes(routes!(get_token))
        .routes(routes!(rotate_token))
}

/// Redirects, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/redirects",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, PageQuery),
    responses((status = 200, body = RedirectPage))
)]
async fn list_redirects(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<RedirectPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            redirects::list(tx, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// Creates a redirect from a path to another path on the same shop (301 by default).
/// Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/redirects",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = RedirectInput,
    responses(
        (status = 201, body = Redirect),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_redirect(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: RedirectInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/redirects",
        &input,
        async |tx| redirects::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/redirects/{id}",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Redirect),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_redirect(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Redirect>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| redirects::get(tx, id).await).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/admin/v1/redirects/{id}",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = RedirectInput,
    responses(
        (status = 200, body = Redirect),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_redirect(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Redirect>, Error> {
    let id = path_id(id)?;
    let input: RedirectInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            redirects::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/redirects/{id}",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_redirect(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        redirects::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct StorefrontToken {
    /// Public storefront token of the tenant (the edge sends it; it grants only public reads
    /// and cart operations of this tenant).
    pub token: String,
}

/// The current storefront token (owner/admin).
#[utoipa::path(
    get,
    path = "/admin/v1/storefront-token",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = StorefrontToken))
)]
async fn get_token(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<StorefrontToken>, Error> {
    staff.require(Role::Admin)?;
    let token = in_tx(&s, staff.tenant_id, async |tx| {
        tenancy::storefront_token(tx).await
    })
    .await?;
    Ok(Json(StorefrontToken { token }))
}

/// Issues a new storefront token (owner/admin, login at most 15 minutes old). The previous
/// token keeps working for 5 minutes while the edge picks up the new one.
#[utoipa::path(
    post,
    path = "/admin/v1/storefront-token/rotate",
    tag = "storefront-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = StorefrontToken),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn rotate_token(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<StorefrontToken>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let actor = &staff.user.user_id;
    let token = in_tx(&s, staff.tenant_id, async |tx| {
        tenancy::rotate_storefront_token(tx, actor).await
    })
    .await?;
    s.edge.tenant(staff.tenant_id).await;
    Ok(Json(StorefrontToken { token }))
}
