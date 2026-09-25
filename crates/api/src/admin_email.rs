//! Email Admin API (spec §11.4, WP18; follow-ups of WP9): the sent-email log, the suppression
//! list (add/remove, audited), the email logo and the editable subject/intro texts. Owner or
//! admin: these are deliverability settings and hold every customer's address.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use commerce::notifications::admin::{
    self, EmailBranding, MessageDetail, MessageFilter, MessagePage, Suppression, SuppressionFilter,
    SuppressionInput, SuppressionPage, TemplateText, TemplateTextInput,
};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_messages))
        .routes(routes!(get_message))
        .routes(routes!(
            list_suppressions,
            add_suppression,
            remove_suppression
        ))
        .routes(routes!(get_branding, put_branding))
        .routes(routes!(list_texts))
        .routes(routes!(put_text))
}

/// Sent and queued emails, newest first (no bodies in the list).
#[utoipa::path(
    get,
    path = "/admin/v1/emails",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader, MessageFilter),
    responses(
        (status = 200, body = MessagePage),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_messages(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<MessageFilter>, QueryRejection>,
) -> Result<Json<MessagePage>, Error> {
    staff.require(Role::Admin)?;
    let f = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::list_messages(tx, &f).await
        })
        .await?,
    ))
}

/// One email; the body only for non-sensitive mail (never sign-in or order links).
#[utoipa::path(
    get,
    path = "/admin/v1/emails/{id}",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = MessageDetail),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_message(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<MessageDetail>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::get_message(tx, id).await
        })
        .await?,
    ))
}

/// Suppressed addresses (bounces, complaints, manual), alphabetically.
#[utoipa::path(
    get,
    path = "/admin/v1/email-suppressions",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader, SuppressionFilter),
    responses((status = 200, body = SuppressionPage))
)]
async fn list_suppressions(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<SuppressionFilter>, QueryRejection>,
) -> Result<Json<SuppressionPage>, Error> {
    staff.require(Role::Admin)?;
    let f = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::list_suppressions(tx, &f).await
        })
        .await?,
    ))
}

/// Suppresses an address for every stream (audited).
#[utoipa::path(
    post,
    path = "/admin/v1/email-suppressions",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = SuppressionInput,
    responses(
        (status = 200, body = Suppression),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn add_suppression(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<Suppression>, Error> {
    staff.require(Role::Admin)?;
    let input: SuppressionInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::add_suppression(tx, actor, &input).await
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EmailQuery {
    pub email: String,
}

/// Removes a suppression (the address gets mail again; audited with what it was).
#[utoipa::path(
    delete,
    path = "/admin/v1/email-suppressions",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader, EmailQuery),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn remove_suppression(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<EmailQuery>, QueryRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let q = query_params(query)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        admin::remove_suppression(tx, actor, &q.email).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The logo at the top of every email.
#[utoipa::path(
    get,
    path = "/admin/v1/email-branding",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = EmailBranding))
)]
async fn get_branding(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<EmailBranding>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| admin::branding(tx).await).await?,
    ))
}

/// Sets the email logo (a processed image asset of the shop) or clears it. Owner or admin.
#[utoipa::path(
    put,
    path = "/admin/v1/email-branding",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = EmailBranding,
    responses(
        (status = 200, body = EmailBranding),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_branding(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<EmailBranding>, Error> {
    staff.require(Role::Admin)?;
    let input: EmailBranding = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::set_branding(tx, actor, &input).await
        })
        .await?,
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateTextList {
    pub items: Vec<TemplateText>,
}

/// Editable email texts: every template and language with the platform default.
#[utoipa::path(
    get,
    path = "/admin/v1/email-templates",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = TemplateTextList))
)]
async fn list_texts(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<TemplateTextList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| admin::texts(tx).await).await?;
    Ok(Json(TemplateTextList { items }))
}

/// Sets the subject/intro of a template in a language (both empty = platform default). Owner
/// or admin.
#[utoipa::path(
    put,
    path = "/admin/v1/email-templates/{template}/{locale}",
    tag = "email",
    security(("staff_jwt" = [])),
    params(TenantHeader, ("template" = String, Path), ("locale" = String, Path)),
    request_body = TemplateTextInput,
    responses(
        (status = 200, body = TemplateText),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_text(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Bytes,
) -> Result<Json<TemplateText>, Error> {
    staff.require(Role::Admin)?;
    let Path((template, locale)) = path.map_err(|_| Error::NotFound)?;
    let input: TemplateTextInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            admin::set_text(tx, actor, &template, &locale, &input).await
        })
        .await?,
    ))
}
