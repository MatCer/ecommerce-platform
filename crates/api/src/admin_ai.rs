//! AI helpers Admin API (spec §12): usage + quota, glossary, proposals (descriptions, SEO,
//! translations) accepted per field, AI markers, and bulk edit by prompt. Every staff member
//! may use them; writes go through the catalog/content/pricing services and the audit log.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::ai::fields::EntityType;
use commerce::ai::glossary::{self, Glossary};
use commerce::ai::marks::{self, AiMarkList};
use commerce::ai::plan::{self, BulkPlan, NewPlan};
use commerce::ai::proposals::{self, AcceptProposal, NewProposal, Proposal};
use commerce::ai::{self, UsageSummary};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, idempotent, in_tx, parse_json, path_id, query_params,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(usage))
        .routes(routes!(get_glossary, put_glossary))
        .routes(routes!(create_proposal))
        .routes(routes!(get_proposal))
        .routes(routes!(accept_proposal))
        .routes(routes!(discard_proposal))
        .routes(routes!(list_marks))
        .routes(routes!(create_plan))
        .routes(routes!(get_plan))
        .routes(routes!(apply_plan))
}

/// This month's AI usage of the shop, its token allowance and the provider in use.
#[utoipa::path(
    get,
    path = "/admin/v1/ai/usage",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = UsageSummary))
)]
async fn usage(staff: TenantStaff, State(s): State<AppState>) -> Result<Json<UsageSummary>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| ai::usage(tx, &s.ai).await).await?,
    ))
}

/// Terms translations must keep (brand names) or translate one fixed way.
#[utoipa::path(
    get,
    path = "/admin/v1/ai/glossary",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = Glossary))
)]
async fn get_glossary(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<Glossary>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| glossary::get(tx).await).await?,
    ))
}

/// Replaces the glossary (at most 500 terms). Audited.
#[utoipa::path(
    put,
    path = "/admin/v1/ai/glossary",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = Glossary,
    responses(
        (status = 200, body = Glossary),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_glossary(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<Glossary>, Error> {
    let input: Glossary = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            glossary::put(tx, actor, &input).await
        })
        .await?,
    ))
}

/// Starts generating a proposal (a job): poll `GET /ai/proposals/{id}` until it is `ready`
/// or `failed`. `402 ai_quota_exceeded` when the monthly allowance is used up.
#[utoipa::path(
    post,
    path = "/admin/v1/ai/proposals",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = NewProposal,
    responses(
        (status = 202, body = Proposal),
        (status = 402, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_proposal(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewProposal = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let p = in_tx(&s, staff.tenant_id, async |tx| {
        proposals::create(tx, &s.ai, actor, &input).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(p)).into_response())
}

#[utoipa::path(
    get,
    path = "/admin/v1/ai/proposals/{id}",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Proposal),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_proposal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<uuid::Uuid>, PathRejection>,
) -> Result<Json<Proposal>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| proposals::get(tx, id).await).await?,
    ))
}

/// Writes the chosen fields through the entity's service (audited) and labels them as
/// AI-generated. `409 proposal_stale` when a field changed since the proposal was made.
#[utoipa::path(
    post,
    path = "/admin/v1/ai/proposals/{id}/accept",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = AcceptProposal,
    responses(
        (status = 200, body = Proposal),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn accept_proposal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<uuid::Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Proposal>, Error> {
    let id = path_id(id)?;
    let input: AcceptProposal = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            proposals::accept(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/admin/v1/ai/proposals/{id}/discard",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Proposal),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn discard_proposal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<uuid::Uuid>, PathRejection>,
) -> Result<Json<Proposal>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            proposals::discard(tx, id).await
        })
        .await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MarksQuery {
    pub entity_type: EntityType,
    /// The entity's id (a menu's handle).
    pub entity_id: String,
}

/// Fields of an entity whose current text was written by AI (AI Act transparency labels).
#[utoipa::path(
    get,
    path = "/admin/v1/ai/marks",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, MarksQuery),
    responses(
        (status = 200, body = AiMarkList),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_marks(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<MarksQuery>, QueryRejection>,
) -> Result<Json<AiMarkList>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            marks::list(tx, q.entity_type, &q.entity_id).await
        })
        .await?,
    ))
}

/// Starts planning a bulk edit from the staff's request (a job): poll
/// `GET /ai/bulk-plans/{id}`; a `ready` plan carries the target count and a preview (the dry
/// run). Nothing changes until the plan is applied.
#[utoipa::path(
    post,
    path = "/admin/v1/ai/bulk-plans",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = NewPlan,
    responses(
        (status = 202, body = BulkPlan),
        (status = 402, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_plan(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewPlan = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let p = in_tx(&s, staff.tenant_id, async |tx| {
        plan::create(tx, &s.ai, actor, &input).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(p)).into_response())
}

#[utoipa::path(
    get,
    path = "/admin/v1/ai/bulk-plans/{id}",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = BulkPlan),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_plan(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<uuid::Uuid>, PathRejection>,
) -> Result<Json<BulkPlan>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| plan::get(tx, id).await).await?,
    ))
}

const APPLY_PLAN: &str = "POST /admin/v1/ai/bulk-plans/{id}/apply";

/// Confirms a `ready` plan and applies it in the background (progress on the plan). Plans that
/// change prices need a sign-in at most 15 minutes old (`401 reauth_required`, A9). Honors
/// `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/ai/bulk-plans/{id}/apply",
    tag = "ai",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam, IdempotencyHeader),
    responses(
        (status = 202, body = BulkPlan),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn apply_plan(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<uuid::Uuid>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let id = path_id(id)?;
    let fresh = staff.require_fresh_auth().is_ok();
    let actor = staff.user.user_id.clone();
    idempotent(
        &s,
        &staff,
        &headers,
        APPLY_PLAN,
        &id,
        StatusCode::ACCEPTED,
        async |tx| plan::confirm(tx, &actor, id, fresh).await,
    )
    .await
}
