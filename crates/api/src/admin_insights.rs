use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use commerce::insights::{self, Insight};
use platform::Error;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, path_id};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_insights))
        .routes(routes!(dismiss_insight))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InsightList {
    /// Open signals, most affected visits first (at most 50).
    pub items: Vec<Insight>,
}

/// What the nightly detectors found, ranked.
#[utoipa::path(
    get,
    path = "/admin/v1/insights",
    tag = "insights",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = InsightList))
)]
async fn list_insights(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<InsightList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| insights::list(tx).await).await?;
    Ok(Json(InsightList { items }))
}

/// Hides a signal for good, also when it is detected again. Idempotent.
#[utoipa::path(
    post,
    path = "/admin/v1/insights/{id}/dismiss",
    tag = "insights",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Insight),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn dismiss_insight(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Insight>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            insights::dismiss(tx, actor, id).await
        })
        .await?,
    ))
}
