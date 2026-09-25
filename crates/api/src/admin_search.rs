//! Search Admin API (spec §11.1): index status and a full rebuild with an index swap.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use commerce::audit;
use commerce::search::{self, index::IndexStatus};
use commerce::tenancy::Role;
use platform::Error;
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::{TenantHeader, in_tx};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(status))
        .routes(routes!(rebuild))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchStatus {
    /// One index per locale the tenant's markets sell in.
    pub indexes: Vec<IndexStatus>,
}

/// State of the tenant's search indexes.
#[utoipa::path(
    get,
    path = "/admin/v1/search/status",
    tag = "search",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = SearchStatus))
)]
async fn status(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<SearchStatus>, Error> {
    let indexes = in_tx(&s, staff.tenant_id, async |tx| {
        search::index::status(tx).await
    })
    .await?;
    Ok(Json(SearchStatus { indexes }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RebuildQueued {
    pub job_id: i64,
}

/// Rebuilds every index of the tenant from the catalog into new indexes and swaps them in
/// atomically; searches keep being answered from the old ones meanwhile. Owner or admin.
/// A request made while a rebuild is already running is served by a second rebuild after it,
/// unless one started after the request anyway.
#[utoipa::path(
    post,
    path = "/admin/v1/search/rebuild",
    tag = "search",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 202, body = RebuildQueued),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn rebuild(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<(StatusCode, Json<RebuildQueued>), Error> {
    staff.require(Role::Admin)?;
    let job_id = in_tx(&s, staff.tenant_id, async |tx| {
        let version = search::next_version(&mut **tx).await?;
        let job = search::manual_rebuild_job(tx.tenant_id(), version);
        let id = platform::queue::enqueue(&mut **tx, &job).await?;
        audit::record(
            tx,
            &staff.user.user_id,
            "search.rebuild_requested",
            "search_index",
            None,
            &json!({ "job_id": id }),
        )
        .await?;
        Ok(id)
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(RebuildQueued { job_id })))
}
