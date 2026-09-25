//! HTTP API (spec §8): health, readiness, OpenAPI, Swagger UI (dev only), the Admin API
//! (`/admin/v1`, staff JWT), the Storefront API (`/storefront/v1`, storefront token via the
//! edge) and the Internal API (`/internal/v1`, service token).

pub mod admin;
pub mod admin_analytics;
pub mod admin_catalog;
pub mod admin_content;
pub mod admin_feeds;
pub mod admin_fulfillment;
pub mod admin_inventory;
pub mod admin_media;
pub mod admin_orders;
pub mod admin_payments;
pub mod admin_platform;
pub mod admin_pricing;
pub mod admin_promotions;
pub mod admin_search;
pub mod admin_staff;
pub mod admin_storefront;
pub mod admin_webhooks;
pub mod auth;
pub mod auth_service;
pub mod cli;
pub use platform::edge;
pub mod internal;
pub mod rate_limit;
pub mod seed;
pub mod storefront;
pub mod storefront_search;
pub mod webhooks;

use std::sync::Arc;

use axum::Router;
use axum::body::{Bytes, HttpBody};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn, from_fn_with_state, map_request, map_response};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{BoxError, Json};
use platform::Error;
use platform::error::PROBLEM_CONTENT_TYPE;
use platform::health::{CheckStatus, Readiness};
use platform::storage::Storage;
use serde::Serialize;
use sqlx::PgPool;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::openapi::OpenApi as OpenApiSpec;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

/// Spec §8.1: request bodies are capped at 1 MB; large uploads go through presigned URLs.
pub const BODY_LIMIT_BYTES: usize = 1024 * 1024;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// The auth service's internal API (staff invitations); `None` when not configured.
    pub auth_service: Option<auth_service::AuthService>,
    pub http: reqwest::Client,
    /// Search-only key (A27): the API never writes to Meilisearch.
    pub meili: commerce::search::Meili,
    pub storage: Storage,
    pub staff_auth: Arc<auth::StaffAuth>,
    pub internal_token: auth::ServiceToken,
    /// The admin SPA origin, the only one CORS allows on `/admin/v1` (A9).
    pub admin_origin: HeaderValue,
    /// How public storefront URLs look (canonicals, sitemaps).
    pub public_urls: commerce::storefront::PublicUrls,
    pub edge: edge::EdgePurge,
    /// Payment gateways and the pickup-point widget (WP10).
    pub checkout: Arc<commerce::checkout::Settings>,
    /// Webhook secrets + SSRF-safe client; `None` without `SECRETS_KEY` (webhooks answer 503).
    pub webhooks: Option<commerce::webhooks::Webhooks>,
    /// Storefront API rate limits (§8.1).
    pub rate_limit: Arc<rate_limit::StorefrontLimiter>,
    /// Packeta/PPL (WP12); `None` in tools and tests without carrier endpoints.
    pub carriers: Option<commerce::carriers::Carriers>,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Commerce Platform API", version = "0.1.0"),
    components(schemas(platform::Problem)),
    modifiers(&SecuritySchemes),
    tags(
        (name = "staff", description = "Admin API: tenant staff management"),
        (name = "health", description = "Liveness and readiness"),
        (name = "admin", description = "Admin API: staff JWT from the auth service + X-Tenant-Id"),
        (name = "catalog", description = "Admin API: products, categories, parameters, tax categories"),
        (name = "media", description = "Admin API: image assets (presigned uploads, variants)"),
        (name = "pricing", description = "Admin API: tax profile, price lists, variant prices, price history"),
        (name = "promotions", description = "Admin API: sales and coupons"),
        (name = "inventory", description = "Admin API: stock levels and movements"),
        (name = "search", description = "Admin API: search index status and rebuilds"),
        (name = "storefront-admin", description = "Admin API: redirects and the storefront token"),
        (name = "feeds", description = "Admin API: feed imports (Heureka, Google) and export feeds"),
        (name = "content", description = "Admin API: pages, blog, menus, legal entity and templates, go-live checklist"),
        (name = "checkout", description = "Admin API: shipping and payment methods, orders"),
        (name = "payments", description = "Admin API: bank accounts and statements, payment exceptions, Stripe Connect, cash on delivery"),
        (name = "fulfillment", description = "Admin API: order management, labels and shipments, invoices and credit notes, refunds, withdrawals, carrier accounts"),
        (name = "analytics", description = "Admin API: the analytics dashboard"),
        (name = "webhooks-admin", description = "Admin API: outbound webhook subscriptions and deliveries"),
        (name = "platform", description = "Admin API for platform superadmins: the job queue"),
        (name = "webhooks", description = "Payment provider webhooks (signed)"),
        (name = "storefront", description = "Storefront API: page models, search, cart, checkout handoff (storefront token, via the edge)"),
        (name = "internal", description = "Internal API for platform services (service token)")
    )
)]
struct ApiDoc;

struct SecuritySchemes;

impl Modify for SecuritySchemes {
    fn modify(&self, spec: &mut OpenApiSpec) {
        let components = spec.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "staff_jwt",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some(
                        "EdDSA JWT from the auth service (`GET /api/auth/token`), aud=admin-api",
                    ))
                    .build(),
            ),
        );
        components.add_security_scheme(
            "service_token",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
    }
}

fn documented_routes() -> (Router<AppState>, OpenApiSpec) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(healthz))
        .routes(routes!(readyz))
        .merge(admin::routes())
        .merge(admin_staff::routes())
        .merge(admin_catalog::routes())
        .merge(admin_media::routes())
        .merge(admin_pricing::routes())
        .merge(admin_promotions::routes())
        .merge(admin_inventory::routes())
        .merge(admin_search::routes())
        .merge(admin_storefront::routes())
        .merge(admin_content::routes())
        .merge(admin_feeds::routes())
        .merge(admin_orders::routes())
        .merge(admin_payments::routes())
        .merge(admin_fulfillment::routes())
        .merge(admin_analytics::routes())
        .merge(admin_webhooks::routes())
        .merge(admin_platform::routes())
        .merge(storefront::routes())
        .merge(storefront_search::routes())
        .merge(internal::routes())
        .merge(webhooks::routes())
        .split_for_parts()
}

/// The OpenAPI document, as served at `/openapi.json`.
pub fn openapi() -> OpenApiSpec {
    documented_routes().1
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Commerce Platform Storefront API",
        version = "0.1.0",
        description = "Theme-facing page models and cart (spec §8.2). Reached through the edge only."
    ),
    components(schemas(platform::Problem)),
    tags((name = "storefront", description = "Page models, cart, checkout handoff"))
)]
struct StorefrontDoc;

/// The storefront subset (`api openapi --storefront`), source of the SDK's generated types.
pub fn openapi_storefront() -> OpenApiSpec {
    OpenApiRouter::<AppState>::with_openapi(StorefrontDoc::openapi())
        .merge(storefront::routes())
        .merge(storefront_search::routes())
        .split_for_parts()
        .1
}

/// CORS for the admin SPA (A9): one exact origin, bearer tokens (no cookies to this API).
/// Applied to the whole router (a per-route layer would lose preflights to the 405 fallback);
/// nothing else is meant for browsers: storefront traffic goes through the edge (A4).
fn admin_cors(origin: HeaderValue) -> CorsLayer {
    CorsLayer::new()
        .allow_origin([origin])
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static(auth::TENANT_HEADER),
            HeaderName::from_static("idempotency-key"),
        ])
        .expose_headers([REQUEST_ID, admin::REPLAYED])
        .max_age(std::time::Duration::from_secs(600))
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
        .layer(from_fn_with_state(state.clone(), rate_limit::storefront))
        .layer(from_fn(record_latency))
        .layer(admin_cors(state.admin_origin.clone()))
        // Outermost, so invalid ids are gone before `SetRequestIdLayer` looks at them.
        .layer(map_request(drop_invalid_request_id))
        .with_state(state)
}

/// `http_request_duration_seconds{method,route,status}` (route = the matched template, so
/// ids and tokens in paths never become labels).
async fn record_latency(req: Request, next: Next) -> Response {
    let started = std::time::Instant::now();
    // A fixed label set: extension methods must not mint new series.
    let method = match *req.method() {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    };
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let res = next.run(req).await;
    metrics::histogram!(
        "http_request_duration_seconds",
        "method" => method,
        "route" => route,
        "status" => res.status().as_u16().to_string(),
    )
    .record(started.elapsed().as_secs_f64());
    res
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

/// Readiness: database and object storage are reachable. Meilisearch is reported; when only
/// it fails the status is `degraded` and the response still 200 (spec A27).
#[utoipa::path(
    get,
    path = "/readyz",
    tag = "health",
    responses(
        (status = 200, description = "Ready (`ok`, or `degraded` without search)", body = Readiness),
        (status = 503, description = "A core dependency is unreachable", body = Readiness)
    )
)]
async fn readyz(State(s): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let report = platform::health::readiness(&s.db, &s.http, s.meili.url(), &s.storage).await;
    let status = if report.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}
