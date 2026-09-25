//! Staff management HTTP boundary; business rules live in commerce::staff.
use crate::{
    AppState,
    admin::{TenantHeader, parse_json, path_id},
    auth::TenantStaff,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use commerce::{
    staff::{self, Invitation, RoleChange, StaffMember},
    tenancy::Role,
};
use platform::{Error, db::tenant_tx};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(invite))
        .routes(routes!(change_role, remove))
}
#[derive(Serialize, ToSchema)]
pub struct StaffList {
    pub items: Vec<StaffMember>,
}
#[utoipa::path(get, path = "/admin/v1/staff", tag = "staff", security(("staff_jwt" = [])), params(TenantHeader),
responses((status = 200, body = StaffList),
(status = 401, body = platform::Problem, content_type = "application/problem+json"),
(status = 403, body = platform::Problem, content_type = "application/problem+json"),
(status = 404, body = platform::Problem, content_type = "application/problem+json"),
(status = 409, body = platform::Problem, content_type = "application/problem+json"),
(status = 422, body = platform::Problem, content_type = "application/problem+json"),
(status = 503, body = platform::Problem, content_type = "application/problem+json")))]
async fn list(caller: TenantStaff, State(s): State<AppState>) -> Result<Json<StaffList>, Error> {
    caller.require(Role::Admin)?;
    let mut tx = tenant_tx(&s.db, caller.tenant_id).await?;
    let items = staff::list(&mut tx, &caller.user.user_id).await?;
    tx.commit().await?;
    Ok(Json(StaffList { items }))
}
#[utoipa::path(post, path = "/admin/v1/staff/invitations", tag = "staff", security(("staff_jwt" = [])), params(TenantHeader),
request_body = Invitation,
responses((status = 201, body = StaffMember),
(status = 401, body = platform::Problem, content_type = "application/problem+json"),
(status = 403, body = platform::Problem, content_type = "application/problem+json"),
(status = 404, body = platform::Problem, content_type = "application/problem+json"),
(status = 409, body = platform::Problem, content_type = "application/problem+json"),
(status = 422, body = platform::Problem, content_type = "application/problem+json"),
(status = 503, body = platform::Problem, content_type = "application/problem+json")))]
async fn invite(
    caller: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<StaffMember>), Error> {
    caller.require(Role::Admin)?;
    caller.require_fresh_auth()?;
    let input: Invitation = parse_json(&body)?;
    staff::authorize(caller.role, None, Some(input.role))?;
    let email = staff::normalize_email(&input.email)?;
    let auth = s.auth_service.as_ref().ok_or_else(|| {
        Error::Unavailable("staff invitations need the auth service (AUTH_INTERNAL_URL)".into())
    })?;
    let user = auth.ensure_user(&email, &email).await?;
    let callback = s
        .admin_origin
        .to_str()
        .map_err(|_| Error::Internal("invalid admin origin".into()))?;
    let mut tx = tenant_tx(&s.db, caller.tenant_id).await?;
    let member = staff::invite(&mut tx, &caller.user.user_id, &user, &email, input.role).await?;
    // The invitation email leaves through the outbox → mail pipeline once the membership has
    // committed (worker `staff.invite_mail`), never before.
    platform::queue::publish(
        &mut *tx,
        staff::INVITED_EVENT,
        &serde_json::json!({ "member_id": member.id, "callback_url": callback }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(member)))
}
#[utoipa::path(patch, path = "/admin/v1/staff/{id}", tag = "staff", security(("staff_jwt" = [])), params(TenantHeader, ("id" = Uuid, Path, description = "Staff membership id")),
request_body = RoleChange,
responses((status = 200, body = StaffMember),
(status = 401, body = platform::Problem, content_type = "application/problem+json"),
(status = 403, body = platform::Problem, content_type = "application/problem+json"),
(status = 404, body = platform::Problem, content_type = "application/problem+json"),
(status = 409, body = platform::Problem, content_type = "application/problem+json"),
(status = 422, body = platform::Problem, content_type = "application/problem+json"),
(status = 503, body = platform::Problem, content_type = "application/problem+json")))]
async fn change_role(
    caller: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<StaffMember>, Error> {
    caller.require(Role::Admin)?;
    caller.require_fresh_auth()?;
    let id = path_id(path)?;
    let input: RoleChange = parse_json(&body)?;
    let mut tx = tenant_tx(&s.db, caller.tenant_id).await?;
    let member = staff::change_role(&mut tx, &caller.user.user_id, id, input.role).await?;
    tx.commit().await?;
    Ok(Json(member))
}
#[utoipa::path(delete, path = "/admin/v1/staff/{id}", tag = "staff", security(("staff_jwt" = [])), params(TenantHeader, ("id" = Uuid, Path, description = "Staff membership id")),
responses((status = 204),
(status = 401, body = platform::Problem, content_type = "application/problem+json"),
(status = 403, body = platform::Problem, content_type = "application/problem+json"),
(status = 404, body = platform::Problem, content_type = "application/problem+json"),
(status = 409, body = platform::Problem, content_type = "application/problem+json"),
(status = 422, body = platform::Problem, content_type = "application/problem+json"),
(status = 503, body = platform::Problem, content_type = "application/problem+json")))]
async fn remove(
    caller: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    caller.require(Role::Admin)?;
    caller.require_fresh_auth()?;
    let id = path_id(path)?;
    let mut tx = tenant_tx(&s.db, caller.tenant_id).await?;
    staff::remove(&mut tx, &caller.user.user_id, id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
