//! Reviews Admin API (spec §11.7, WP16): the moderation queue, publish / reject / hide, and
//! the merchant's public reply. Staff role and up; every change is audited.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use commerce::reviews::{self, Review, ReviewPage, Status};
use platform::Error;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_reviews))
        .routes(routes!(set_review_status))
        .routes(routes!(set_review_reply))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReviewQuery {
    /// Only reviews in this state (the queue is `pending`).
    pub status: Option<Status>,
    pub product_id: Option<Uuid>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// Reviews, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/reviews",
    tag = "reviews",
    security(("staff_jwt" = [])),
    params(TenantHeader, ReviewQuery),
    responses((status = 200, body = ReviewPage))
)]
async fn list_reviews(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<ReviewQuery>, QueryRejection>,
) -> Result<Json<ReviewPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            reviews::list(tx, q.status, q.product_id, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewStatusInput {
    /// `published`, `rejected` (pending only) or `hidden` (published only).
    pub status: Status,
}

/// Publishes, rejects or hides a review.
#[utoipa::path(
    put,
    path = "/admin/v1/reviews/{id}/status",
    tag = "reviews",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ReviewStatusInput,
    responses(
        (status = 200, body = Review),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn set_review_status(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Review>, Error> {
    let id = path_id(id)?;
    let input: ReviewStatusInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            reviews::set_status(tx, actor, id, input.status).await
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewReplyInput {
    /// The public reply (plain text, ≤ 2000 characters); `null` or blank removes it.
    pub reply: Option<String>,
}

/// Sets or removes the merchant's public reply.
#[utoipa::path(
    put,
    path = "/admin/v1/reviews/{id}/reply",
    tag = "reviews",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ReviewReplyInput,
    responses(
        (status = 200, body = Review),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn set_review_reply(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Review>, Error> {
    let id = path_id(id)?;
    let input: ReviewReplyInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            reviews::set_reply(tx, actor, id, input.reply.as_deref()).await
        })
        .await?,
    ))
}
