//! Catalog Admin API (spec §8.3): products, categories, parameters, tax categories.
//! Any member may edit the catalog (role `staff` and up). Creates honor `Idempotency-Key`;
//! every mutation is audited and publishes an outbox event (in `commerce::catalog`).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::{NaiveDate, Utc};
use commerce::catalog::categories::{
    self, Category, CategoryMove, CategoryNode, CategoryUpdate, NewCategory,
};
use commerce::catalog::parameters::{self, Parameter, ParameterInput, ParameterPage};
use commerce::catalog::products::{
    self, Product, ProductFilter, ProductInput, ProductPage, ProductStatus,
};
use commerce::catalog::tax::{self, TaxCategory};
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
        .routes(routes!(list_products, create_product))
        .routes(routes!(get_product, replace_product, delete_product))
        .routes(routes!(category_tree, create_category))
        .routes(routes!(get_category, update_category, delete_category))
        .routes(routes!(move_category))
        .routes(routes!(list_parameters, create_parameter))
        .routes(routes!(get_parameter, update_parameter, delete_parameter))
        .routes(routes!(list_tax_categories))
}

// ---------------------------------------------------------------------------------------
// Products

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProductQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
    pub status: Option<ProductStatus>,
    /// Only products in this category (not its subcategories).
    pub category_id: Option<Uuid>,
    /// Case-insensitive substring of a product name (any locale) or a SKU.
    pub q: Option<String>,
}

/// Products, newest first, with filters and cursor pagination.
#[utoipa::path(
    get,
    path = "/admin/v1/products",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, ProductQuery),
    responses(
        (status = 200, body = ProductPage),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_products(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<ProductQuery>, QueryRejection>,
) -> Result<Json<ProductPage>, Error> {
    let q = query_params(query)?;
    let filter = ProductFilter {
        status: q.status,
        category_id: q.category_id,
        q: q.q,
    };
    let page = in_tx(&s, staff.tenant_id, async |tx| {
        products::list(tx, &filter, q.cursor, q.limit.unwrap_or(50)).await
    })
    .await?;
    Ok(Json(page))
}

/// Creates a product document (attributes, translations, options, variants, categories,
/// media, parameter values, tax categories). Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/products",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = ProductInput,
    responses(
        (status = 201, body = Product),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json",
         description = "`sku_taken`, `slug_taken`, `idempotency_conflict`"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_product(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ProductInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/products",
        &input,
        async |tx| products::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/products/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Product),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_product(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Product>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| products::get(tx, id).await).await?,
    ))
}

/// Replaces the whole product document. Variants listed with an `id` are updated in place
/// (their ids stay stable), variants without one are created, unlisted ones are deleted.
#[utoipa::path(
    put,
    path = "/admin/v1/products/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ProductInput,
    responses(
        (status = 200, body = Product),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn replace_product(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Product>, Error> {
    let id = path_id(id)?;
    let input: ProductInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            products::replace(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/products/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_product(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        products::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Categories

#[derive(Serialize, ToSchema)]
pub struct CategoryTree {
    /// Root categories with nested `children`, siblings in position order.
    pub items: Vec<CategoryNode>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/categories",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = CategoryTree))
)]
async fn category_tree(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<CategoryTree>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| categories::tree(tx).await).await?;
    Ok(Json(CategoryTree { items }))
}

/// Creates a category at the end of its siblings. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/categories",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = NewCategory,
    responses(
        (status = 201, body = Category),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_category(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: NewCategory = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/categories",
        &input,
        async |tx| categories::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/categories/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Category),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_category(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Category>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            categories::get(tx, id).await
        })
        .await?,
    ))
}

/// Updates translations and image; use `/move` to change the parent or position.
#[utoipa::path(
    put,
    path = "/admin/v1/categories/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CategoryUpdate,
    responses(
        (status = 200, body = Category),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_category(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Category>, Error> {
    let id = path_id(id)?;
    let input: CategoryUpdate = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            categories::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Moves a category with its subtree. Into its own subtree: `422 category_cycle`.
#[utoipa::path(
    post,
    path = "/admin/v1/categories/{id}/move",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CategoryMove,
    responses(
        (status = 200, body = Category),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn move_category(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Category>, Error> {
    let id = path_id(id)?;
    let input: CategoryMove = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            categories::move_to(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes a category without subcategories (`409 has_children` otherwise).
#[utoipa::path(
    delete,
    path = "/admin/v1/categories/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_category(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        categories::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Parameters

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ParameterQuery {
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
    /// Page size, 1-100 (default 100).
    pub limit: Option<i64>,
}

/// Parameters ordered by key.
#[utoipa::path(
    get,
    path = "/admin/v1/parameters",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, ParameterQuery),
    responses(
        (status = 200, body = ParameterPage),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_parameters(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<ParameterQuery>, QueryRejection>,
) -> Result<Json<ParameterPage>, Error> {
    let q = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            parameters::list(tx, q.cursor.as_deref(), q.limit.unwrap_or(100)).await
        })
        .await?,
    ))
}

/// Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/parameters",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = ParameterInput,
    responses(
        (status = 201, body = Parameter),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_parameter(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: ParameterInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/parameters",
        &input,
        async |tx| parameters::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/parameters/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Parameter),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_parameter(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Parameter>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            parameters::get(tx, id).await
        })
        .await?,
    ))
}

/// The kind cannot change while products have values (`409 parameter_in_use`).
#[utoipa::path(
    put,
    path = "/admin/v1/parameters/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ParameterInput,
    responses(
        (status = 200, body = Parameter),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_parameter(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Parameter>, Error> {
    let id = path_id(id)?;
    let input: ParameterInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            parameters::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes the parameter and its values on all products.
#[utoipa::path(
    delete,
    path = "/admin/v1/parameters/{id}",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_parameter(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        parameters::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------
// Tax categories (read-only statutory data, A3)

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TaxQuery {
    /// ISO 3166-1 alpha-2, e.g. `CZ`; all EU countries when omitted.
    pub country: Option<String>,
    /// Rates in force on this date (default today).
    pub at: Option<NaiveDate>,
}

#[derive(Serialize, ToSchema)]
pub struct TaxCategoryList {
    pub items: Vec<TaxCategory>,
}

/// Tax categories and rates in force on a date. Products map to one per country
/// (`tax_categories` in the product document) and default to `standard`.
#[utoipa::path(
    get,
    path = "/admin/v1/tax-categories",
    tag = "catalog",
    security(("staff_jwt" = [])),
    params(TenantHeader, TaxQuery),
    responses(
        (status = 200, body = TaxCategoryList),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn list_tax_categories(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<TaxQuery>, QueryRejection>,
) -> Result<Json<TaxCategoryList>, Error> {
    let q = query_params(query)?;
    let at = q.at.unwrap_or_else(|| Utc::now().date_naive());
    let items = in_tx(&s, staff.tenant_id, async |tx| {
        tax::list(tx, q.country.as_deref(), at).await
    })
    .await?;
    Ok(Json(TaxCategoryList { items }))
}
