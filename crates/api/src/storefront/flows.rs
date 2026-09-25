//! Watch subscriptions and checkout-origin capability actions (WP19).
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use commerce::flows::{self, WatchInput, WatchStatus};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::customer::{ip_hash, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(subscribe))
        .routes(routes!(confirm))
        .routes(routes!(unsubscribe))
        .routes(routes!(unsubscribe_cart))
        .routes(routes!(restore))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenInput {
    pub token: String,
}
#[derive(Serialize, ToSchema)]
pub struct RestoreResult {
    pub cart_token: String,
}

#[utoipa::path(post,path="/storefront/v1/watch/subscribe",tag="storefront",params(StorefrontHeaders),request_body=WatchInput,responses((status=202,body=WatchStatus)))]
async fn subscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: WatchInput = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        flows::subscribe_watch_with_ip(tx, ctx, &input, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(
        (
            axum::http::StatusCode::ACCEPTED,
            Json(WatchStatus { status: "accepted" }),
        )
            .into_response(),
    ))
}

#[utoipa::path(post,path="/storefront/v1/watch/confirm",tag="storefront",params(StorefrontHeaders),request_body=TokenInput,responses((status=200,body=WatchStatus),(status=404)))]
async fn confirm(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: TokenInput = parse_json(&body)?;
    let yes = with_ctx(&s, &shopper, async |tx, _| {
        flows::confirm_watch(tx, &input.token, chrono::Utc::now()).await
    })
    .await?;
    if !yes {
        return Err(Error::NotFound);
    }
    Ok(no_store(
        Json(WatchStatus {
            status: "confirmed",
        })
        .into_response(),
    ))
}

#[utoipa::path(post,path="/storefront/v1/watch/unsubscribe",tag="storefront",params(StorefrontHeaders),request_body=TokenInput,responses((status=200,body=WatchStatus)))]
async fn unsubscribe(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: TokenInput = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, _| {
        flows::unsubscribe_watch(tx, &input.token).await
    })
    .await?;
    Ok(no_store(
        Json(WatchStatus {
            status: "unsubscribed",
        })
        .into_response(),
    ))
}

#[utoipa::path(post,path="/storefront/v1/flows/unsubscribe",tag="storefront",params(StorefrontHeaders),request_body=TokenInput,responses((status=200,body=WatchStatus)))]
async fn unsubscribe_cart(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: TokenInput = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, _| {
        flows::unsubscribe_cart(tx, &input.token).await
    })
    .await?;
    Ok(no_store(
        Json(WatchStatus {
            status: "unsubscribed",
        })
        .into_response(),
    ))
}

#[utoipa::path(post,path="/storefront/v1/flows/restore-cart",tag="storefront",params(StorefrontHeaders),request_body=TokenInput,responses((status=200,body=RestoreResult),(status=404)))]
async fn restore(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: TokenInput = parse_json(&body)?;
    let token = with_ctx(&s, &shopper, async |tx, ctx| {
        flows::restore_cart(tx, ctx.market.id, &input.token, chrono::Utc::now()).await
    })
    .await?
    .ok_or(Error::NotFound)?;
    Ok(no_store(
        Json(RestoreResult { cart_token: token }).into_response(),
    ))
}
