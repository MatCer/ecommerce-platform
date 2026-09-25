//! Ad-tracking Admin API (spec §11.3, WP20): per-platform settings (credentials write-only),
//! connection tests, pause/resume and the delivery log. Owner/admin only; changing where data
//! goes or what it is sent with (settings, credentials, markets, enabling) also needs a login
//! under 15 minutes old (A9), like webhooks.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use commerce::adtracking::{
    self, AdTracking, ConnectionTest, DeliveryPage, DeliveryQuery, Platform, PlatformConfig,
    PlatformList, PlatformUpdate,
};
use commerce::tenancy::Role;
use platform::Error;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::{TenantHeader, in_tx, parse_json, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_ad_platforms))
        .routes(routes!(update_ad_platform))
        .routes(routes!(test_ad_platform))
        .routes(routes!(list_ad_deliveries))
}

fn configured(s: &AppState) -> Result<&AdTracking, Error> {
    s.ads
        .as_ref()
        .ok_or_else(|| Error::Unavailable("SECRETS_KEY is not configured".into()))
}

/// `{platform}` path parameter.
#[derive(IntoParams)]
#[into_params(parameter_in = Path)]
#[allow(dead_code)]
struct PlatformParam {
    /// `meta`, `ga4`, `google_ads` or `sklik`.
    platform: String,
}

fn platform_of(path: Result<Path<String>, PathRejection>) -> Result<Platform, Error> {
    path.ok()
        .and_then(|Path(p)| Platform::parse(&p))
        .ok_or(Error::NotFound)
}

/// Every platform's configuration (never the credentials).
#[utoipa::path(
    get,
    path = "/admin/v1/ad-platforms",
    tag = "ad-tracking",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses(
        (status = 200, body = PlatformList),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_ad_platforms(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<PlatformList>, Error> {
    staff.require(Role::Admin)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| adtracking::list(tx).await).await?,
    ))
}

/// Changes a platform's configuration (absent fields are kept; credentials are merged).
/// Enabling needs complete settings and credentials; resuming sends what was held.
#[utoipa::path(
    patch,
    path = "/admin/v1/ad-platforms/{platform}",
    tag = "ad-tracking",
    security(("staff_jwt" = [])),
    params(TenantHeader, PlatformParam),
    request_body = PlatformUpdate,
    responses(
        (status = 200, body = PlatformConfig),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_ad_platform(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<PlatformConfig>, Error> {
    staff.require(Role::Admin)?;
    let platform = platform_of(path)?;
    let input: PlatformUpdate = parse_json(&body)?;
    if input.is_sensitive() {
        staff.require_fresh_auth()?;
    }
    let ads = configured(&s)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        adtracking::update(tx, ads, &staff.user.user_id, platform, &input).await
    })
    .await?;
    Ok(Json(out))
}

/// Checks the stored settings and credentials against the vendor without recording an event
/// (Meta: reads the pixel; GA4: validation server; Google Ads: OAuth + validate-only upload;
/// Seznam: no test endpoint, the settings only).
#[utoipa::path(
    post,
    path = "/admin/v1/ad-platforms/{platform}/test",
    tag = "ad-tracking",
    security(("staff_jwt" = [])),
    params(TenantHeader, PlatformParam),
    responses(
        (status = 200, body = ConnectionTest),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn test_ad_platform(
    staff: TenantStaff,
    State(s): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<ConnectionTest>, Error> {
    staff.require(Role::Admin)?;
    let platform = platform_of(path)?;
    let ads = configured(&s)?;
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        adtracking::test_connection(tx, ads, platform).await
    })
    .await?;
    Ok(Json(out))
}

/// The delivery log, newest first: status, attempts, response code, error. No payloads.
#[utoipa::path(
    get,
    path = "/admin/v1/ad-platforms/deliveries",
    tag = "ad-tracking",
    security(("staff_jwt" = [])),
    params(TenantHeader, DeliveryQuery),
    responses(
        (status = 200, body = DeliveryPage),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_ad_deliveries(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<DeliveryQuery>, QueryRejection>,
) -> Result<Json<DeliveryPage>, Error> {
    staff.require(Role::Admin)?;
    let q = query_params(query)?;
    let page = in_tx(&s, staff.tenant_id, async |tx| {
        adtracking::deliveries(tx, &q).await
    })
    .await?;
    Ok(Json(page))
}
