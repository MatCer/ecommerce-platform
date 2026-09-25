//! Internal API (`/internal/v1`, spec §8.4) for platform services holding the service token.
//! Not routed by Caddy: callers reach the API on the internal network.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use commerce::tenancy::{self, Resolved};
use commerce::themes;
use object_store::ObjectStoreExt;
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::{BuilderService, Service, TENANT_HEADER};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(resolve))
        .routes(routes!(counters))
        .routes(routes!(preview_resolve))
        .routes(routes!(build_spec))
        .routes(routes!(build_source))
        .routes(routes!(build_status))
        .routes(routes!(build_artifact))
        .routes(routes!(build_screenshot))
        // A wildcard path: registered on axum directly (utoipa paths cannot express `{*path}`).
        .route("/internal/v1/artifacts/{id}/{*path}", get(artifact_file))
}

/// Page request and search counters flushed by the edge (A20: no identifiers).
#[utoipa::path(
    post,
    path = "/internal/v1/analytics/counters",
    tag = "internal",
    security(("service_token" = [])),
    request_body = commerce::analytics::CounterBatch,
    responses(
        (status = 200, body = commerce::analytics::CountersRecorded),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn counters(
    _service: Service,
    State(s): State<AppState>,
    body: axum::body::Bytes,
) -> Result<Json<commerce::analytics::CountersRecorded>, Error> {
    let batch: commerce::analytics::CounterBatch = crate::admin::parse_json(&body)?;
    Ok(Json(
        commerce::analytics::record_counters(&s.db, &batch).await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ResolveQuery {
    /// Request hostname, optionally with a port (`demo.localhost:8080`).
    pub host: String,
}

/// Hostname -> tenant and market, for verified domains of active tenants.
#[utoipa::path(
    get,
    path = "/internal/v1/resolve",
    tag = "internal",
    security(("service_token" = [])),
    params(ResolveQuery),
    responses(
        (status = 200, body = Resolved),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn resolve(
    _service: Service,
    State(s): State<AppState>,
    query: Result<Query<ResolveQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Resolved>, Error> {
    let Query(q) = query.map_err(|e| Error::BadRequest {
        code: "invalid_query",
        detail: e.body_text(),
    })?;
    tenancy::resolve_host(&s.db, &q.host)
        .await?
        .map(Json)
        .ok_or(Error::NotFound)
}

/// A file of a registered theme/checkout artifact (A22), for the edge to unpack locally. The
/// edge verifies the content address of what it downloads, so this only has to serve bytes.
/// `GET /internal/v1/artifacts/{id}/{path}` (service token).
async fn artifact_file(
    _service: Service,
    State(s): State<AppState>,
    path: Result<Path<(String, String)>, axum::extract::rejection::PathRejection>,
) -> Result<Response, Error> {
    let Path((id, file)) = path.map_err(|_| Error::NotFound)?;
    if !themes::artifact_id_valid(&id)
        || !themes::artifact_path_valid(&file)
        || !themes::artifact_exists(&s.db, &id).await?
    {
        return Err(Error::NotFound);
    }
    let object = match s.storage.private.get(&themes::object_key(&id, &file)).await {
        Ok(r) => r,
        Err(object_store::Error::NotFound { .. }) => return Err(Error::NotFound),
        Err(e) => return Err(e.into()),
    };
    // Registration caps artifacts; a larger object is not ours to serve. Streamed, not buffered.
    if object.meta.size > themes::MAX_ARTIFACT_BYTES as u64 {
        return Err(Error::Internal(format!(
            "artifact object {id}/{file} is too large"
        )));
    }
    let mut res = axum::body::Body::from_stream(object.into_stream()).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // Content-addressed: never changes.
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(res)
}

// ---------------------------------------------------------------------------------------
// Theme previews (edge) and builder callbacks (WP23)

fn theme_keys(s: &AppState) -> Result<&themes::ThemeKeys, Error> {
    s.themes
        .as_ref()
        .ok_or_else(|| Error::Unavailable("theme builder is not configured (THEME_SECRET)".into()))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PreviewQuery {
    /// `preview-<n>--<shop host>` (port allowed).
    pub host: String,
    /// The preview token from the admin's link (A21).
    pub token: String,
}

/// A preview host + token → the shop's site with the previewed revision's artifact (A21).
/// 404 for anything not authentic, expired, or not matching the host's tenant and revision.
#[utoipa::path(
    get,
    path = "/internal/v1/previews/resolve",
    tag = "internal",
    security(("service_token" = [])),
    params(PreviewQuery),
    responses(
        (status = 200, body = themes::PreviewSite),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preview_resolve(
    _service: Service,
    State(s): State<AppState>,
    query: Result<Query<PreviewQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<themes::PreviewSite>, Error> {
    let Query(q) = query.map_err(|e| Error::BadRequest {
        code: "invalid_query",
        detail: e.body_text(),
    })?;
    let host = q
        .host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    themes::resolve_preview(&s.db, theme_keys(&s)?, &host, &q.token, chrono::Utc::now())
        .await?
        .map(Json)
        .ok_or(Error::NotFound)
}

/// The tenant of a builder call (`X-Tenant-Id`); the revision is looked up under its RLS.
fn builder_tenant(headers: &axum::http::HeaderMap) -> Result<uuid::Uuid, Error> {
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| uuid::Uuid::try_parse(v).ok())
        .ok_or(Error::BadRequest {
            code: "missing_tenant",
            detail: "X-Tenant-Id is required".into(),
        })
}

async fn builder_tx(
    s: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<platform::db::TenantTx, Error> {
    Ok(platform::db::tenant_tx(&s.db, builder_tenant(headers)?).await?)
}

fn revision_id(
    path: Result<Path<uuid::Uuid>, axum::extract::rejection::PathRejection>,
) -> Result<uuid::Uuid, Error> {
    path.map(|Path(id)| id).map_err(|_| Error::NotFound)
}

/// What to build: the revision, whether it is token-only, the tenant's `ASTRO_KEY`.
#[utoipa::path(
    get,
    path = "/internal/v1/themes/revisions/{id}/build",
    tag = "internal",
    security(("builder_token" = [])),
    params(crate::admin::IdParam),
    responses(
        (status = 200, body = themes::BuildSpec),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn build_spec(
    _b: BuilderService,
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    id: Result<Path<uuid::Uuid>, axum::extract::rejection::PathRejection>,
) -> Result<Json<themes::BuildSpec>, Error> {
    let id = revision_id(id)?;
    let keys = theme_keys(&s)?.clone();
    let mut tx = builder_tx(&s, &headers).await?;
    let spec = themes::build_spec(&mut tx, &keys, id).await?;
    tx.commit().await?;
    Ok(Json(spec))
}

/// The revision's source archive (`.tar.gz`, validated when it was stored).
#[utoipa::path(
    get,
    path = "/internal/v1/themes/revisions/{id}/source",
    tag = "internal",
    security(("builder_token" = [])),
    params(crate::admin::IdParam),
    responses(
        (status = 200, content_type = "application/gzip", body = Vec<u8>),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn build_source(
    _b: BuilderService,
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    id: Result<Path<uuid::Uuid>, axum::extract::rejection::PathRejection>,
) -> Result<Response, Error> {
    let id = revision_id(id)?;
    let mut tx = builder_tx(&s, &headers).await?;
    let bytes = themes::source_archive(&mut tx, &s.storage, id).await?;
    tx.commit().await?;
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/gzip"),
        )],
        bytes,
    )
        .into_response())
}

/// Builder status callback (spec §8.4): `building`, then `failed` or (after the artifact)
/// `ready`, with the gate report.
#[utoipa::path(
    post,
    path = "/internal/v1/themes/revisions/{id}/status",
    tag = "internal",
    security(("builder_token" = [])),
    params(crate::admin::IdParam),
    request_body = themes::StatusUpdate,
    responses(
        (status = 200, body = themes::RevisionSummary),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn build_status(
    _b: BuilderService,
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    id: Result<Path<uuid::Uuid>, axum::extract::rejection::PathRejection>,
    body: axum::body::Bytes,
) -> Result<Json<themes::RevisionSummary>, Error> {
    let id = revision_id(id)?;
    let update: themes::StatusUpdate = crate::admin::parse_json(&body)?;
    let mut tx = builder_tx(&s, &headers).await?;
    let rev = themes::builder_status(&mut tx, id, &update).await?;
    tx.commit().await?;
    Ok(Json(rev))
}

/// The built artifact as an uncompressed tar of its directory: registered (content-addressed,
/// A22) and attached; the revision moves to `checking`. Returns the preview host, a preview
/// token and the pages for the browser gates.
#[utoipa::path(
    put,
    path = "/internal/v1/themes/revisions/{id}/artifact",
    tag = "internal",
    security(("builder_token" = [])),
    params(crate::admin::IdParam),
    request_body(content = Vec<u8>, content_type = "application/x-tar"),
    responses(
        (status = 200, body = themes::CheckSpec),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_artifact", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn build_artifact(
    _b: BuilderService,
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    id: Result<Path<uuid::Uuid>, axum::extract::rejection::PathRejection>,
    body: axum::body::Body,
) -> Result<Json<themes::CheckSpec>, Error> {
    let id = revision_id(id)?;
    let tenant = builder_tenant(&headers)?;
    let keys = theme_keys(&s)?.clone();
    let tar = axum::body::to_bytes(
        body,
        crate::body_limit("/internal/v1/themes/revisions/x/artifact"),
    )
    .await
    .map_err(|_| Error::PayloadTooLarge)?;
    let (artifact_id, files) =
        themes::archive::read_artifact_tar(&tar).map_err(|e| Error::Validation {
            code: "invalid_artifact",
            detail: e.problems.join("; "),
        })?;
    Ok(Json(
        themes::attach_artifact(
            &s.db,
            &s.storage,
            &keys,
            tenant,
            id,
            &artifact_id,
            files,
            chrono::Utc::now(),
        )
        .await?,
    ))
}

/// A gate screenshot (PNG; `home|category|product` × `mobile|desktop`), private bucket.
#[utoipa::path(
    put,
    path = "/internal/v1/themes/revisions/{id}/screenshots/{name}",
    tag = "internal",
    security(("builder_token" = [])),
    params(crate::admin::IdParam, ("name" = String, Path, description = "e.g. home-mobile")),
    request_body(content = Vec<u8>, content_type = "image/png"),
    responses(
        (status = 204, description = "Stored"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn build_screenshot(
    _b: BuilderService,
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    path: Result<Path<(uuid::Uuid, String)>, axum::extract::rejection::PathRejection>,
    body: axum::body::Body,
) -> Result<axum::http::StatusCode, Error> {
    let Path((id, name)) = path.map_err(|_| Error::NotFound)?;
    let png = axum::body::to_bytes(body, 6 * 1024 * 1024)
        .await
        .map_err(|_| Error::PayloadTooLarge)?;
    let mut tx = builder_tx(&s, &headers).await?;
    themes::store_screenshot(&mut tx, &s.storage, id, &name, &png).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
