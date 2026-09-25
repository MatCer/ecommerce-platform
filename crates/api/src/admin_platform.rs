//! Superadmin Admin API (spec §13): the job queue across tenants, dead jobs and requeue.
//! Only users listed in `platform.platform_admins`; no `X-Tenant-Id`.

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use platform::Error;
use platform::queue::{self, JobInfo};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::query_params;
use crate::auth::StaffUser;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_platform_jobs))
        .routes(routes!(requeue_platform_job))
}

/// Whether a staff user is a platform superadmin.
pub async fn is_superadmin(db: &sqlx::PgPool, user_id: &str) -> Result<bool, Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM platform.platform_admins WHERE user_id = $1) AS "ok!""#,
        user_id
    )
    .fetch_one(db)
    .await?)
}

/// A verified staff user who is a platform superadmin.
pub struct Superadmin(pub StaffUser);

impl FromRequestParts<AppState> for Superadmin {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Error> {
        let user = StaffUser::from_request_parts(parts, state).await?;
        if !is_superadmin(&state.db, &user.user_id).await? {
            return Err(Error::Forbidden {
                code: "not_a_superadmin",
            });
        }
        Ok(Self(user))
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobQuery {
    /// `queued`, `running`, `done` or `dead` (default `dead`).
    pub status: Option<String>,
    /// Only this job kind (e.g. `media.process`).
    pub kind: Option<String>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<i64>,
    /// Page size, 1-200 (default 50).
    pub limit: Option<i32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobPage {
    pub items: Vec<JobInfo>,
    pub next_cursor: Option<i64>,
}

/// Jobs of every tenant, newest first; dead ones by default.
#[utoipa::path(
    get,
    path = "/admin/v1/platform/jobs",
    tag = "platform",
    security(("staff_jwt" = [])),
    params(JobQuery),
    responses(
        (status = 200, body = JobPage),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_platform_jobs(
    _admin: Superadmin,
    State(s): State<AppState>,
    query: Result<Query<JobQuery>, QueryRejection>,
) -> Result<Json<JobPage>, Error> {
    let q = query_params(query)?;
    let status = q.status.as_deref().unwrap_or("dead");
    if !matches!(status, "queued" | "running" | "done" | "dead") {
        return Err(Error::BadRequest {
            code: "invalid_query",
            detail: "status must be queued, running, done or dead".into(),
        });
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let mut items =
        queue::list(&s.db, Some(status), q.kind.as_deref(), q.cursor, limit + 1).await?;
    let more = items.len() > usize::try_from(limit).unwrap_or(usize::MAX);
    items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = if more {
        items.last().map(|j| j.id)
    } else {
        None
    };
    Ok(Json(JobPage { items, next_cursor }))
}

/// Puts a dead job back in the queue with fresh attempts (`409 not_dead` otherwise).
#[utoipa::path(
    post,
    path = "/admin/v1/platform/jobs/{id}/requeue",
    tag = "platform",
    security(("staff_jwt" = [])),
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = 204, description = "Requeued"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn requeue_platform_job(
    admin: Superadmin,
    State(s): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<axum::http::StatusCode, Error> {
    let Path(id) = path.map_err(|_| Error::NotFound)?;
    if !queue::requeue(&s.db, id).await? {
        return Err(Error::Conflict {
            code: "not_dead",
            detail: "only dead jobs can be requeued".into(),
        });
    }
    tracing::info!(job = id, user = %admin.0.user_id, "dead job requeued by superadmin");
    Ok(axum::http::StatusCode::NO_CONTENT)
}
