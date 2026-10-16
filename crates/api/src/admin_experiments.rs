use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use commerce::experiments;
use commerce::tenancy::Role;
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, path_id};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(start_experiment))
}

/// Starts a draft experiment (owner/admin). Refused with `cookie_policy_outdated` until the
/// published cookie page, in every market locale, comes from a legal template that says
/// analytics covers testing page variants.
#[utoipa::path(
    post,
    path = "/admin/v1/experiments/{id}/start",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Running"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "experiment_not_draft, experiment_running", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "cookie_policy_outdated", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn start_experiment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        experiments::start(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
