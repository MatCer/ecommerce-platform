//! Withdrawal (A19) and document downloads on the checkout origin.
//!
//! Public form (`checkout.<host>/withdraw`): `POST /withdrawals` emails a confirmation link
//! (always `202`, no enumeration); the link's token opens the form (`GET /withdrawals/{token}`)
//! and the explicit confirmation consumes it (`POST /withdrawals/{token}`). Signed-in customers
//! use `/customer/orders/{id}/withdrawal`. Invoices and credit notes are listed with 5-minute
//! download links for the order-token page and the account (A21).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use commerce::invoicing::{self, document::DocumentKind};
use commerce::money::MoneyView;
use commerce::orders;
use commerce::withdrawals::{self, DeclareInput, LinkRequest, WithdrawalForm, WithdrawalReceipt};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::customer::{SessionHeader, as_customer, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::{IdParam, parse_json, path_id};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(request_link))
        .routes(routes!(form_by_token, declare_by_token))
        .routes(routes!(my_form, my_declare))
        .routes(routes!(order_documents))
        .routes(routes!(my_documents))
}

fn token(path: Result<Path<String>, PathRejection>) -> Result<String, Error> {
    path.map(|Path(t)| t).map_err(|_| Error::NotFound)
}

/// Step 1 of the public form: order number + email. If they match an order that can be
/// withdrawn from, a single-use confirmation link (24 h) is emailed. Always `202`.
#[utoipa::path(
    post,
    path = "/storefront/v1/withdrawals",
    tag = "storefront",
    params(StorefrontHeaders),
    request_body = LinkRequest,
    responses(
        (status = 202),
        (status = 422, description = "invalid_email", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn request_link(
    shopper: Shopper,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let input: LinkRequest = parse_json(&body)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        withdrawals::request_link(tx, ctx, &input).await
    })
    .await?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/withdrawals/{token}",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Emailed withdrawal link token")),
    responses(
        (status = 200, body = WithdrawalForm),
        (status = 404, description = "unknown, used or expired link", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn form_by_token(
    shopper: Shopper,
    State(s): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(path)?;
    let form = with_ctx(&s, &shopper, async |tx, _| {
        let order = withdrawals::order_by_token(tx, &t).await?;
        withdrawals::form(tx, order).await
    })
    .await?;
    Ok(no_store(Json(form).into_response()))
}

/// The explicit confirmation (A19): records the withdrawal, consumes the link and emails the
/// receipt with the full declaration.
#[utoipa::path(
    post,
    path = "/storefront/v1/withdrawals/{token}",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Emailed withdrawal link token")),
    request_body = DeclareInput,
    responses(
        (status = 201, body = WithdrawalReceipt),
        (status = 404, description = "unknown, used or expired link", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "not_withdrawable", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "confirmation_required | invalid_withdrawal | iban_required | invalid_iban", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn declare_by_token(
    shopper: Shopper,
    State(s): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<WithdrawalReceipt>), Error> {
    let t = token(path)?;
    let input: DeclareInput = parse_json(&body)?;
    let receipt = with_ctx(&s, &shopper, async |tx, ctx| {
        let order = withdrawals::consume_token(tx, &t).await?;
        withdrawals::declare(tx, ctx, order, &input, "web").await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

async fn own_order(tx: &mut TenantTx, order: Uuid, customer: Uuid) -> Result<(), Error> {
    if orders::customer_of(tx, order).await? == Some(customer) {
        Ok(())
    } else {
        Err(Error::NotFound)
    }
}

#[utoipa::path(
    get,
    path = "/storefront/v1/customer/orders/{id}/withdrawal",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    responses(
        (status = 200, body = WithdrawalForm),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn my_form(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = path_id(path)?;
    let form = as_customer(&s, &shopper, &headers, async |tx, c| {
        own_order(tx, id, c).await?;
        withdrawals::form(tx, id).await
    })
    .await?;
    Ok(no_store(Json(form).into_response()))
}

#[utoipa::path(
    post,
    path = "/storefront/v1/customer/orders/{id}/withdrawal",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    request_body = DeclareInput,
    responses(
        (status = 201, body = WithdrawalReceipt),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "not_withdrawable", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn my_declare(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<WithdrawalReceipt>), Error> {
    let id = path_id(path)?;
    let input: DeclareInput = parse_json(&body)?;
    let token =
        super::customer::header_str(&headers, super::customer::SESSION_HEADER).map(str::to_owned);
    let receipt = with_ctx(&s, &shopper, async |tx, ctx| {
        let session = commerce::customers::require(tx, token.as_deref()).await?;
        own_order(tx, id, session.customer_id).await?;
        withdrawals::declare(tx, ctx, id, &input, "account").await
    })
    .await?;
    Ok((StatusCode::CREATED, Json(receipt)))
}

/// An invoice or credit note with a 5-minute download link.
#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentLink {
    pub id: Uuid,
    pub kind: DocumentKind,
    pub number: String,
    pub issued_on: NaiveDate,
    pub total: MoneyView,
    pub url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentLinks {
    pub items: Vec<DocumentLink>,
}

async fn links(
    s: &AppState,
    docs: Vec<(invoicing::InvoiceSummary, String)>,
) -> Result<DocumentLinks, Error> {
    let mut items = Vec::with_capacity(docs.len());
    for (d, key) in docs {
        items.push(DocumentLink {
            url: commerce::documents::download_url(&s.storage, &key, &format!("{}.pdf", d.number))
                .await?,
            id: d.id,
            kind: d.kind,
            number: d.number,
            issued_on: d.issued_on,
            total: d.total,
        });
    }
    Ok(DocumentLinks { items })
}

#[utoipa::path(
    get,
    path = "/storefront/v1/orders/{token}/documents",
    tag = "storefront",
    params(StorefrontHeaders, ("token" = String, Path, description = "Order capability token")),
    responses(
        (status = 200, body = DocumentLinks),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn order_documents(
    shopper: Shopper,
    State(s): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Error> {
    let t = token(path)?;
    let docs = with_ctx(&s, &shopper, async |tx, _| {
        let order = orders::by_token(tx, &t).await?;
        invoicing::rendered_for_order(tx, order).await
    })
    .await?;
    Ok(no_store(Json(links(&s, docs).await?).into_response()))
}

#[utoipa::path(
    get,
    path = "/storefront/v1/customer/orders/{id}/documents",
    tag = "storefront",
    params(StorefrontHeaders, SessionHeader, IdParam),
    responses(
        (status = 200, body = DocumentLinks),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn my_documents(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Response, Error> {
    let id = path_id(path)?;
    let docs = as_customer(&s, &shopper, &headers, async |tx, c| {
        own_order(tx, id, c).await?;
        invoicing::rendered_for_order(tx, id).await
    })
    .await?;
    Ok(no_store(Json(links(&s, docs).await?).into_response()))
}
