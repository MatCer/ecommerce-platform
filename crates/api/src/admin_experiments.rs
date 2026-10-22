use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::Utc;
use commerce::experiments::{self, Experiment, ExperimentDetail, NewExperiment};
use commerce::tenancy::Role;
use platform::Error;
use serde::Serialize;
use utoipa::ToSchema;
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
        .routes(routes!(list_experiments, create_experiment))
        .routes(routes!(get_experiment))
        .routes(routes!(start_experiment))
        .routes(routes!(stop_experiment))
        .routes(routes!(promote_experiment))
}

#[derive(Serialize, ToSchema)]
pub struct ExperimentList {
    pub items: Vec<Experiment>,
}

/// The newest 100 experiments (owner/admin).
#[utoipa::path(
    get,
    path = "/admin/v1/experiments",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = ExperimentList),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_experiments(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<ExperimentList>, Error> {
    staff.require(Role::Admin)?;
    let items = in_tx(&s, staff.tenant_id, async |tx| experiments::list(tx).await).await?;
    Ok(Json(ExperimentList { items }))
}

/// Creates a draft that splits visitors 50/50 between the shop as it is (`control`) and the
/// change (`variant`), as the next version of its name (owner/admin). Honors
/// `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/experiments",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewExperiment,
    responses(
        (status = 201, body = Experiment),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "experiment_exists, idempotency_conflict, idempotency_in_progress", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_body, invalid_experiment_name, invalid_experiment_config, unknown_theme_revision, theme_revision_not_ready", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_experiment(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewExperiment = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "experiments.create",
        &input,
        async |tx| experiments::create(tx, &actor, &input).await,
    )
    .await
}

/// An experiment and, once it started, its readout as of the last UTC midnight
/// (owner/admin).
#[utoipa::path(
    get,
    path = "/admin/v1/experiments/{id}",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = ExperimentDetail),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_experiment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ExperimentDetail>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            experiments::get(tx, id, Utc::now()).await
        })
        .await?,
    ))
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
        (status = 409, description = "experiment_not_draft, experiment_running, theme_revision_not_ready", body = platform::Problem, content_type = "application/problem+json"),
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

/// Stops a running experiment (owner/admin); an experiment that already stopped stays as it
/// is. Its readout keeps counting the purchases of its last week of visitors.
#[utoipa::path(
    post,
    path = "/admin/v1/experiments/{id}/stop",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Stopped"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "experiment_not_running", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn stop_experiment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        experiments::stop(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes the variant the shop's own (owner/admin, login at most 15 minutes old): publishes
/// its theme revision or stores its hero copy, and stops the experiment if it still runs. An
/// experiment promoted before is returned as it is.
#[utoipa::path(
    post,
    path = "/admin/v1/experiments/{id}/promote",
    tag = "experiments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Experiment),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "experiment_not_started, experiment_not_promotable, not_publishable, ai_run_not_accepted", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn promote_experiment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Experiment>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            experiments::promote(tx, actor, id).await
        })
        .await?,
    ))
}
