//! Order management in the Admin API (spec §8.3, §10.5-10.7, WP12): status actions, labels
//! and shipments, cancellation, refunds with credit notes, notes, address edits before a label,
//! invoices and labels as short-lived download links (A21), packing slips and label sheets,
//! carrier accounts, and the withdrawals queue (A19).
//!
//! Roles: fulfillment actions (labels, dispatch, delivery, notes) are open to all staff; money
//! (cancel, refunds, withdrawals refunds) needs an admin; carrier credentials an admin with a
//! fresh login (A9).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use commerce::carriers::{self, CarrierAccount, CarrierAccountInput, CarrierKind};
use commerce::checkout::CheckoutAddress;
use commerce::documents::{self, DocumentInput, GeneratedDocument};
use commerce::fulfillment::{self, LabelInput, ShipmentView};
use commerce::invoicing;
use commerce::refunds::{self, CancelInput, CancelOutcome, RefundInput, RefundOutcome, RefundPlan};
use commerce::tenancy::Role;
use commerce::withdrawals::{self, Withdrawal};
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{IdParam, TenantHeader, in_tx, parse_json, path_id, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_carriers))
        .routes(routes!(put_carrier, delete_carrier))
        .routes(routes!(start_processing))
        .routes(routes!(create_label, cancel_label))
        .routes(routes!(label_url))
        .routes(routes!(ship))
        .routes(routes!(deliver))
        .routes(routes!(returned_to_sender))
        .routes(routes!(cancel_order))
        .routes(routes!(add_note))
        .routes(routes!(update_address))
        .routes(routes!(preview_refund))
        .routes(routes!(create_refund))
        .routes(routes!(retry_refund))
        .routes(routes!(refund_exception))
        .routes(routes!(invoice_url))
        .routes(routes!(create_document))
        .routes(routes!(get_document))
        .routes(routes!(list_withdrawals))
        .routes(routes!(get_withdrawal))
        .routes(routes!(receive_withdrawal))
        .routes(routes!(withdrawal_proof))
        .routes(routes!(refund_withdrawal))
}

fn carriers_of(s: &AppState) -> Result<&carriers::Carriers, Error> {
    s.carriers
        .as_ref()
        .ok_or_else(|| Error::Unavailable("carrier integrations are not configured".into()))
}

// ---------------------------------------------------------------------------------------
// Carrier accounts

#[derive(Serialize, ToSchema)]
pub struct CarrierAccountList {
    pub items: Vec<CarrierAccount>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/carriers",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = CarrierAccountList))
)]
async fn list_carriers(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<CarrierAccountList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| carriers::accounts(tx).await).await?;
    Ok(Json(CarrierAccountList { items }))
}

/// Stores a carrier's API credentials (sealed; never returned). Admin with a fresh login.
/// Packeta: `api_password`; PPL: `client_id` + `client_secret`.
#[utoipa::path(
    put,
    path = "/admin/v1/carriers/{carrier}",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, ("carrier" = CarrierKind, Path)),
    request_body = CarrierAccountInput,
    responses(
        (status = 200, body = CarrierAccount),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_carrier(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<CarrierKind>, PathRejection>,
    body: Bytes,
) -> Result<Json<CarrierAccount>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let Path(carrier) = path.map_err(|_| Error::NotFound)?;
    let input: CarrierAccountInput = parse_json(&body)?;
    let c = carriers_of(&s)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            carriers::configure(tx, c, actor, carrier, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/carriers/{carrier}",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, ("carrier" = CarrierKind, Path)),
    responses(
        (status = 204),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_carrier(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<CarrierKind>, PathRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let Path(carrier) = path.map_err(|_| Error::NotFound)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        carriers::remove(tx, actor, carrier).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Status actions

/// `confirmed → processing` (packing started).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/processing",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn start_processing(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::start_processing(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates the shipment and its label at the carrier (the order becomes `processing`).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/shipment",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = LabelInput,
    responses(
        (status = 201, body = ShipmentView),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "order_not_ready | label_exists | label_in_progress | carrier_not_configured", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "carrier_rejected", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "the carrier is unavailable", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_label(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<ShipmentView>), Error> {
    let id = path_id(id)?;
    let input: LabelInput = if body.is_empty() {
        LabelInput::default()
    } else {
        parse_json(&body)?
    };
    let view = fulfillment::create_label(
        &s.db,
        carriers_of(&s)?,
        &s.storage,
        staff.tenant_id,
        &staff.user.user_id,
        id,
        &input,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Voids the shipment before dispatch.
#[utoipa::path(
    delete,
    path = "/admin/v1/orders/{id}/shipment",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 409, description = "no_shipment | already_shipped", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cancel_label(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::cancel_label(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A 5-minute download link (A21).
#[derive(Serialize, ToSchema)]
pub struct DownloadLink {
    pub url: String,
}

/// The live shipment's label PDF.
#[utoipa::path(
    get,
    path = "/admin/v1/orders/{id}/shipment/label",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = DownloadLink),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn label_url(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<DownloadLink>, Error> {
    let id = path_id(id)?;
    let key = in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::label_of(tx, id).await?.ok_or(Error::NotFound)
    })
    .await?;
    Ok(Json(DownloadLink {
        url: documents::download_url(&s.storage, &key, "label.pdf").await?,
    }))
}

/// The carrier took the parcel: stock commit, `order.shipped`, shipped email.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/ship",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 409, description = "no_shipment | invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn ship(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::ship(tx, &s.public_urls, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The parcel was delivered (manual; tracking does it automatically).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/deliver",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 409, description = "no_shipment | invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn deliver(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::deliver(tx, &s.public_urls, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The undelivered parcel is back (merchant-confirmed): restock, order `returned`.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/returned-to-sender",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204),
        (status = 409, description = "no_shipment | invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn returned_to_sender(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::returned_to_sender(tx, actor, id, chrono::Utc::now()).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Cancels an order that has not left: stock and coupon released, the customer told; a paid
/// order is refunded in full with a credit note. Admin.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/cancel",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CancelInput,
    responses(
        (status = 200, body = CancelOutcome),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition | already_shipped | invoice_pending | refund_rejected", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cancel_order(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<CancelOutcome>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: CancelInput = if body.is_empty() {
        CancelInput::default()
    } else {
        parse_json(&body)?
    };
    Ok(Json(
        refunds::cancel(
            &s.db,
            &s.checkout.payments,
            &s.public_urls,
            staff.tenant_id,
            &staff.user.user_id,
            id,
            &input,
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrderNoteInput {
    /// 1-2000 characters, shown on the timeline.
    pub note: String,
}

#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/notes",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = OrderNoteInput,
    responses(
        (status = 204),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn add_note(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let input: OrderNoteInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::add_note(tx, actor, id, &input.note).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Changes the shipping address before a label exists (same country: M1 has no financial
/// edits, A13).
#[utoipa::path(
    put,
    path = "/admin/v1/orders/{id}/shipping-address",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CheckoutAddress,
    responses(
        (status = 204),
        (status = 409, description = "address_locked", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_address(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let input: CheckoutAddress = parse_json(&body)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        fulfillment::update_shipping_address(tx, actor, id, &input).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Refunds

/// What a refund would return (A15), without refunding.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/refunds/preview",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = RefundInput,
    responses(
        (status = 200, body = RefundPlan),
        (status = 422, description = "invalid_refund", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preview_refund(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<RefundPlan>, Error> {
    let id = path_id(id)?;
    let input: RefundInput = parse_json(&body)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            refunds::plan(tx, id, &input).await
        })
        .await?,
    ))
}

/// Refunds lines/quantities and charges of a paid order through its payment method, with a
/// credit note and the refund email. Admin.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/refunds",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = RefundInput,
    responses(
        (status = 201, body = RefundOutcome),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "nothing_to_refund | invoice_pending | refund_rejected", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_refund | invalid_iban", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_refund(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<RefundOutcome>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: RefundInput = parse_json(&body)?;
    let out = refunds::refund_order(
        &s.db,
        &s.checkout.payments,
        &s.public_urls,
        staff.tenant_id,
        &staff.user.user_id,
        id,
        &input,
        None,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// Repeats a pending Stripe refund with the same idempotency key.
#[utoipa::path(
    post,
    path = "/admin/v1/refunds/{id}/retry",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = commerce::payments::Refund),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn retry_refund(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<commerce::payments::Refund>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        commerce::payments::retry_refund(&s.db, &s.checkout.payments, staff.tenant_id, id).await?,
    ))
}

/// Returns a late or duplicate payment (A10) and resolves the order's exception. Admin.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/exception/refund",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Vec<commerce::payments::Refund>),
        (status = 409, description = "no_open_exception", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn refund_exception(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Vec<commerce::payments::Refund>>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        refunds::refund_exception(
            &s.db,
            &s.checkout.payments,
            staff.tenant_id,
            &staff.user.user_id,
            id,
        )
        .await?,
    ))
}

/// An invoice or credit note PDF (404 until rendered).
#[utoipa::path(
    get,
    path = "/admin/v1/invoices/{id}/pdf",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = DownloadLink),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn invoice_url(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<DownloadLink>, Error> {
    let id = path_id(id)?;
    let (key, number) = in_tx(&s, staff.tenant_id, async |tx| {
        let (_, doc, key) = invoicing::get(tx, id).await?;
        Ok((key.ok_or(Error::NotFound)?, doc.number))
    })
    .await?;
    Ok(Json(DownloadLink {
        url: documents::download_url(&s.storage, &key, &format!("{number}.pdf")).await?,
    }))
}

// ---------------------------------------------------------------------------------------
// Packing slips and label sheets

#[derive(Serialize, ToSchema)]
pub struct DocumentRequested {
    pub document: GeneratedDocument,
    /// Orders whose label could not be created (label sheets): `{order_id, error}`.
    pub failures: Vec<LabelFailure>,
}

#[derive(Serialize, ToSchema)]
pub struct LabelFailure {
    pub order_id: Uuid,
    pub error: String,
}

/// Queues a packing slip or label sheet PDF for up to 100 orders (poll `GET
/// /admin/v1/documents/{id}`). Label sheets first create the missing labels.
#[utoipa::path(
    post,
    path = "/admin/v1/documents",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = DocumentInput,
    responses(
        (status = 202, body = DocumentRequested),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_document(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<(StatusCode, Json<DocumentRequested>), Error> {
    let input: DocumentInput = parse_json(&body)?;
    if input.order_ids.len() > documents::MAX_ORDERS {
        return Err(Error::Validation {
            code: "invalid_document",
            detail: "1-100 orders".into(),
        });
    }
    let mut failures = Vec::new();
    if input.kind == documents::DocumentKind::Labels {
        let c = carriers_of(&s)?;
        for order in &input.order_ids {
            let needs = in_tx(&s, staff.tenant_id, async |tx| {
                Ok(fulfillment::actions(tx, *order).await?.create_label)
            })
            .await?;
            if !needs {
                continue;
            }
            if let Err(e) = fulfillment::create_label(
                &s.db,
                c,
                &s.storage,
                staff.tenant_id,
                &staff.user.user_id,
                *order,
                &LabelInput::default(),
            )
            .await
            {
                failures.push(LabelFailure {
                    order_id: *order,
                    error: e.to_string(),
                });
            }
        }
    }
    let actor = &staff.user.user_id;
    let document = in_tx(&s, staff.tenant_id, async |tx| {
        documents::request(tx, actor, &input).await
    })
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(DocumentRequested { document, failures }),
    ))
}

#[utoipa::path(
    get,
    path = "/admin/v1/documents/{id}",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = GeneratedDocument),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_document(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<GeneratedDocument>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            documents::get(tx, Some(&s.storage), id).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Withdrawals (A19)

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WithdrawalQuery {
    /// Only withdrawals still to refund (default true).
    pub open: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct WithdrawalList {
    pub items: Vec<Withdrawal>,
}

/// Withdrawals, soonest refund deadline first.
#[utoipa::path(
    get,
    path = "/admin/v1/withdrawals",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, WithdrawalQuery),
    responses((status = 200, body = WithdrawalList))
)]
async fn list_withdrawals(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<WithdrawalQuery>, QueryRejection>,
) -> Result<Json<WithdrawalList>, Error> {
    let q = query_params(query)?;
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        withdrawals::list(tx, q.open.unwrap_or(true)).await
    })
    .await?;
    Ok(Json(WithdrawalList { items }))
}

#[utoipa::path(
    get,
    path = "/admin/v1/withdrawals/{id}",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Withdrawal),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_withdrawal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Withdrawal>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            withdrawals::get(tx, id).await
        })
        .await?,
    ))
}

/// The returned goods arrived: restocked (A13).
#[utoipa::path(
    post,
    path = "/admin/v1/withdrawals/{id}/receive",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Withdrawal),
        (status = 409, description = "already_received | invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn receive_withdrawal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Withdrawal>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            withdrawals::receive(tx, actor, id).await
        })
        .await?,
    ))
}

/// The customer proved they sent the goods back (the refund may go out before they arrive).
#[utoipa::path(
    post,
    path = "/admin/v1/withdrawals/{id}/proof",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses((status = 200, body = Withdrawal))
)]
async fn withdrawal_proof(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Withdrawal>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            withdrawals::record_proof(tx, actor, id).await
        })
        .await?,
    ))
}

/// Refunds the withdrawn goods (+ shipping when everything was withdrawn). Admin.
#[utoipa::path(
    post,
    path = "/admin/v1/withdrawals/{id}/refund",
    tag = "fulfillment",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 201, body = RefundOutcome),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "goods_not_back | already_refunded | nothing_to_refund | invoice_pending", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn refund_withdrawal(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<(StatusCode, Json<RefundOutcome>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let out = withdrawals::refund(
        &s.db,
        &s.checkout.payments,
        &s.public_urls,
        staff.tenant_id,
        &staff.user.user_id,
        id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(out)))
}
