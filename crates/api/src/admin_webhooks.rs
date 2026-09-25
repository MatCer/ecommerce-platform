//! Webhooks Admin API (spec §8.5, A21): subscriptions, the delivery log and redelivery.
//! Owner/admin only; changing where data goes (create, update, rotate, delete) also needs a
//! login under 15 minutes old (A9), like other data-export operations.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use commerce::tenancy::Role;
use commerce::webhooks::{
    self, Delivery, DeliveryPage, NewSubscription, Subscription, SubscriptionList,
    SubscriptionUpdate, SubscriptionWithSecret, Webhooks,
};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(update, remove))
        .routes(routes!(rotate))
        .routes(routes!(deliveries))
        .routes(routes!(redeliver))
}

fn configured(s: &AppState) -> Result<&Webhooks, Error> {
    s.webhooks
        .as_ref()
        .ok_or_else(|| Error::Unavailable("SECRETS_KEY is not configured".into()))
}

fn admin(staff: &TenantStaff, sensitive: bool) -> Result<(), Error> {
    staff.require(Role::Admin)?;
    if sensitive {
        staff.require_fresh_auth()?;
    }
    Ok(())
}

/// Subscriptions (never their secrets) and the event types on offer.
#[utoipa::path(
    get,
    path = "/admin/v1/webhooks",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = SubscriptionList),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<SubscriptionList>, Error> {
    admin(&staff, false)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| webhooks::list(tx).await).await?,
    ))
}

/// Creates a subscription. The signing secret is in this response only.
#[utoipa::path(
    post,
    path = "/admin/v1/webhooks",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = NewSubscription,
    responses(
        (status = 201, body = SubscriptionWithSecret),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<SubscriptionWithSecret>), Error> {
    admin(&staff, true)?;
    let hooks = configured(&s)?;
    let input: NewSubscription = parse_json(&body)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::create(tx, hooks, &staff.user.user_id, &input).await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// Changes the URL, event types, description or active flag.
#[utoipa::path(
    patch,
    path = "/admin/v1/webhooks/{id}",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = SubscriptionUpdate,
    responses(
        (status = 200, body = Subscription),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Subscription>, Error> {
    admin(&staff, true)?;
    let hooks = configured(&s)?;
    let id = path_id(path)?;
    let input: SubscriptionUpdate = parse_json(&body)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::update(tx, hooks, &staff.user.user_id, id, &input).await
    })
    .await?;
    Ok(Json(out))
}

/// Deletes a subscription and its delivery log.
#[utoipa::path(
    delete,
    path = "/admin/v1/webhooks/{id}",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn remove(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    admin(&staff, true)?;
    let id = path_id(path)?;
    in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::delete(tx, &staff.user.user_id, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replaces the signing secret (the old one stops working at once); the new secret is in
/// this response only.
#[utoipa::path(
    post,
    path = "/admin/v1/webhooks/{id}/rotate-secret",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = SubscriptionWithSecret),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn rotate(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<SubscriptionWithSecret>, Error> {
    admin(&staff, true)?;
    let hooks = configured(&s)?;
    let id = path_id(path)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::rotate_secret(tx, hooks, &staff.user.user_id, id).await
    })
    .await?;
    Ok(Json(out))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryQuery {
    /// Only this subscription's deliveries.
    pub subscription_id: Option<Uuid>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// The delivery log, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/webhooks/deliveries",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, DeliveryQuery),
    responses((status = 200, body = DeliveryPage))
)]
async fn deliveries(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<DeliveryQuery>, QueryRejection>,
) -> Result<Json<DeliveryPage>, Error> {
    admin(&staff, false)?;
    let q = query_params(query)?;
    let page = in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::deliveries(tx, q.subscription_id, q.cursor, q.limit.unwrap_or(50)).await
    })
    .await?;
    Ok(Json(page))
}

/// Sends a finished (succeeded or dead) delivery again, with a new 24 h retry window.
#[utoipa::path(
    post,
    path = "/admin/v1/webhooks/deliveries/{id}/redeliver",
    tag = "webhooks-admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 202, body = Delivery),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn redeliver(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<(StatusCode, Json<Delivery>), Error> {
    admin(&staff, false)?;
    let id = path_id(path)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        webhooks::redeliver(tx, &staff.user.user_id, id).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(out)))
}
