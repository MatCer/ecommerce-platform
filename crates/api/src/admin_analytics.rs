//! Analytics Admin API (spec §11.3): the dashboard.

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use commerce::analytics::{self, Dashboard, DashboardQuery};
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::{TenantHeader, in_tx, query_params};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(analytics_dashboard))
}

/// Sales (from orders: placed, not cancelled), traffic (edge page counters without
/// identifiers; sessions, funnel and conversion over consented sessions only, A20), top
/// products, top and zero-result searches, and Web Vitals p75 per template. Days are UTC.
#[utoipa::path(
    get,
    path = "/admin/v1/analytics/dashboard",
    tag = "analytics",
    security(("staff_jwt" = [])),
    params(TenantHeader, DashboardQuery),
    responses(
        (status = 200, body = Dashboard),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn analytics_dashboard(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<DashboardQuery>, QueryRejection>,
) -> Result<Json<Dashboard>, Error> {
    let q = query_params(query)?;
    let d = in_tx(&s, staff.tenant_id, async |tx| {
        analytics::dashboard(tx, &q).await
    })
    .await?;
    Ok(Json(d))
}
