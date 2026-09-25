//! Page models and public reads (spec §8.2).

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use commerce::redirects::{self, ResolvedRedirect};
use commerce::storefront::pages::{
    self, HomePage, ListingPage, ListingParams, Recommendations, SearchSuggest, ShopModel,
};
use commerce::storefront::product::{self, ProductPage};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::query_params;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(shop))
        .routes(routes!(home))
        .routes(routes!(category))
        .routes(routes!(product_page))
        .routes(routes!(search))
        .routes(routes!(suggest))
        .routes(routes!(recommendations))
        .routes(routes!(resolve_redirect))
}

/// Layout data: shop name, locale, markets, menus, consent config, messages, theme tokens.
#[utoipa::path(
    get,
    path = "/storefront/v1/shop",
    tag = "storefront",
    params(StorefrontHeaders),
    responses(
        (status = 200, body = ShopModel),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn shop(shopper: Shopper, State(s): State<AppState>) -> Result<Json<ShopModel>, Error> {
    with_ctx(&s, &shopper, async |tx, ctx| pages::shop(tx, ctx).await)
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/pages/home",
    tag = "storefront",
    params(StorefrontHeaders),
    responses((status = 200, body = HomePage))
)]
async fn home(shopper: Shopper, State(s): State<AppState>) -> Result<Json<HomePage>, Error> {
    with_ctx(&s, &shopper, async |tx, ctx| pages::home(tx, ctx).await)
        .await
        .map(Json)
}

/// Listing query: `sort`, `page`; any other key is a facet filter (`?color=red&size=m`).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct ListingQueryDoc {
    /// `recommended` (default), `price_asc`, `price_desc`, `newest`, `name`.
    sort: Option<String>,
    /// 1-based page (24 products per page).
    page: Option<u32>,
}

fn listing_params(
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<ListingParams, Error> {
    Ok(ListingParams::from_pairs(&query_params(query)?))
}

/// A category listing with facets, sort options and pagination. Filtered or re-sorted URLs
/// are `noindex,follow` with a canonical to the category (spec §9.5).
#[utoipa::path(
    get,
    path = "/storefront/v1/pages/category/{slug}",
    tag = "storefront",
    params(StorefrontHeaders, ("slug" = String, Path), ListingQueryDoc),
    responses(
        (status = 200, body = ListingPage),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn category(
    shopper: Shopper,
    State(s): State<AppState>,
    slug: Result<Path<String>, PathRejection>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<ListingPage>, Error> {
    let Path(slug) = slug.map_err(|_| Error::NotFound)?;
    let params = listing_params(query)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        pages::category(tx, ctx, &slug, &params).await
    })
    .await?
    .map(Json)
    .ok_or(Error::NotFound)
}

#[utoipa::path(
    get,
    path = "/storefront/v1/pages/product/{slug}",
    tag = "storefront",
    params(StorefrontHeaders, ("slug" = String, Path)),
    responses(
        (status = 200, body = ProductPage),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn product_page(
    shopper: Shopper,
    State(s): State<AppState>,
    slug: Result<Path<String>, PathRejection>,
) -> Result<Json<ProductPage>, Error> {
    let Path(slug) = slug.map_err(|_| Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        product::product_page(tx, ctx, &slug).await
    })
    .await?
    .map(Json)
    .ok_or(Error::NotFound)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct SearchQueryDoc {
    q: Option<String>,
    sort: Option<String>,
    page: Option<u32>,
}

/// Search results (name search until WP7's Meilisearch); always `noindex`.
#[utoipa::path(
    get,
    path = "/storefront/v1/pages/search",
    tag = "storefront",
    params(StorefrontHeaders, SearchQueryDoc),
    responses((status = 200, body = ListingPage))
)]
async fn search(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Json<ListingPage>, Error> {
    let params = listing_params(query)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        pages::search(tx, ctx, &params).await
    })
    .await
    .map(Json)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestQuery {
    /// At least 2 characters; at most 100 are used.
    pub q: Option<String>,
}

/// Typeahead: up to 5 products and 3 categories.
#[utoipa::path(
    get,
    path = "/storefront/v1/search/suggest",
    tag = "storefront",
    params(StorefrontHeaders, SuggestQuery),
    responses((status = 200, body = SearchSuggest))
)]
async fn suggest(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<SuggestQuery>, QueryRejection>,
) -> Result<Json<SearchSuggest>, Error> {
    let q = query_params(query)?.q.unwrap_or_default();
    with_ctx(&s, &shopper, async |tx, ctx| {
        pages::suggest(tx, ctx, &q).await
    })
    .await
    .map(Json)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecommendationsQuery {
    /// `product:<id>`, `cart` or `home`.
    pub context: Option<String>,
}

#[utoipa::path(
    get,
    path = "/storefront/v1/recommendations",
    tag = "storefront",
    params(StorefrontHeaders, RecommendationsQuery),
    responses((status = 200, body = Recommendations))
)]
async fn recommendations(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<RecommendationsQuery>, QueryRejection>,
) -> Result<Json<Recommendations>, Error> {
    let context = query_params(query)?.context.unwrap_or_default();
    with_ctx(&s, &shopper, async |tx, ctx| {
        pages::recommendations(tx, ctx, &context).await
    })
    .await
    .map(Json)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RedirectQuery {
    /// Requested path (`/stary-produkt`); the query string is ignored.
    pub path: String,
}

/// The redirect for a path the theme could not render (the edge asks on 404, spec §9.5).
#[utoipa::path(
    get,
    path = "/storefront/v1/redirects/resolve",
    tag = "storefront",
    params(StorefrontHeaders, RedirectQuery),
    responses(
        (status = 200, body = ResolvedRedirect),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn resolve_redirect(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<RedirectQuery>, QueryRejection>,
) -> Result<Json<ResolvedRedirect>, Error> {
    let path = query_params(query)?.path;
    with_ctx(&s, &shopper, async |tx, _ctx| {
        redirects::resolve(tx, &path).await
    })
    .await?
    .map(Json)
    .ok_or(Error::NotFound)
}
