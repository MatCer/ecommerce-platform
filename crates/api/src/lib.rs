//! HTTP API (spec §8). Routers for storefront/admin/internal arrive in later WPs; WP0 ships the
//! shell: health, readiness, OpenAPI, Swagger UI (dev only) and the shared middleware stack.

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderName, StatusCode};
use axum::routing::get;
use platform::Error;
use platform::health::{CheckStatus, Readiness};
use platform::storage::Storage;
use reqwest::Url;
use serde::Serialize;
use sqlx::PgPool;
use tower::ServiceBuilder;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

/// Spec §8.1: request bodies are capped at 1 MB; large uploads go through presigned URLs.
pub const BODY_LIMIT_BYTES: usize = 1024 * 1024;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub http: reqwest::Client,
    pub meili_url: Url,
    pub storage: Storage,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Commerce Platform API", version = "0.1.0"),
    components(schemas(platform::Problem)),
    tags((name = "health", description = "Liveness and readiness"))
)]
struct ApiDoc;

fn documented_routes() -> (Router<AppState>, OpenApiSpec) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(healthz))
        .routes(routes!(readyz))
        .split_for_parts()
}

/// The OpenAPI document, as served at `/openapi.json`.
pub fn openapi() -> OpenApiSpec {
    documented_routes().1
}

/// The full application. `docs` mounts Swagger UI at `/docs` (dev only).
pub fn app(state: AppState, docs: bool) -> Router {
    let (router, spec) = documented_routes();
    let router = if docs {
        router.merge(SwaggerUi::new("/docs").url("/openapi.json", spec))
    } else {
        router.route(
            "/openapi.json",
            get(move || {
                let spec = spec.clone();
                async move { Json(spec) }
            }),
        )
    };

    router
        .fallback(|| async { Error::NotFound })
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|req: &Request| {
                            let request_id = req
                                .headers()
                                .get(REQUEST_ID)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default();
                            // Path only: query strings may carry tokens.
                            tracing::info_span!(
                                "request",
                                method = %req.method(),
                                path = %req.uri().path(),
                                request_id,
                            )
                        })
                        .on_response(DefaultOnResponse::new().level(Level::INFO)),
                )
                .layer(PropagateRequestIdLayer::new(REQUEST_ID))
                .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES)),
        )
        .with_state(state)
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: CheckStatus,
}

/// Liveness: the process serves HTTP. Never touches dependencies.
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "health",
    responses((status = 200, description = "Process is alive", body = Health))
)]
async fn healthz() -> Json<Health> {
    Json(Health {
        status: CheckStatus::Ok,
    })
}

/// Readiness: database, Meilisearch and object storage are reachable.
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "health",
    responses(
        (status = 200, description = "All dependencies reachable", body = Readiness),
        (status = 503, description = "At least one dependency is unreachable", body = Readiness)
    )
)]
async fn readyz(State(s): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let report = platform::health::readiness(&s.db, &s.http, &s.meili_url, &s.storage).await;
    let status = if report.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}
