//! Themes Admin API (WP23, spec §8.3, §12.3, A6, A9, A21): tenant theme revisions, the check
//! report, previews, publish and rollback.
//!
//! Staff may look and preview; creating revisions and downloading sources needs Admin;
//! publishing (and rolling back) needs Admin with a login at most 15 minutes old (A9).
//!
//! AI theme edits (WP24): Admin starts, cancels, accepts or discards a run; staff may follow
//! it. An AI revision is publishable only after its run was accepted.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use commerce::tenancy::Role;
use commerce::themes::ai_edit::{self, NewRun, RevisionDiff, RunDetail, RunSummary};
use commerce::themes::{
    self, Download, PreviewLink, RevisionDetail, RevisionSummary, ThemeKeys, TokensInput,
};
use platform::Error;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_revisions))
        .routes(routes!(get_revision))
        .routes(routes!(fork))
        .routes(routes!(reset))
        .routes(routes!(edit_tokens))
        .routes(routes!(upload))
        .routes(routes!(source))
        .routes(routes!(preview))
        .routes(routes!(publish))
        .routes(routes!(revision_diff))
        .routes(routes!(list_runs, start_run))
        .routes(routes!(get_run))
        .routes(routes!(cancel_run))
        .routes(routes!(accept_run))
        .routes(routes!(discard_run))
}

fn keys(s: &AppState) -> Result<&ThemeKeys, Error> {
    s.themes
        .as_ref()
        .ok_or_else(|| Error::Unavailable("theme builder is not configured (THEME_SECRET)".into()))
}

#[derive(Serialize, ToSchema)]
pub struct RevisionList {
    pub items: Vec<RevisionSummary>,
}

/// Revisions, newest first (at most 100), with the active one marked.
#[utoipa::path(
    get,
    path = "/admin/v1/themes/revisions",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = RevisionList))
)]
async fn list_revisions(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<RevisionList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| themes::list(tx).await).await?;
    Ok(Json(RevisionList { items }))
}

/// One revision with its check report (budget numbers, axe, smoke, failures), design tokens
/// and screenshots (presigned, 5 minutes).
#[utoipa::path(
    get,
    path = "/admin/v1/themes/revisions/{id}",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RevisionDetail),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_revision(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RevisionDetail>, Error> {
    let id = path_id(id)?;
    let storage = s.storage.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            themes::detail(tx, &storage, id).await
        })
        .await?,
    ))
}

/// Forks the default theme into a tenant-owned source revision and queues its build.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/fork",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 201, body = RevisionSummary),
        (status = 409, description = "builds_in_progress | default_source_missing", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn fork(staff: TenantStaff, State(s): State<AppState>) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let (storage, actor) = (s.storage.clone(), staff.user.user_id.clone());
    let rev = in_tx(&s, staff.tenant_id, async |tx| {
        themes::fork(tx, &storage, &actor).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(rev)).into_response())
}

/// A new revision from the latest default theme with the shop's current design tokens.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/reset",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 201, body = RevisionSummary),
        (status = 409, description = "builds_in_progress | default_source_missing", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn reset(staff: TenantStaff, State(s): State<AppState>) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let (storage, actor) = (s.storage.clone(), staff.user.user_id.clone());
    let rev = in_tx(&s, staff.tenant_id, async |tx| {
        themes::reset(tx, &storage, &actor).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(rev)).into_response())
}

/// A revision that changes only the design tokens (`theme.tokens.json`) of a base revision
/// (default: the active one). Its build skips the type check, Lighthouse and the smoke test;
/// the JS/calls budgets and axe still run.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/tokens",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = TokensInput,
    responses(
        (status = 201, body = RevisionSummary),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_tokens", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn edit_tokens(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: axum::body::Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: TokensInput = parse_json(&body)?;
    let (storage, actor) = (s.storage.clone(), staff.user.user_id.clone());
    let rev = in_tx(&s, staff.tenant_id, async |tx| {
        themes::edit_tokens(tx, &storage, &actor, &input).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(rev)).into_response())
}

/// Uploads a theme source archive (`.tar.gz`, at most 20 MB, expanded at most 50 MB) for power
/// users. Structural problems (symlinks, `..`, absolute paths, files outside `src/`,
/// `public/`, `checks/`, oversize) are refused with every reason listed (A6); contract
/// violations (dependencies, foreign fetch, ...) fail the build's gates with reasons.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/upload",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body(content = Vec<u8>, content_type = "application/gzip"),
    responses(
        (status = 201, body = RevisionSummary),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 413, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_archive (detail: one reason per line)", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn upload(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Body,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let gz = axum::body::to_bytes(body, themes::archive::MAX_UPLOAD_BYTES)
        .await
        .map_err(|_| Error::PayloadTooLarge)?;
    let (storage, actor) = (s.storage.clone(), staff.user.user_id.clone());
    let rev = in_tx(&s, staff.tenant_id, async |tx| {
        themes::upload(tx, &storage, &actor, &gz).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(rev)).into_response())
}

/// A presigned download (5 minutes) of a revision's source archive.
#[utoipa::path(
    get,
    path = "/admin/v1/themes/revisions/{id}/source",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Download),
        (status = 404, description = "no such revision, or it follows the default theme", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn source(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Download>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let storage = s.storage.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            themes::source_download(tx, &storage, id).await
        })
        .await?,
    ))
}

/// A preview link for a built revision: `preview-<n>--<shop>` with an HMAC token bound to the
/// tenant, the revision and a 1-hour expiry (A21). Never cached, `noindex`, no checkout.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/{id}/preview",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = PreviewLink),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "not_built | no_domain", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preview(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<PreviewLink>, Error> {
    let id = path_id(id)?;
    let keys = keys(&s)?.clone();
    let urls = s.public_urls.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            themes::preview_link(tx, &keys, &urls, id, chrono::Utc::now()).await
        })
        .await?,
    ))
}

/// Publishes a revision that passed the checks, or rolls back to an earlier published one:
/// the active pointer moves atomically, then the edge is purged (Admin, login at most 15
/// minutes old, audited).
#[utoipa::path(
    post,
    path = "/admin/v1/themes/revisions/{id}/publish",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RevisionSummary),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "not_publishable | ai_run_not_accepted", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn publish(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RevisionSummary>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let id = path_id(id)?;
    let actor = staff.user.user_id.clone();
    let rev = in_tx(&s, staff.tenant_id, async |tx| {
        themes::publish(tx, &actor, id).await
    })
    .await?;
    s.edge.tenant(staff.tenant_id).await;
    Ok(Json(rev))
}

/// What a revision changed compared to its parent: a unified diff of the sources.
#[utoipa::path(
    get,
    path = "/admin/v1/themes/revisions/{id}/diff",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RevisionDiff),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn revision_diff(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RevisionDiff>, Error> {
    let id = path_id(id)?;
    let storage = s.storage.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            ai_edit::revision_diff(tx, &storage, id).await
        })
        .await?,
    ))
}

#[derive(Serialize, ToSchema)]
#[schema(as = AiThemeRunList)]
pub struct RunList {
    pub items: Vec<RunSummary>,
    /// `anthropic`, `fake` (the scripted demo agent, no key configured) or `disabled`.
    pub provider: String,
}

/// AI theme edits, newest first (at most 50).
#[utoipa::path(
    get,
    path = "/admin/v1/themes/ai-runs",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = RunList))
)]
async fn list_runs(staff: TenantStaff, State(s): State<AppState>) -> Result<Json<RunList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| ai_edit::list(tx).await).await?;
    Ok(Json(RunList {
        items,
        provider: s.ai.provider().into(),
    }))
}

/// Starts an AI theme edit from a prompt (a job): the agent edits a copy of the base revision
/// (default: the active one), writes a functional check and runs the builder's gates, with at
/// most 25 turns and 3 repairs. Poll `GET /themes/ai-runs/{id}`.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/ai-runs",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = NewRun,
    responses(
        (status = 202, body = RunSummary),
        (status = 402, description = "ai_quota_exceeded", body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "ai_run_in_progress | base_not_validated | ai_run_not_accepted", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn start_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewRun = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    let run = in_tx(&s, staff.tenant_id, async |tx| {
        ai_edit::start(tx, &s.ai, &actor, &input).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run)).into_response())
}

/// One AI theme edit: progress (tool steps, turns, checks), the agent's summary, the diff
/// against the base revision and the last check report.
#[utoipa::path(
    get,
    path = "/admin/v1/themes/ai-runs/{id}",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RunDetail),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RunDetail>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            ai_edit::detail(tx, id).await
        })
        .await?,
    ))
}

/// Cancels a queued run at once, a running one at its next step.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/ai-runs/{id}/cancel",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RunSummary),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cancel_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RunSummary>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = staff.user.user_id.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            ai_edit::cancel(tx, &actor, id).await
        })
        .await?,
    ))
}

/// Accepts a succeeded run after review: its final revision may then be previewed and
/// published like any other (publishing still needs a fresh login).
#[utoipa::path(
    post,
    path = "/admin/v1/themes/ai-runs/{id}/accept",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RunSummary),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition | revision_not_ready", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn accept_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RunSummary>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = staff.user.user_id.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            ai_edit::accept(tx, &actor, id).await
        })
        .await?,
    ))
}

/// Discards a succeeded run: its revisions stay unpublishable.
#[utoipa::path(
    post,
    path = "/admin/v1/themes/ai-runs/{id}/discard",
    tag = "themes",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = RunSummary),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn discard_run(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<RunSummary>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = staff.user.user_id.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            ai_edit::discard(tx, &actor, id).await
        })
        .await?,
    ))
}
