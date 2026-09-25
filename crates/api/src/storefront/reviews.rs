//! Reviews (spec §11.6, §11.7, WP16). The review form lives on the checkout origin
//! (`checkout.<shop>/review?token=`, posted to `/_p/reviews`); the token is a capability from
//! a review link (one per delivered order line, single use) and travels only in these calls.
//! Published reviews reach the shop through the product page model.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::reviews::{self, ReviewInput, ReviewInvitation};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::customer::{ip_hash, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::{parse_json, query_params};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(invitation))
        .routes(routes!(submit))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReviewTokenQuery {
    /// The token from the review link.
    pub token: String,
}

/// What the review form shows for a link (reading changes nothing).
#[utoipa::path(
    get,
    path = "/storefront/v1/reviews/invitation",
    tag = "storefront",
    params(StorefrontHeaders, ReviewTokenQuery),
    responses(
        (status = 200, body = ReviewInvitation),
        (status = 404, description = "Invalid, used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn invitation(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<ReviewTokenQuery>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?;
    let inv = with_ctx(&s, &shopper, async |tx, _| {
        reviews::invitation(tx, &q.token).await
    })
    .await?;
    Ok(no_store(Json(inv).into_response()))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReviewSubmitted {
    /// Always `pending`: the shop publishes reviews after moderation.
    pub status: String,
}

/// Submits a review with a review link's token (single use). Plain text only; the review
/// waits for moderation.
#[utoipa::path(
    post,
    path = "/storefront/v1/reviews",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = ReviewInput,
    responses(
        (status = 201, body = ReviewSubmitted),
        (status = 404, description = "Invalid, used or expired token", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_reviews", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn submit(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ReviewInput = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        reviews::submit(tx, ctx, &input, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(ReviewSubmitted {
                status: "pending".into(),
            }),
        )
            .into_response(),
    ))
}
