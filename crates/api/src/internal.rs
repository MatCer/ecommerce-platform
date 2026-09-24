//! Internal API (`/internal/v1`, spec §8.4) for platform services holding the service token.
//! Not routed by Caddy: callers reach the API on the internal network.

use axum::Json;
use axum::extract::{Query, State};
use commerce::tenancy::{self, Resolved};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::auth::Service;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(resolve))
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
