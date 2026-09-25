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
use crate::auth::Service;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(resolve))
        // A wildcard path: registered on axum directly (utoipa paths cannot express `{*path}`).
        .route("/internal/v1/artifacts/{id}/{*path}", get(artifact_file))
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
