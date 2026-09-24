//! Admin API (`/admin/v1`, spec §8.3). Staff JWT + `X-Tenant-Id` membership on every call;
//! every mutation writes the audit log in its transaction.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use commerce::audit::{self, AuditPage};
use commerce::idempotency::{self, Stored};
use commerce::markets::{self, Market, NewMarket};
use commerce::tenancy::{self, Membership, Role};
use platform::Error;
use platform::db::tenant_tx;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{StaffUser, TenantStaff};

const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");
pub const REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(me))
        .routes(routes!(list_markets, create_market))
        .routes(routes!(audit_log))
}

#[derive(Serialize, ToSchema)]
pub struct Me {
    pub user_id: String,
    pub email: String,
    /// Tenants the user can act in (send one as `X-Tenant-Id`).
    pub memberships: Vec<Membership>,
}

/// The signed-in staff user and their tenants. Needs no `X-Tenant-Id`.
#[utoipa::path(
    get,
    path = "/admin/v1/me",
    tag = "admin",
    security(("staff_jwt" = [])),
    responses(
        (status = 200, body = Me),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn me(user: StaffUser, State(s): State<AppState>) -> Result<Json<Me>, Error> {
    let memberships = tenancy::memberships(&s.db, &user.user_id).await?;
    Ok(Json(Me {
        user_id: user.user_id,
        email: user.email,
        memberships,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct MarketList {
    pub items: Vec<Market>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/markets",
    tag = "admin",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = MarketList),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_markets(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<MarketList>, Error> {
    let mut tx = tenant_tx(&s.db, staff.tenant_id).await?;
    let items = markets::list(&mut tx).await?;
    tx.commit().await?;
    Ok(Json(MarketList { items }))
}

const CREATE_MARKET: &str = "POST /admin/v1/markets";

/// Creates a market (owner/admin). Honors `Idempotency-Key`: a retry with the same key and
/// body returns the original response with `Idempotent-Replayed: true`; the same key with a
/// different body is `409 idempotency_conflict`.
#[utoipa::path(
    post,
    path = "/admin/v1/markets",
    tag = "admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewMarket,
    responses(
        (status = 201, body = Market),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_market(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    let input: NewMarket = parse_json(&body)?;
    let key = idempotency_key(&headers)?;

    let mut tx = tenant_tx(&s.db, staff.tenant_id).await?;
    if let Some(key) = &key {
        let hash = idempotency::request_hash(&serde_json::to_vec(&input).map_err(internal)?);
        if let Some(stored) = idempotency::begin(&mut tx, CREATE_MARKET, key, &hash).await? {
            return Ok(replay(stored));
        }
    }
    let market = markets::create(&mut tx, &staff.user.user_id, &input).await?;
    if let Some(key) = &key {
        let body = serde_json::to_value(&market).map_err(internal)?;
        idempotency::finish(
            &mut tx,
            CREATE_MARKET,
            key,
            StatusCode::CREATED.as_u16(),
            &body,
        )
        .await?;
    }
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(market)).into_response())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

/// The tenant's audit log, newest first (owner/admin).
#[utoipa::path(
    get,
    path = "/admin/v1/audit-log",
    tag = "admin",
    security(("staff_jwt" = [])),
    params(TenantHeader, AuditQuery),
    responses(
        (status = 200, body = AuditPage),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn audit_log(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<AuditQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<AuditPage>, Error> {
    staff.require(Role::Admin)?;
    let Query(q) = query.map_err(|e| Error::BadRequest {
        code: "invalid_query",
        detail: e.body_text(),
    })?;
    let mut tx = tenant_tx(&s.db, staff.tenant_id).await?;
    let page = audit::list(&mut tx, q.cursor, q.limit.unwrap_or(50)).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// `X-Tenant-Id` (documentation only; `TenantStaff` reads it).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
struct TenantHeader {
    /// The tenant to act in; the caller must be a member.
    #[param(rename = "X-Tenant-Id")]
    x_tenant_id: Uuid,
}

/// `Idempotency-Key` (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
struct IdempotencyHeader {
    /// 1-255 visible ASCII characters; kept for 24 hours.
    #[param(rename = "Idempotency-Key")]
    idempotency_key: Option<String>,
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body).map_err(|e| Error::Validation {
        code: "invalid_body",
        detail: e.to_string(),
    })
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, Error> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(None);
    };
    let key = raw
        .to_str()
        .map_err(|_| Error::BadRequest {
            code: "invalid_idempotency_key",
            detail: "Idempotency-Key must be visible ASCII".into(),
        })?
        .to_owned();
    idempotency::validate_key(&key)?;
    Ok(Some(key))
}

fn replay(stored: Stored) -> Response {
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    let mut res = (status, Json(stored.body)).into_response();
    res.headers_mut()
        .insert(REPLAYED, HeaderValue::from_static("true"));
    res
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}
