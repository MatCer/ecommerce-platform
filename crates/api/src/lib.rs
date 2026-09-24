//! HTTP API (spec §8). Routers for storefront/admin/internal arrive in later WPs; WP0 ships the
//! shell: health, readiness, OpenAPI, Swagger UI (dev only) and the shared middleware stack.

use axum::Router;
use axum::body::{Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::{HeaderName, StatusCode, header};
use axum::middleware::{map_request, map_response};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{BoxError, Json};
use platform::Error;
use platform::error::PROBLEM_CONTENT_TYPE;
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
        .method_not_allowed_fallback(|| async { Error::MethodNotAllowed })
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
                .layer(map_response(problem_for_body_limit))
                .layer(RequestBodyLimitLayer::new(BODY_LIMIT_BYTES)),
        )
        // Outermost, so invalid ids are gone before `SetRequestIdLayer` looks at them.
        .layer(map_request(drop_invalid_request_id))
        .with_state(state)
}

/// Client-supplied request ids are echoed and logged, so only short, plain ids are kept;
/// anything else is dropped and `SetRequestIdLayer` generates a fresh UUID.
async fn drop_invalid_request_id(mut req: Request) -> Request {
    let valid = req.headers().get(&REQUEST_ID).is_none_or(|v| {
        let b = v.as_bytes();
        !b.is_empty()
            && b.len() <= 64
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b'_')
    });
    if !valid {
        req.headers_mut().remove(&REQUEST_ID);
    }
    req
}

/// `RequestBodyLimitLayer` and axum's body extractors answer 413 in plain text; render it as
/// problem+json like every other error.
async fn problem_for_body_limit<B>(res: Response<B>) -> Response
where
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let is_problem = res
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|ct| ct == PROBLEM_CONTENT_TYPE);
    if res.status() == StatusCode::PAYLOAD_TOO_LARGE && !is_problem {
        Error::PayloadTooLarge.into_response()
    } else {
        res.into_response()
    }
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
