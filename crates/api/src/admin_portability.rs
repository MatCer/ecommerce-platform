//! Data portability Admin API (spec §10.8, §14, A9, A21, A28, A29): CSV imports of customers,
//! historical orders and subscribers, the archived-order list, full tenant exports and GDPR
//! access/erasure requests. Owner/Admin only (staff may read the order archive, like orders);
//! downloading an export and the privacy requests also need a sign-in within the last 15
//! minutes (A9).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use commerce::portability::archived::{self, ArchivedOrderFilter, ArchivedOrderPage};
use commerce::portability::export::{self, DataExport, DataExportList, ExportDownload};
use commerce::portability::imports::{
    self, AnalyzeInput, CreatedDataImport, DataImport, DataImportList, NewDataImport,
};
use commerce::privacy::{self, AccessRequest, ErasureReport, ErasureRequest};
use commerce::tenancy::Role;
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
    query_params,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_data_imports, create_data_import))
        .routes(routes!(get_data_import))
        .routes(routes!(analyze_data_import))
        .routes(routes!(apply_data_import))
        .routes(routes!(list_archived_orders))
        .routes(routes!(list_data_exports, create_data_export))
        .routes(routes!(get_data_export))
        .routes(routes!(download_data_export))
        .routes(routes!(privacy_access))
        .routes(routes!(privacy_erasure))
}

/// The 50 most recent CSV imports.
#[utoipa::path(
    get,
    path = "/admin/v1/data-imports",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = DataImportList))
)]
async fn list_data_imports(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<DataImportList>, Error> {
    staff.require(Role::Admin)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| imports::list(tx).await).await?,
    ))
}

/// Starts a CSV import of customers, historical orders (archived, A28) or newsletter
/// subscribers: PUT the file to `upload`, then call `analyze`. Nothing is written until
/// `apply`. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/data-imports",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewDataImport,
    responses(
        (status = 201, body = CreatedDataImport),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_data_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewDataImport = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    let storage = s.storage.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/data-imports",
        &input,
        async |tx| imports::create(tx, &storage, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/data-imports/{id}",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = DataImport),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_data_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<DataImport>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| imports::get(tx, id).await).await?,
    ))
}

/// Runs (or re-runs, optionally with a new column mapping) the dry run: row errors, counts
/// and a preview. Writes nothing.
#[utoipa::path(
    post,
    path = "/admin/v1/data-imports/{id}/analyze",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = AnalyzeInput,
    responses(
        (status = 202, body = DataImport),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn analyze_data_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<DataImport>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let input: AnalyzeInput = if body.is_empty() {
        AnalyzeInput::default()
    } else {
        parse_json(&body)?
    };
    let actor = &staff.user.user_id;
    let run = in_tx(&s, staff.tenant_id, async |tx| {
        imports::analyze(tx, actor, id, &input).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Imports the valid rows of an analyzed file in the background. Rows with errors are
/// skipped; no email, stock movement, payment, invoice or event is ever triggered.
#[utoipa::path(
    post,
    path = "/admin/v1/data-imports/{id}/apply",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 202, body = DataImport),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn apply_data_import(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<(StatusCode, Json<DataImport>), Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    let run = in_tx(&s, staff.tenant_id, async |tx| {
        imports::apply(tx, actor, id).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

/// Imported historical orders (read-only archive), newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/archived-orders",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, ArchivedOrderFilter),
    responses((status = 200, body = ArchivedOrderPage))
)]
async fn list_archived_orders(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<ArchivedOrderFilter>, QueryRejection>,
) -> Result<Json<ArchivedOrderPage>, Error> {
    let f = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| archived::list(tx, &f).await).await?,
    ))
}

/// Recent full data exports.
#[utoipa::path(
    get,
    path = "/admin/v1/data-exports",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = DataExportList))
)]
async fn list_data_exports(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<DataExportList>, Error> {
    staff.require(Role::Admin)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| export::list(tx).await).await?,
    ))
}

/// Starts a full export of the shop's data (JSON Lines per table + assets manifest, zipped),
/// prepared in the background. One at a time; needs a recent sign-in (A9).
#[utoipa::path(
    post,
    path = "/admin/v1/data-exports",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 202, body = DataExport),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "export_busy", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_data_export(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<(StatusCode, Json<DataExport>), Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let actor = &staff.user.user_id;
    let e = in_tx(&s, staff.tenant_id, async |tx| {
        export::create(tx, actor).await
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(e)))
}

#[utoipa::path(
    get,
    path = "/admin/v1/data-exports/{id}",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = DataExport),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_data_export(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<DataExport>, Error> {
    staff.require(Role::Admin)?;
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| export::get(tx, id).await).await?,
    ))
}

/// A 5-minute download link of a ready export (A21). Needs a recent sign-in (A9); audited.
#[utoipa::path(
    post,
    path = "/admin/v1/data-exports/{id}/download",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = ExportDownload),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn download_data_export(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ExportDownload>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    let storage = s.storage.clone();
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            export::download(tx, &storage, actor, id).await
        })
        .await?,
    ))
}

/// GDPR access request (art. 15): everything the shop holds about an email address and its
/// customer account, as a JSON file. Needs a recent sign-in (A9); audited.
#[utoipa::path(
    post,
    path = "/admin/v1/privacy/access",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = AccessRequest,
    responses(
        (status = 200, content_type = "application/json", body = Object),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn privacy_access(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let input: AccessRequest = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let doc = in_tx(&s, staff.tenant_id, async |tx| {
        privacy::access(tx, actor, &input.email).await
    })
    .await?;
    let mut res = Json(doc).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"personal-data.json\""),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// GDPR erasure (art. 17): deletes the customer account and personal data, anonymizes orders
/// (invoices stay as issued, tax law). Refused with `409 erasure_blocked` while an order is in
/// progress. Irreversible; `confirm_email` must repeat the address. Needs a recent sign-in.
#[utoipa::path(
    post,
    path = "/admin/v1/privacy/erasure",
    tag = "portability",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = ErasureRequest,
    responses(
        (status = 200, body = ErasureReport),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "erasure_blocked", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn privacy_erasure(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<ErasureReport>, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let input: ErasureRequest = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let report = in_tx(&s, staff.tenant_id, async |tx| {
        privacy::erase(tx, actor, &input).await
    })
    .await?;
    Ok(Json(report))
}
