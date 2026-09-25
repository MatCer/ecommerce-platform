//! Newsletter (spec §11.5, A20, WP18). Sign-up comes from the theme's form on the shop origin
//! (`/_p/newsletter`); confirmation, the preference page, unsubscribe (incl. the RFC 8058
//! one-click POST) and tracked clicks are served on the checkout origin
//! (`checkout.<shop>/newsletter`, `/_p/newsletter/*`). Tokens are capabilities (256-bit,
//! hashed at rest) and travel only in these calls.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::marketing::campaigns::{self, Preferences};
use commerce::marketing::subscribers::{self, Confirmation};
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
        .routes(routes!(subscribe))
        .routes(routes!(confirmation, confirm))
        .routes(routes!(preferences))
        .routes(routes!(unsubscribe))
        .routes(routes!(resubscribe))
        .routes(routes!(click))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewsletterSignup {
    pub email: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NewsletterStatus {
    /// `accepted` (a confirmation mail is sent when one is due; the answer never tells whether
    /// the address is already known) or `subscribed`.
    pub status: String,
}

/// A capability token from a newsletter email.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewsletterToken {
    pub token: String,
}

fn accepted(status: &str) -> Response {
    no_store(
        (
            StatusCode::ACCEPTED,
            Json(NewsletterStatus {
                status: status.into(),
            }),
        )
            .into_response(),
    )
}

/// Newsletter sign-up (double opt-in, §11.5): always `202 accepted` for a valid address.
#[utoipa::path(
    post,
    path = "/storefront/v1/newsletter/subscribe",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = NewsletterSignup,
    responses(
        (status = 202, body = NewsletterStatus),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_signups", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn subscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewsletterSignup = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        subscribers::subscribe(tx, ctx, &input.email, ip.as_deref(), "form").await
    })
    .await?;
    Ok(accepted("accepted"))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TokenQuery {
    /// The token from the email link.
    pub token: String,
}

/// What the confirmation page shows (reading changes nothing: link scanners cannot confirm).
#[utoipa::path(
    get,
    path = "/storefront/v1/newsletter/confirmation",
    tag = "storefront",
    params(StorefrontHeaders, TokenQuery),
    responses(
        (status = 200, body = Confirmation),
        (status = 404, description = "Invalid, used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn confirmation(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<TokenQuery>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?;
    let c = with_ctx(&s, &shopper, async |tx, _| {
        subscribers::confirmation(tx, &q.token).await
    })
    .await?;
    Ok(no_store(Json(c).into_response()))
}

/// Confirms a sign-up (the button on the confirmation page): subscribed, consent recorded.
#[utoipa::path(
    post,
    path = "/storefront/v1/newsletter/confirmation",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = NewsletterToken,
    responses(
        (status = 200, body = NewsletterStatus),
        (status = 404, description = "Invalid, used or expired", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn confirm(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewsletterToken = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, _| {
        let ip = ip_hash(tx, &headers).await?;
        subscribers::confirm(tx, &input.token, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        Json(NewsletterStatus {
            status: "subscribed".into(),
        })
        .into_response(),
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SendTokenQuery {
    /// The recipient token from a campaign email.
    pub t: String,
}

/// The preference page's view of the subscription behind a campaign email.
#[utoipa::path(
    get,
    path = "/storefront/v1/newsletter/preferences",
    tag = "storefront",
    params(StorefrontHeaders, SendTokenQuery),
    responses(
        (status = 200, body = Preferences),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preferences(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<SendTokenQuery>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?;
    let p = with_ctx(&s, &shopper, async |tx, _| {
        campaigns::preferences(tx, &q.t).await
    })
    .await?;
    Ok(no_store(Json(p).into_response()))
}

/// Unsubscribes the recipient of a campaign email (RFC 8058 one-click, the preference page).
/// Idempotent.
#[utoipa::path(
    post,
    path = "/storefront/v1/newsletter/unsubscribe",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = NewsletterToken,
    responses(
        (status = 200, body = Preferences),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn unsubscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewsletterToken = parse_json(&body)?;
    let p = with_ctx(&s, &shopper, async |tx, _| {
        let ip = ip_hash(tx, &headers).await?;
        campaigns::unsubscribe(tx, &input.token, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(Json(p).into_response()))
}

/// Subscribing again from the preference page: a new double opt-in mail to the same address.
#[utoipa::path(
    post,
    path = "/storefront/v1/newsletter/resubscribe",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = NewsletterToken,
    responses(
        (status = 202, body = NewsletterStatus),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn resubscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewsletterToken = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        campaigns::resubscribe(tx, ctx, &input.token, ip.as_deref()).await
    })
    .await?;
    Ok(accepted("accepted"))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClickQuery {
    /// Recipient token.
    pub t: String,
    /// Target URL.
    pub u: String,
    /// Signature (hex).
    pub s: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ClickTarget {
    /// Where to redirect (exactly what the campaign linked to).
    pub url: String,
}

/// A tracked campaign link: counts the click and returns the signed target; `404` for anything
/// not signed for this recipient (never an open redirect).
#[utoipa::path(
    get,
    path = "/storefront/v1/newsletter/click",
    tag = "storefront",
    params(StorefrontHeaders, ClickQuery),
    responses(
        (status = 200, body = ClickTarget),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn click(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<ClickQuery>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?;
    let url = with_ctx(&s, &shopper, async |tx, _| {
        campaigns::click(tx, &q.t, &q.u, &q.s).await
    })
    .await?;
    Ok(no_store(Json(ClickTarget { url }).into_response()))
}
