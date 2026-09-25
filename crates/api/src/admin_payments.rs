//! Payments in the Admin API (spec §8.3, §10.4, WP11): receiving bank accounts, statement
//! upload and the bank transactions, the payment exceptions queue, Stripe Connect onboarding
//! and status, and cash-on-delivery actions.
//!
//! Roles: reading is for any staff member; settings (bank account, Stripe onboarding) need an
//! admin with a fresh login (A9); money actions (imports, resolutions, COD) need an admin.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use commerce::orders::{self, OrderSummary};
use commerce::payments::Attempt;
use commerce::payments::bank::{
    self, BankAccount, BankAccountInput, BankTransaction, BankTransactionPage, ResolveInput,
    StatementImport, TxFilter, TxStatus,
};
use commerce::payments::cod::{self, CodReport, CollectInput};
use commerce::payments::statements::{self, StatementFormat};
use commerce::payments::stripe::{self, StripeAccount};
use commerce::tenancy::Role;
use platform::Error;
use platform::config::StripeMode;
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
        .routes(routes!(get_bank_account, put_bank_account))
        .routes(routes!(upload_statement))
        .routes(routes!(list_bank_transactions))
        .routes(routes!(resolve_bank_transaction))
        .routes(routes!(list_exceptions))
        .routes(routes!(resolve_order_exception))
        .routes(routes!(stripe_status))
        .routes(routes!(stripe_onboarding))
        .routes(routes!(stripe_refresh))
        .routes(routes!(stripe_simulate))
        .routes(routes!(cod_deliver))
        .routes(routes!(cod_collect))
        .routes(routes!(cod_remit))
        .routes(routes!(cod_report))
}

// ---------------------------------------------------------------------------------------
// Bank accounts and statements

/// The market's receiving bank account (`404` when none is set).
#[utoipa::path(
    get,
    path = "/admin/v1/markets/{id}/bank-account",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = BankAccount),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_bank_account(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<BankAccount>, Error> {
    let market = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            bank::account(tx, market).await?.ok_or(Error::NotFound)
        })
        .await?,
    ))
}

/// Sets the market's receiving account for bank transfers (IBAN check digits verified). The
/// optional Fio API token is stored encrypted and never returned. Payment settings: admin
/// role and a login within the last 15 minutes (`401 reauth_required`, A9).
#[utoipa::path(
    put,
    path = "/admin/v1/markets/{id}/bank-account",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = BankAccountInput,
    responses(
        (status = 200, body = BankAccount),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_bank_account", body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "PAYMENTS_SECRET_KEY missing for a Fio token", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_bank_account(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<BankAccount>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let market = path_id(id)?;
    let input: BankAccountInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let secrets = s.checkout.payments.secrets.as_deref();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            bank::configure_account(tx, actor, secrets, market, &input).await
        })
        .await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatementQuery {
    /// `camt053` (ISO 20022 XML), `fio_csv` or `gpc` (ABO).
    #[param(inline)]
    pub format: StatementFormat,
}

/// Imports a bank statement (the raw file as the body, at most 1 MB) into the account and
/// matches the new credits (A25): lines already imported (same bank transaction id) are
/// ignored; exact VS + amount + currency matches pay their order; the rest waits in the
/// exceptions queue. `422 statement_account_mismatch` when the file names another account.
#[utoipa::path(
    post,
    path = "/admin/v1/bank-accounts/{id}/statements",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam, StatementQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 200, body = StatementImport),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_statement | statement_account_mismatch", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn upload_statement(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    query: Result<Query<StatementQuery>, QueryRejection>,
    body: Bytes,
) -> Result<Json<StatementImport>, Error> {
    staff.require(Role::Admin)?;
    let account = path_id(id)?;
    let q = query_params(query)?;
    let statement = statements::parse(q.format, &body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            bank::import(tx, actor, account, q.format.source(), &statement).await
        })
        .await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TxQuery {
    pub status: Option<TxStatus>,
    pub bank_account_id: Option<Uuid>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// Imported bank transactions, newest first, with their match status.
#[utoipa::path(
    get,
    path = "/admin/v1/bank-transactions",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, TxQuery),
    responses((status = 200, body = BankTransactionPage))
)]
async fn list_bank_transactions(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<TxQuery>, QueryRejection>,
) -> Result<Json<BankTransactionPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            let filter = TxFilter {
                status: q.status,
                open: false,
                bank_account_id: q.bank_account_id,
            };
            bank::transactions(tx, &filter, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// Resolves an open bank transaction (audited): `accept` a partial/over payment as the
/// order's payment, `assign` it to an order by number, or `dismiss` it with a note.
#[utoipa::path(
    post,
    path = "/admin/v1/bank-transactions/{id}/resolve",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ResolveInput,
    responses(
        (status = 200, body = BankTransaction),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "transaction_resolved", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_resolution", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn resolve_bank_transaction(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<BankTransaction>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: ResolveInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            bank::resolve(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// The payment exceptions queue: bank transactions waiting for a person (unmatched, partial,
/// over) and orders holding money they cannot keep (late or duplicate payments, A10).
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentExceptions {
    pub bank_transactions: Vec<BankTransaction>,
    pub orders: Vec<OrderSummary>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/payment-exceptions",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = PaymentExceptions))
)]
async fn list_exceptions(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<PaymentExceptions>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            let bank_transactions = bank::transactions(
                tx,
                &TxFilter {
                    open: true,
                    ..TxFilter::default()
                },
                None,
                100,
            )
            .await?
            .items;
            let orders = orders::list(
                tx,
                &orders::OrderFilter {
                    exception: true,
                    ..orders::OrderFilter::default()
                },
                None,
                100,
            )
            .await?
            .items;
            Ok(PaymentExceptions {
                bank_transactions,
                orders,
            })
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteInput {
    /// What was done (e.g. "refunded to the customer's account"), 1-500 characters.
    pub note: String,
}

/// Marks an order's exception (late or duplicate payment) as settled (audited); it leaves the
/// queue. `409 no_open_exception` otherwise.
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/exception/resolve",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = NoteInput,
    responses(
        (status = 204),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "no_open_exception", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn resolve_order_exception(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: NoteInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        orders::resolve_exception(tx, actor, id, &input.note).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Stripe Connect

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StripePlatformMode {
    Live,
    Test,
    /// Local: stripe-mock and signed simulated events.
    Simulator,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StripeStatus {
    /// `null`: the platform has no Stripe configuration (Stripe cannot be offered).
    pub mode: Option<StripePlatformMode>,
    /// The shop's connected account, once onboarding started.
    pub account: Option<StripeAccount>,
}

fn mode(s: &AppState) -> Option<StripePlatformMode> {
    s.checkout.payments.stripe.as_ref().map(|s| match s.mode() {
        StripeMode::Live => StripePlatformMode::Live,
        StripeMode::Test => StripePlatformMode::Test,
        StripeMode::Simulator => StripePlatformMode::Simulator,
    })
}

fn stripe_of(s: &AppState) -> Result<&stripe::Stripe, Error> {
    s.checkout
        .payments
        .stripe
        .as_ref()
        .ok_or_else(|| Error::Unavailable("Stripe is not configured on this platform".into()))
}

/// The platform's Stripe mode and the shop's connected account (capabilities, A11).
#[utoipa::path(
    get,
    path = "/admin/v1/payments/stripe",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = StripeStatus))
)]
async fn stripe_status(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<StripeStatus>, Error> {
    let account = in_tx(&s, staff.tenant_id, async |tx| stripe::account(tx).await).await?;
    Ok(Json(StripeStatus {
        mode: mode(&s),
        account,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OnboardingLink {
    /// Stripe-hosted onboarding (single use, expires in minutes); in simulator mode the admin
    /// page itself (the account becomes ready through a simulated `account.updated`).
    pub url: String,
}

/// Starts or continues Stripe-hosted onboarding of the shop's connected account (created on
/// first use). Payment settings: admin role and a fresh login (A9).
#[utoipa::path(
    post,
    path = "/admin/v1/payments/stripe/onboarding",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = OnboardingLink),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn stripe_onboarding(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<OnboardingLink>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let stripe = stripe_of(&s)?;
    let admin = s.admin_origin.to_str().unwrap_or_default();
    let page = format!("{admin}/settings/payments");
    let url = stripe::start_onboarding(
        &s.db,
        stripe,
        staff.tenant_id,
        &staff.user.user_id,
        &format!("{page}?stripe=return"),
        &format!("{page}?stripe=refresh"),
    )
    .await?;
    Ok(Json(OnboardingLink { url }))
}

/// Re-reads the connected account from Stripe (after returning from onboarding).
#[utoipa::path(
    post,
    path = "/admin/v1/payments/stripe/refresh",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = StripeStatus),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn stripe_refresh(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<StripeStatus>, Error> {
    staff.require(Role::Admin)?;
    let account = stripe::refresh_account(&s.db, stripe_of(&s)?, staff.tenant_id).await?;
    Ok(Json(StripeStatus {
        mode: mode(&s),
        account,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SimulateAccountInput {
    /// `true`: onboarding completed (charges enabled, card payments active); `false`: the
    /// card_payments capability is lost (Stripe disappears from checkout, A11).
    pub enabled: bool,
}

/// Simulator only (`404` otherwise): emits a signed `account.updated` for the shop's account.
#[utoipa::path(
    post,
    path = "/admin/v1/payments/stripe/simulate",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = SimulateAccountInput,
    responses(
        (status = 202),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn stripe_simulate(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<StatusCode, Error> {
    staff.require(Role::Admin)?;
    let input: SimulateAccountInput = parse_json(&body)?;
    let stripe = stripe_of(&s).map_err(|_| Error::NotFound)?;
    if !stripe.simulator() {
        return Err(Error::NotFound);
    }
    let account = in_tx(&s, staff.tenant_id, async |tx| stripe::account(tx).await)
        .await?
        .ok_or(Error::NotFound)?;
    stripe::simulate_account(&s.db, stripe, &account.account_id, input.enabled).await?;
    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------------------
// Cash on delivery (A16)

/// The parcel was delivered (COD `pending → delivered`, audited).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/cod/deliver",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Attempt),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition | not_cash_on_delivery | order_cancelled", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cod_deliver(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Attempt>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            cod::deliver(tx, actor, id).await
        })
        .await?,
    ))
}

/// The money was collected: tender (`cash` is rounded as a separate charge, A16) and
/// collector are recorded, the order becomes paid (audited).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/cod/collect",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CollectInput,
    responses(
        (status = 200, body = Attempt),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition | not_cash_on_delivery | order_cancelled", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_collection", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cod_collect(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Attempt>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: CollectInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            cod::collect(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemitInput {
    /// E.g. the carrier's payout reference, at most 500 characters.
    #[serde(default)]
    pub note: Option<String>,
}

/// The carrier paid the collected money out (COD `collected → remitted`, audited).
#[utoipa::path(
    post,
    path = "/admin/v1/orders/{id}/cod/remit",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = RemitInput,
    responses(
        (status = 200, body = Attempt),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "invalid_transition", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cod_remit(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Attempt>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: RemitInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            cod::remit(tx, actor, id, input.note.as_deref()).await
        })
        .await?,
    ))
}

/// Imports a carrier COD report (CSV body `order_number;amount;tender;event`, `event` =
/// `collected` or `remitted`). Each row applies on its own; mismatched amounts are reported,
/// never applied.
#[utoipa::path(
    post,
    path = "/admin/v1/cod-reports",
    tag = "payments",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body(content = Vec<u8>, content_type = "text/csv"),
    responses(
        (status = 200, body = CodReport),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "invalid_cod_report", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cod_report(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<CodReport>, Error> {
    staff.require(Role::Admin)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            cod::import_report(tx, actor, &body).await
        })
        .await?,
    ))
}
