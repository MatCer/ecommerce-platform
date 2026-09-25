//! Cart and checkout handoff (spec §8.2, §10.3, A1, A4). The edge maps the shop origin's
//! `/_p/cart/*` to these endpoints and turns the `cart` cookie into `X-Cart-Token`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use commerce::cart::{self, CartRef, CartView, CouponCode, LineUpdate, NewLine, Scope};
use commerce::idempotency;
use commerce::storefront::Context;
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{CART_HEADER, CartHeader, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::{REPLAYED, idempotency_key, parse_json};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_cart, get_cart))
        .routes(routes!(add_line))
        .routes(routes!(update_line, remove_line))
        .routes(routes!(apply_coupon))
        .routes(routes!(remove_coupon))
        .routes(routes!(start_handoff))
        .routes(routes!(redeem_handoff))
        .routes(routes!(events))
        .routes(routes!(newsletter))
}

fn token(headers: &HeaderMap) -> Result<String, Error> {
    headers
        .get(CART_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or(Error::NotFound)
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// A cart mutation's identity for `Idempotency-Key` (§8.1, A12): the operation and a hash of
/// the request (path parameters and body).
struct Mutation {
    op: &'static str,
    hash: String,
}

fn mutation(op: &'static str, request: &[u8]) -> Option<Mutation> {
    Some(Mutation {
        op,
        hash: idempotency::request_hash(request),
    })
}

/// Finds the cart behind the capability (`scope`: required capability, `None` = either),
/// runs `f` on it and answers with the recomputed cart. A mutation with an `Idempotency-Key`
/// runs once per cart and key: a retry replays the stored response (`Idempotent-Replayed`),
/// the same key with another request is `409 idempotency_conflict`.
async fn with_cart(
    s: &AppState,
    shopper: &Shopper,
    headers: &HeaderMap,
    scope: Option<Scope>,
    change: Option<Mutation>,
    f: impl AsyncFnOnce(&mut TenantTx, &Context, &CartRef) -> Result<(), Error>,
) -> Result<Response, Error> {
    let token = token(headers)?;
    let key = match &change {
        Some(_) => idempotency_key(headers)?,
        None => None,
    };
    let (view, replayed) = with_ctx(s, shopper, async |tx, ctx| {
        let cart = cart::find(tx, ctx, &token, scope).await?;
        // Keys are scoped to the cart: two carts never share an idempotency record.
        let op = change
            .as_ref()
            .map(|m| format!("{} cart:{}", m.op, cart.id));
        if let (Some(key), Some(op), Some(m)) = (&key, &op, &change)
            && let Some(stored) = idempotency::begin(tx, op, key, &m.hash).await?
        {
            return Ok((stored.body, true));
        }
        f(tx, ctx, &cart).await?;
        let view = serde_json::to_value(cart::view(tx, ctx, &cart).await?)
            .map_err(|e| Error::Internal(e.to_string()))?;
        if let (Some(key), Some(op)) = (&key, &op) {
            idempotency::finish(tx, op, key, StatusCode::OK.as_u16(), &view).await?;
        }
        Ok((view, false))
    })
    .await?;
    let mut res = no_store(Json(view).into_response());
    if replayed {
        res.headers_mut()
            .insert(REPLAYED, HeaderValue::from_static("true"));
    }
    Ok(res)
}

/// Creates an empty cart; the capability comes back in `X-Cart-Token` (the edge stores it in
/// the HttpOnly `cart` cookie).
#[utoipa::path(
    post,
    path = "/storefront/v1/cart",
    tag = "storefront",
    params(StorefrontHeaders),
    responses((status = 201, body = CartView, headers(("X-Cart-Token" = String, description = "Shop cart capability"))))
)]
async fn create_cart(shopper: Shopper, State(s): State<AppState>) -> Result<Response, Error> {
    let (view, token) = with_ctx(&s, &shopper, async |tx, ctx| {
        let (id, token) = cart::create(tx, ctx).await?;
        let r = CartRef {
            id,
            market_id: ctx.market.id,
            scope: Scope::Shop,
        };
        Ok((cart::view(tx, ctx, &r).await?, token))
    })
    .await?;
    let mut res = (StatusCode::CREATED, Json(view)).into_response();
    res.headers_mut().insert(
        CART_HEADER,
        HeaderValue::from_str(&token).map_err(|e| Error::Internal(e.to_string()))?,
    );
    Ok(no_store(res))
}

/// The cart with totals and VAT recomputed now (shop or checkout capability).
#[utoipa::path(
    get,
    path = "/storefront/v1/cart",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    responses(
        (status = 200, body = CartView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_cart(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    with_cart(&s, &shopper, &headers, None, None, async |_, _, _| Ok(())).await
}

#[utoipa::path(
    post,
    path = "/storefront/v1/cart/lines",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = NewLine,
    responses(
        (status = 200, body = CartView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "out_of_stock / insufficient_stock", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn add_line(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let line: NewLine = parse_json(&body)?;
    let res = with_cart(
        &s,
        &shopper,
        &headers,
        Some(Scope::Shop),
        mutation("POST /cart/lines", &body),
        async |tx, ctx, c| cart::add_line(tx, ctx, c, &line).await,
    )
    .await?;
    if !res.headers().contains_key(REPLAYED) {
        let event = json!({ "type": "add_to_cart", "variant_id": line.variant_id,
                            "quantity": line.quantity, "template": "product" });
        track(&s, &shopper, &headers, &event).await;
    }
    Ok(res)
}

/// A20: a consented visitor's cart step joins their analytics session, recorded here (no
/// client script needed). Best effort after the change; nothing is stored unless the consent
/// records of the subject (`X-Consent-Subject`, the edge's cookie) grant `analytics`.
async fn track(s: &AppState, shopper: &Shopper, headers: &HeaderMap, event: &serde_json::Value) {
    let Some(subject) =
        super::customer::header_str(headers, super::customer::CONSENT_SUBJECT_HEADER)
    else {
        return;
    };
    let body = json!({ "events": [event] }).to_string();
    let ua = super::customer::header_str(headers, super::customer::CLIENT_UA_HEADER);
    let stored = with_ctx(s, shopper, async |tx, ctx| {
        commerce::analytics::ingest(tx, ctx.market.id, Some(subject), body.as_bytes(), ctx.now)
            .await?;
        // WP20: forwarded to ad platforms only while the subject grants `ads`.
        let events: Vec<_> = commerce::analytics::clean_event(event)
            .into_iter()
            .collect();
        commerce::adtracking::capture_events(tx, ctx.market.id, Some(subject), &events, ua, ctx.now)
            .await
    })
    .await;
    if let Err(e) = stored {
        tracing::warn!(error = %e, "recording a cart analytics event failed");
    }
}

fn line_id(path: Result<Path<Uuid>, PathRejection>) -> Result<Uuid, Error> {
    path.map(|Path(id)| id).map_err(|_| Error::NotFound)
}

/// Sets the quantity (0 removes the line).
#[utoipa::path(
    patch,
    path = "/storefront/v1/cart/lines/{id}",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader, ("id" = Uuid, Path)),
    request_body = LineUpdate,
    responses((status = 200, body = CartView))
)]
async fn update_line(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Response, Error> {
    let id = line_id(id)?;
    let update: LineUpdate = parse_json(&body)?;
    with_cart(
        &s,
        &shopper,
        &headers,
        Some(Scope::Shop),
        mutation(
            "PATCH /cart/lines",
            &[id.as_bytes().as_slice(), &body].concat(),
        ),
        async |tx, _, c| cart::update_line(tx, c, id, &update).await,
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/storefront/v1/cart/lines/{id}",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = CartView))
)]
async fn remove_line(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = line_id(id)?;
    with_cart(
        &s,
        &shopper,
        &headers,
        Some(Scope::Shop),
        mutation("DELETE /cart/lines", id.as_bytes()),
        async |tx, _, c| cart::remove_line(tx, c, id).await,
    )
    .await
}

/// Applies a coupon (one per cart; replaces an earlier one). `422` with the coupon's reason
/// (`coupon_not_found`, `coupon_min_subtotal`, ...) when it does not apply.
#[utoipa::path(
    post,
    path = "/storefront/v1/cart/coupons",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    request_body = CouponCode,
    responses(
        (status = 200, body = CartView),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn apply_coupon(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let code: CouponCode = parse_json(&body)?;
    with_cart(
        &s,
        &shopper,
        &headers,
        Some(Scope::Shop),
        mutation("POST /cart/coupons", &body),
        async |tx, ctx, c| cart::apply_coupon(tx, ctx, c, &code.code).await,
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/storefront/v1/cart/coupons/{code}",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader, ("code" = String, Path)),
    responses((status = 200, body = CartView))
)]
async fn remove_coupon(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    code: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let Path(code) = code.map_err(|_| Error::NotFound)?;
    with_cart(
        &s,
        &shopper,
        &headers,
        Some(Scope::Shop),
        mutation("DELETE /cart/coupons", code.as_bytes()),
        async |tx, _, c| cart::remove_coupon(tx, c, &code).await,
    )
    .await
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HandoffToken {
    /// Single use, valid for 60 seconds, for `checkout.<shop>/start?h=`.
    pub token: String,
}

/// Starts the checkout handoff (A1): revokes the shop capability (the edge clears the `cart`
/// cookie) and returns a single-use handoff token.
#[utoipa::path(
    post,
    path = "/storefront/v1/cart/handoff",
    tag = "storefront",
    params(StorefrontHeaders, CartHeader),
    responses(
        (status = 200, body = HandoffToken),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "cart_empty", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn start_handoff(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let token = token(&headers)?;
    let handoff = with_ctx(&s, &shopper, async |tx, ctx| {
        let c = cart::find(tx, ctx, &token, Some(Scope::Shop)).await?;
        cart::start_handoff(tx, &c).await
    })
    .await?;
    track(
        &s,
        &shopper,
        &headers,
        &json!({ "type": "begin_checkout", "template": "other" }),
    )
    .await;
    Ok(no_store(
        Json(HandoffToken { token: handoff }).into_response(),
    ))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CheckoutCart {
    /// Checkout-scoped cart capability (the edge sets it as the `__Host-cart` cookie).
    pub cart_token: String,
}

/// Redeems a handoff token on the checkout origin (A1): consumed atomically; returns a new
/// checkout-scoped capability. `400 invalid_handoff` when unknown, used, expired or minted for
/// another market.
#[utoipa::path(
    post,
    path = "/storefront/v1/checkout/handoff",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = HandoffToken,
    responses(
        (status = 200, body = CheckoutCart),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn redeem_handoff(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: HandoffToken = parse_json(&body)?;
    let cart_token = with_ctx(&s, &shopper, async |tx, ctx| {
        cart::redeem_handoff(tx, ctx.market.id, &input.token).await
    })
    .await?
    .ok_or_else(|| Error::BadRequest {
        code: "invalid_handoff",
        detail: "the checkout link expired or was already used".into(),
    })?;
    Ok(no_store(Json(CheckoutCart { cart_token }).into_response()))
}

/// Events beacon (`/_p/e`, A20): `{"events": [...]}` with `page_view`, `view_item`,
/// `add_to_cart`, `begin_checkout` and `web_vital` events. Stored only when the consent
/// records of the anonymous subject (`X-Consent-Subject`, the edge's consent cookie) grant
/// `analytics` right now; purposes the client claims are ignored and unknown props dropped.
/// Page views and shopping steps also go to the enabled ad platforms while the subject grants
/// `ads` (WP20). Always `202`, so the answer does not reveal the consent state.
#[utoipa::path(
    post,
    path = "/storefront/v1/events",
    tag = "storefront",
    params(StorefrontHeaders, super::consent::ConsentHeaders),
    responses((status = 202, description = "Accepted"))
)]
async fn events(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let subject = super::customer::header_str(&headers, super::customer::CONSENT_SUBJECT_HEADER);
    let ua = super::customer::header_str(&headers, super::customer::CLIENT_UA_HEADER);
    let (stored, forwarded) = with_ctx(&s, &shopper, async |tx, ctx| {
        let stored =
            commerce::analytics::ingest(tx, ctx.market.id, subject, &body, ctx.now).await?;
        // WP20: the same events go to the ad platforms only while the subject grants `ads`
        // (resolved from the consent records here and again when sending).
        let events = commerce::analytics::parse_batch(&body);
        let forwarded =
            commerce::adtracking::capture_events(tx, ctx.market.id, subject, &events, ua, ctx.now)
                .await?;
        Ok((stored, forwarded))
    })
    .await?;
    tracing::debug!(stored, forwarded, "events beacon");
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewsletterSignup {
    pub email: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NewsletterStatus {
    /// `accepted`: double opt-in mail arrives with the newsletter module (M2, §11.5).
    pub status: String,
}

/// Newsletter sign-up. ponytail: validates and accepts; storage and the double opt-in mail
/// arrive with the newsletter module (M2).
#[utoipa::path(
    post,
    path = "/storefront/v1/newsletter/subscribe",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = NewsletterSignup,
    responses(
        (status = 202, body = NewsletterStatus),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn newsletter(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewsletterSignup = parse_json(&body)?;
    let email = input.email.trim();
    let valid = email.len() <= 254
        && email
            .split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'))
        && !email.chars().any(char::is_whitespace);
    if !valid {
        return Err(Error::Validation {
            code: "invalid_email",
            detail: "not an email address".into(),
        });
    }
    with_ctx(&s, &shopper, async |_, _| Ok(())).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(NewsletterStatus {
            status: "accepted".into(),
        }),
    )
        .into_response())
}
