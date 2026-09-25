//! Feeds Admin API (spec §10.8, A28): feed imports (dry run, apply, history), export feed
//! status and regeneration, and tenant search synonyms (WP7 follow-up).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::Utc;
use commerce::feeds::export::{self, FeedFileList};
use commerce::feeds::import::{self, CreatedImport, ImportRun, ImportRunList, NewImport};
use commerce::search::synonyms::{self, Synonyms, SynonymsView};
use commerce::tenancy::Role;
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_imports, create_import))
        .routes(routes!(get_import))
        .routes(routes!(analyze_import))
        .routes(routes!(apply_import))
        .routes(routes!(list_feeds))
        .routes(routes!(regenerate_feeds))
        .routes(routes!(get_synonyms, put_synonyms))
}

/// The 50 most recent imports.
#[utoipa::path(
    get,
    path = "/admin/v1/imports",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = ImportRunList))
)]
async fn list_imports(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<ImportRunList>, Error> {
    staff.require(Role::Admin)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| import::list(tx).await).await?,
    ))
}

/// Starts a feed import (owner/admin): from a URL (downloaded through the SSRF-safe client and
/// analyzed right away) or an upload (PUT the file to `upload`, then call `analyze`). Nothing
/// is written to the catalog until `apply`. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/imports",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewImport,
    responses(
        (status = 201, body = CreatedImport),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewImport = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    let storage = s.storage.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/imports",
        &input,
        async |tx| import::create(tx, &storage, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/imports/{id}",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = ImportRun),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ImportRun>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| import::get(tx, id).await).await?,
    ))
}

/// Runs (or re-runs) the dry run: counts, missing fields, collisions. Writes nothing.
#[utoipa::path(
    post,
    path = "/admin/v1/imports/{id}/analyze",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 202, body = ImportRun),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn analyze_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<(StatusCode, Json<ImportRun>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    let run = in_tx(&s, staff.tenant_id, async |tx| {
        import::analyze(tx, actor, id).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Applies an analyzed import in the background (products as drafts, prices without
/// reduction claims, images through the media pipeline, redirects from old URLs).
#[utoipa::path(
    post,
    path = "/admin/v1/imports/{id}/apply",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 202, body = ImportRun),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn apply_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<(StatusCode, Json<ImportRun>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    let run = in_tx(&s, staff.tenant_id, async |tx| {
        import::apply(tx, actor, id).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Export feeds per market and channel (Google, Heureka, Zboží) with their public URLs.
#[utoipa::path(
    get,
    path = "/admin/v1/feeds",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = FeedFileList))
)]
async fn list_feeds(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<FeedFileList>, Error> {
    let urls = s.public_urls.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            export::list(tx, &urls).await
        })
        .await?,
    ))
}

/// Regenerates the export feeds now (they also refresh hourly and after catalog changes).
#[utoipa::path(
    post,
    path = "/admin/v1/feeds/regenerate",
    tag = "feeds",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 202, description = "Queued"))
)]
async fn regenerate_feeds(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<StatusCode, Error> {
    let tenant = staff.tenant_id;
    in_tx(&s, tenant, async |tx| {
        platform::queue::enqueue(&mut **tx, &export::job(tenant, Utc::now(), false)).await?;
        Ok(())
    })
    .await?;
    Ok(StatusCode::ACCEPTED)
}

/// The tenant's search synonyms as entered.
#[utoipa::path(
    get,
    path = "/admin/v1/search/synonyms",
    tag = "search",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = SynonymsView))
)]
async fn get_synonyms(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<SynonymsView>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| synonyms::get(tx).await).await?,
    ))
}

/// Replaces the synonyms (owner/admin); the worker applies them to every search index within
/// seconds.
#[utoipa::path(
    put,
    path = "/admin/v1/search/synonyms",
    tag = "search",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = Synonyms,
    responses(
        (status = 200, body = SynonymsView),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_synonyms(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<SynonymsView>, Error> {
    staff.require(Role::Admin)?;
    let input: Synonyms = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            synonyms::put(tx, actor, &input).await
        })
        .await?,
    ))
}
