//! Tenant-scoped flow administration. Staff may inspect runs; admin/owner configure flows.
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use commerce::flows::{self, Definition, DefinitionChange, Run, RunDetail};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::AppState;
use crate::admin::{TenantHeader, in_tx, parse_json};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_flows))
        .routes(routes!(configure_flow))
        .routes(routes!(list_runs))
        .routes(routes!(run_detail))
        .routes(routes!(cancel_run))
        .routes(routes!(advance_clock))
}

#[derive(Serialize, ToSchema)]
pub struct FlowList {
    pub items: Vec<Definition>,
}
#[derive(Serialize, ToSchema)]
pub struct RunList {
    pub items: Vec<Run>,
}

#[utoipa::path(get,path="/admin/v1/flows",tag="flows",security(("staff_jwt"=[])),params(TenantHeader),responses((status=200,body=FlowList)))]
async fn list_flows(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<FlowList>, Error> {
    Ok(Json(FlowList {
        items: in_tx(&s, staff.tenant_id, async |tx| flows::definitions(tx).await).await?,
    }))
}

#[utoipa::path(put,path="/admin/v1/flows/{kind}",tag="flows",security(("staff_jwt"=[])),params(TenantHeader,("kind"=String,Path)),request_body=DefinitionChange,responses((status=200,body=Definition)))]
async fn configure_flow(
    staff: TenantStaff,
    State(s): State<AppState>,
    Path(kind): Path<String>,
    body: Bytes,
) -> Result<Json<Definition>, Error> {
    staff.require(Role::Admin)?;
    let change: DefinitionChange = parse_json(&body)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            flows::configure(tx, &staff.user.user_id, &kind, &change).await
        })
        .await?,
    ))
}

#[utoipa::path(get,path="/admin/v1/flows/runs",tag="flows",security(("staff_jwt"=[])),params(TenantHeader),responses((status=200,body=RunList)))]
async fn list_runs(staff: TenantStaff, State(s): State<AppState>) -> Result<Json<RunList>, Error> {
    Ok(Json(RunList {
        items: in_tx(&s, staff.tenant_id, async |tx| flows::runs(tx, 100).await).await?,
    }))
}

#[utoipa::path(get,path="/admin/v1/flows/runs/{id}",tag="flows",security(("staff_jwt"=[])),params(TenantHeader,("id"=Uuid,Path)),responses((status=200,body=RunDetail),(status=404)))]
async fn run_detail(
    staff: TenantStaff,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RunDetail>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            flows::run_detail(tx, id).await
        })
        .await?,
    ))
}

#[utoipa::path(post,path="/admin/v1/flows/runs/{id}/cancel",tag="flows",security(("staff_jwt"=[])),params(TenantHeader,("id"=Uuid,Path)),responses((status=200,body=RunDetail),(status=404)))]
async fn cancel_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RunDetail>, Error> {
    staff.require(Role::Admin)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            flows::cancel_run(tx, &staff.user.user_id, id).await
        })
        .await?,
    ))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdvanceInput {
    pub hours: i64,
}
#[derive(Serialize, ToSchema)]
pub struct ClockState {
    pub now: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(post,path="/admin/v1/flows/test-clock/advance",tag="flows",security(("staff_jwt"=[])),params(TenantHeader),request_body=AdvanceInput,responses((status=200,body=ClockState),(status=404,description="Unavailable in production")))]
async fn advance_clock(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<ClockState>, Error> {
    if !flows::dev_clock_allowed() {
        return Err(Error::NotFound);
    }
    staff.require(Role::Admin)?;
    let input: AdvanceInput = parse_json(&body)?;
    let now = in_tx(&s, staff.tenant_id, async |tx| {
        let now = flows::advance_clock(tx, input.hours).await?;
        flows::enroll_due(tx, now).await?;
        flows::execute_due(tx, &s.public_urls, now).await?;
        Ok(now)
    })
    .await?;
    Ok(Json(ClockState { now }))
}
