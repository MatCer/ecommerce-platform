//! Customer accounts (spec §5.4, A1, A4, A5), reached only from the checkout origin: the edge
//! maps `checkout.<shop>/_p/account/*` here, turns the `__Host-sid` cookie into
//! `X-Customer-Session`, enforces same-origin JSON (CSRF, §14) and forwards the client IP.
//!
//! Credentials never appear in bodies: a new session token comes back in `X-Session-Token`
//! (the edge stores it as the cookie and strips the header), `X-Session-Clear: 1` asks the
//! edge to delete the cookie.

use std::net::IpAddr;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use commerce::customers::{
    self, Address, AddressInput, CustomerView, MagicLinkRequest, MagicLinkToken, PasswordChange,
    PasswordLogin, PasswordOutcome, SignedIn,
};
use commerce::orders::{self, OrderPage, OrderView};
use commerce::privacy;
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{CART_HEADER, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::{IdParam, parse_json, path_id};

pub const SESSION_HEADER: &str = "x-customer-session";
pub const SESSION_TOKEN_HEADER: &str = "x-session-token";
pub const SESSION_CLEAR_HEADER: &str = "x-session-clear";
pub const CONSENT_SUBJECT_HEADER: &str = "x-consent-subject";
pub const CLIENT_IP_HEADER: &str = "x-client-ip";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(request_magic_link))
        .routes(routes!(consume_magic_link))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(set_password))
        .routes(routes!(list_addresses, add_address))
        .routes(routes!(update_address, delete_address))
        .routes(routes!(my_orders))
        .routes(routes!(my_order))
}

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

pub(crate) fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    header_str(headers, CLIENT_IP_HEADER).and_then(|v| v.parse().ok())
}

pub(crate) async fn ip_hash(
    tx: &mut TenantTx,
    headers: &HeaderMap,
) -> Result<Option<Vec<u8>>, Error> {
    match client_ip(headers) {
        Some(ip) => Ok(Some(privacy::ip_hash(tx, ip).await?)),
        None => Ok(None),
    }
}

pub(crate) fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// The customer's session (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct SessionHeader {
    /// Session token from the checkout origin's `__Host-sid` cookie (set by the edge).
    #[param(rename = "X-Customer-Session")]
    x_customer_session: String,
}

/// What the edge adds to sign-in calls (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct SignInHeaders {
    /// Checkout cart capability (`__Host-cart`): the cart is attached to the customer (A4).
    #[param(rename = "X-Cart-Token")]
    x_cart_token: Option<String>,
    /// Anonymous consent subject (consent cookie): its choices move to the customer (A20).
    #[param(rename = "X-Consent-Subject")]
    x_consent_subject: Option<String>,
    /// The client's IP (rate limits; stored only as a salted hash).
    #[param(rename = "X-Client-Ip")]
    x_client_ip: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SignInResult {
    pub customer: CustomerView,
    /// Relative path on the checkout origin to continue to.
    pub redirect: String,
}

fn signed_in(s: SignedIn) -> Result<Response, Error> {
    let mut res = Json(SignInResult {
        customer: s.customer,
        redirect: s.redirect,
    })
    .into_response();
    res.headers_mut().insert(
        SESSION_TOKEN_HEADER,
        HeaderValue::from_str(&s.session_token).map_err(|e| Error::Internal(e.to_string()))?,
    );
    Ok(no_store(res))
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/magic-link",
    tag = "storefront",
    params(StorefrontHeaders, SignInHeaders),
    request_body = MagicLinkRequest,
    responses(
        (status = 202, description = "Sent if the address can receive mail (same answer for every address)"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_attempts", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn request_magic_link(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: MagicLinkRequest = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        customers::request_magic_link(tx, ctx, &input, ip.as_deref()).await
    })
    .await?;
    Ok(no_store(StatusCode::ACCEPTED.into_response()))
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/magic-link/consume",
    tag = "storefront",
    params(StorefrontHeaders, SignInHeaders),
    request_body = MagicLinkToken,
    responses(
        (status = 200, body = SignInResult, headers(("X-Session-Token" = String, description = "New session (for the edge only)"))),
        (status = 400, description = "invalid_magic_link", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn consume_magic_link(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: MagicLinkToken = parse_json(&body)?;
    let cart = header_str(&headers, CART_HEADER);
    let anon = header_str(&headers, CONSENT_SUBJECT_HEADER);
    let signed = with_ctx(&s, &shopper, async |tx, ctx| {
        customers::consume_magic_link(tx, ctx, &input.token, cart, anon).await
    })
    .await?;
    signed_in(signed)
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/login",
    tag = "storefront",
    params(StorefrontHeaders, SignInHeaders),
    request_body = PasswordLogin,
    responses(
        (status = 200, body = SignInResult, headers(("X-Session-Token" = String, description = "New session (for the edge only)"))),
        (status = 401, description = "invalid_credentials", body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_attempts", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn login(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: PasswordLogin = parse_json(&body)?;
    let cart = header_str(&headers, CART_HEADER);
    let anon = header_str(&headers, CONSENT_SUBJECT_HEADER);
    // A failed attempt is committed (rate limit) before the 401.
    let signed = with_ctx(&s, &shopper, async |tx, ctx| {
        let ip = ip_hash(tx, &headers).await?;
        customers::login(tx, ctx, &input, cart, anon, ip.as_deref()).await
    })
    .await?
    .ok_or(Error::Unauthorized {
        code: "invalid_credentials",
    })?;
    signed_in(signed)
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/logout",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    responses((status = 204, headers(("X-Session-Clear" = String, description = "The edge deletes the cookie"))))
)]
async fn logout(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    if let Some(token) = header_str(&headers, SESSION_HEADER) {
        with_ctx(&s, &shopper, async |tx, _| {
            customers::logout(tx, token).await
        })
        .await?;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(SESSION_CLEAR_HEADER, HeaderValue::from_static("1"));
    Ok(no_store(res))
}

#[utoipa::path(
    get,
    path = "/storefront/v1/customer/me",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    responses(
        (status = 200, body = CustomerView),
        (status = 401, description = "not_signed_in", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn me(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let token = header_str(&headers, SESSION_HEADER);
    let view = with_ctx(&s, &shopper, async |tx, _| {
        let session = customers::require(tx, token).await?;
        customers::me(tx, &session).await
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}

/// Sets or changes the password (A5). Every other session is signed out; a notice is emailed.
#[utoipa::path(
    post,
    path = "/storefront/v1/customer/password",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    request_body = PasswordChange,
    responses(
        (status = 204),
        (status = 401, description = "not_signed_in", body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, description = "reauth_required | invalid_current_password", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "weak_password", body = platform::Problem, content_type = "application/problem+json"),
        (status = 429, description = "too_many_attempts", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn set_password(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: PasswordChange = parse_json(&body)?;
    let token = header_str(&headers, SESSION_HEADER);
    let outcome = with_ctx(&s, &shopper, async |tx, ctx| {
        let session = customers::require(tx, token).await?;
        let ip = ip_hash(tx, &headers).await?;
        customers::set_password(tx, ctx, &session, &input, ip.as_deref()).await
    })
    .await?;
    match outcome {
        PasswordOutcome::Changed => Ok(no_store(StatusCode::NO_CONTENT.into_response())),
        PasswordOutcome::WrongCurrentPassword => Err(Error::Forbidden {
            code: "invalid_current_password",
        }),
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddressList {
    pub items: Vec<Address>,
}

/// Runs `f` for the signed-in customer.
async fn as_customer<T>(
    s: &AppState,
    shopper: &Shopper,
    headers: &HeaderMap,
    f: impl AsyncFnOnce(&mut TenantTx, Uuid) -> Result<T, Error>,
) -> Result<T, Error> {
    let token = header_str(headers, SESSION_HEADER);
    with_ctx(s, shopper, async |tx, _| {
        let session = customers::require(tx, token).await?;
        f(tx, session.customer_id).await
    })
    .await
}

#[utoipa::path(
    get,
    path = "/storefront/v1/customer/addresses",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    responses(
        (status = 200, body = AddressList),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_addresses(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let items = as_customer(&s, &shopper, &headers, async |tx, c| {
        customers::addresses(tx, c).await
    })
    .await?;
    Ok(no_store(Json(AddressList { items }).into_response()))
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/addresses",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    request_body = AddressInput,
    responses(
        (status = 201, body = Address),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn add_address(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: AddressInput = parse_json(&body)?;
    let a = as_customer(&s, &shopper, &headers, async |tx, c| {
        customers::add_address(tx, c, &input).await
    })
    .await?;
    Ok(no_store((StatusCode::CREATED, Json(a)).into_response()))
}

#[utoipa::path(
    put,
    path = "/storefront/v1/customer/addresses/{id}",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    request_body = AddressInput,
    responses(
        (status = 200, body = Address),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_address(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Response, Error> {
    let id = path_id(path)?;
    let input: AddressInput = parse_json(&body)?;
    let a = as_customer(&s, &shopper, &headers, async |tx, c| {
        customers::update_address(tx, c, id, &input).await
    })
    .await?;
    Ok(no_store(Json(a).into_response()))
}

#[utoipa::path(
    delete,
    path = "/storefront/v1/customer/addresses/{id}",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    responses(
        (status = 204),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_address(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = path_id(path)?;
    as_customer(&s, &shopper, &headers, async |tx, c| {
        customers::delete_address(tx, c, id).await
    })
    .await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

/// The signed-in customer's orders, newest first: placed while signed in, or guest orders
/// with the account's email once it was verified (A5).
#[utoipa::path(
    get,
    path = "/storefront/v1/customer/orders",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader),
    responses(
        (status = 200, body = OrderPage),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn my_orders(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let page = as_customer(&s, &shopper, &headers, async |tx, c| {
        orders::list(tx, Some(c), None, None, 50).await
    })
    .await?;
    Ok(no_store(Json(page).into_response()))
}

#[utoipa::path(
    get,
    path = "/storefront/v1/customer/orders/{id}",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    responses(
        (status = 200, body = OrderView),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn my_order(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = path_id(path)?;
    let view = as_customer(&s, &shopper, &headers, async |tx, c| {
        if orders::customer_of(tx, id).await? != Some(c) {
            return Err(Error::NotFound);
        }
        orders::view(tx, id).await
    })
    .await?;
    Ok(no_store(Json(view).into_response()))
}
