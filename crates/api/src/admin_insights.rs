use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use chrono::Utc;
use commerce::experiments::Experiment;
use commerce::insights::{self, Insight};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_insights))
        .routes(routes!(dismiss_insight))
        .routes(routes!(test_insight_revision))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InsightList {
    /// Open signals, and resolved ones a published theme change came from or an experiment
    /// tests; most affected visits first (at most 50).
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
    let today = Utc::now().date_naive();
    let mut items = in_tx(&s, staff.tenant_id, async |tx| {
        insights::list(tx, today).await
    })
    .await?;
    if staff.role < Role::Admin {
        items.iter_mut().for_each(|i| i.experiment_id = None);
    }
    Ok(Json(InsightList { items }))
}

/// Hides a signal until a run detects it with at least 1.5 times its score at dismissal, which
/// reopens it. Idempotent.
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
            insights::dismiss(tx, actor, id, Utc::now().date_naive()).await
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = InsightNewExperiment)]
pub struct NewExperiment {
    /// The final revision of an accepted AI theme edit started from the insight's draft.
    pub revision_id: Uuid,
}

/// Prepares a draft experiment testing the insight's accepted AI theme revision against the
/// active theme, or returns the experiment that already tests it (owner/admin). Start it
/// through the experiments API.
#[utoipa::path(
    post,
    path = "/admin/v1/insights/{id}/experiments",
    tag = "insights",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = NewExperiment,
    responses(
        (status = 200, body = Experiment),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, description = "unknown insight, or not the final revision of an AI theme edit started from it", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "ai_run_not_accepted", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_body, theme_revision_not_ready", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn test_insight_revision(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Experiment>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: NewExperiment = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            insights::test_revision(tx, actor, id, input.revision_id).await
        })
        .await?,
    ))
}
