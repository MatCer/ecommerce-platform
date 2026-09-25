//! Recommendations Admin API (spec §8.3, §11.2): collections (staff), strategy settings and
//! exclusions (admin), and the "why recommended" explanation for staff.

use std::collections::BTreeMap;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::{DateTime, Utc};
use commerce::consent::{self, ConsentPurpose, Subject};
use commerce::recommendations::collections::{self, Collection, CollectionInput};
use commerce::recommendations::engine::{self, Affinity, Explained, Target, Visitor};
use commerce::recommendations::settings::{self, RecommendationSettings};
use commerce::storefront;
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
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
        .routes(routes!(list_collections, create_collection))
        .routes(routes!(
            get_collection,
            update_collection,
            delete_collection
        ))
        .routes(routes!(get_settings, put_settings))
        .routes(routes!(explain))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionList {
    pub items: Vec<Collection>,
}

/// Collections, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/collections",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = CollectionList))
)]
async fn list_collections(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<CollectionList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| collections::list(tx).await).await?;
    Ok(Json(CollectionList { items }))
}

/// Creates a collection. `seasonal` needs a schedule window; `manual` may have one. Honors
/// `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/collections",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = CollectionInput,
    responses(
        (status = 201, body = Collection),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_collection(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: CollectionInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/collections",
        &input,
        async |tx| collections::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/collections/{id}",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Collection),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_collection(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Collection>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            collections::get(tx, id).await
        })
        .await?,
    ))
}

/// Replaces a collection.
#[utoipa::path(
    put,
    path = "/admin/v1/collections/{id}",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CollectionInput,
    responses(
        (status = 200, body = Collection),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_collection(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Collection>, Error> {
    let id = path_id(id)?;
    let input: CollectionInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            collections::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/collections/{id}",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_collection(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        collections::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Which strategies run and which products are never recommended.
#[utoipa::path(
    get,
    path = "/admin/v1/recommendations/settings",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = RecommendationSettings))
)]
async fn get_settings(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<RecommendationSettings>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| settings::get(tx).await).await?,
    ))
}

/// Replaces the settings. Owner or admin.
#[utoipa::path(
    put,
    path = "/admin/v1/recommendations/settings",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = RecommendationSettings,
    responses(
        (status = 200, body = RecommendationSettings),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_settings(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<RecommendationSettings>, Error> {
    staff.require(Role::Admin)?;
    let input: RecommendationSettings = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            settings::put(tx, actor, &input).await
        })
        .await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExplainQuery {
    /// The market to recommend in.
    pub market_id: Uuid,
    /// As on the storefront: `product:<id>`, `category:<id>`, `collection:<id>`, `home`,
    /// `cart` or `recent`.
    pub context: Option<String>,
    /// 1-24 (default 8).
    pub limit: Option<u32>,
    /// `recent`: product ids; `cart`: the cart's product ids (comma-separated).
    pub ids: Option<String>,
    /// Recommend for this customer: their affinity, only if their `personalization` consent
    /// is granted.
    pub customer_id: Option<Uuid>,
    /// Evaluate as of this time (preview a scheduled collection). Default: now.
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecommendationExplain {
    /// The customer's `personalization` consent is granted (always false without one).
    pub personalization: bool,
    /// The customer's interests, when personalization is allowed.
    pub affinity: Option<Affinity>,
    pub result: Explained,
    /// Names of the skipped products (recommended ones carry theirs in the card).
    pub names: BTreeMap<Uuid, String>,
}

fn ids(raw: Option<&str>) -> Vec<Uuid> {
    raw.unwrap_or_default()
        .split(',')
        .filter_map(|s| Uuid::parse_str(s.trim()).ok())
        .take(engine::MAX_LIMIT)
        .collect()
}

/// Why these products: the strategy chain for a context, every recommended product with its
/// strategy and score, and every candidate that was left out with the reason (excluded, not
/// sold in the market, out of stock, duplicate, ...).
#[utoipa::path(
    get,
    path = "/admin/v1/recommendations/explain",
    tag = "recommendations",
    security(("staff_jwt" = [])),
    params(TenantHeader, ExplainQuery),
    responses(
        (status = 200, body = RecommendationExplain),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn explain(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<ExplainQuery>, QueryRejection>,
) -> Result<Json<RecommendationExplain>, Error> {
    let q = query_params(query)?;
    let context = q.context.as_deref().unwrap_or("home");
    let target = Target::parse(context, q.ids.as_deref())?;
    let limit = engine::limit(q.limit);
    let now = q.at.unwrap_or_else(Utc::now);
    let out = in_tx(&s, staff.tenant_id, async |tx| {
        // Shoppers only reach markets with a verified domain; say so instead of a bare 404.
        let ctx = storefront::context(tx, &s.public_urls, q.market_id, None, now)
            .await
            .map_err(|e| match e {
                Error::NotFound | Error::Forbidden { .. } => Error::Validation {
                    code: "market_unpublished",
                    detail: "the market does not exist or has no verified domain yet".into(),
                },
                e => e,
            })?;
        let mut visitor = Visitor::default();
        if target == Target::Cart {
            visitor.cart = ids(q.ids.as_deref());
        }
        let mut affinity = None;
        if let Some(customer) = q.customer_id {
            visitor.personalization = consent::current(
                tx,
                &Subject::Customer(customer),
                ConsentPurpose::Personalization,
            )
            .await?;
            if visitor.personalization {
                let a = engine::customer_affinity(tx, customer).await?;
                affinity = Some(a.clone());
                visitor.affinity = Some(a);
            }
        }
        let settings = settings::get(tx).await?;
        let result = engine::recommend(tx, &ctx, &settings, &target, &visitor, limit).await?;
        let skipped: Vec<Uuid> = result.skipped.iter().map(|s| s.product_id).collect();
        let names = engine::product_names(tx, &skipped, &ctx.locale).await?;
        Ok(RecommendationExplain {
            personalization: visitor.personalization,
            affinity,
            result,
            names,
        })
    })
    .await?;
    Ok(Json(out))
}
