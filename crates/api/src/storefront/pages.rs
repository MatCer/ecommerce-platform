//! Page models and public reads (spec §8.2).

use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use commerce::consent::{self, ConsentPurpose, Subject, well_formed_anon};
use commerce::recommendations::engine::{self, Target, Visitor};
use commerce::redirects::{self, ResolvedRedirect};
use commerce::storefront::Search;
use commerce::storefront::pages::{
    self, HomePage, ListingPage, ListingParams, Recommendations, ShopModel,
};
use commerce::storefront::product::{self, ProductPage};
use commerce::{analytics, cart};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::customer::{CONSENT_SUBJECT_HEADER, header_str};
use super::{CART_HEADER, Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::query_params;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(shop))
        .routes(routes!(home))
        .routes(routes!(category))
        .routes(routes!(product_page))
        .routes(routes!(search))
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

/// The search engine (WP7) for listings; pages fall back to Postgres when it is degraded.
fn engine(s: &AppState) -> Search<'_> {
    Search {
        meili: &s.meili,
        storage: &s.storage,
    }
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
        pages::category(tx, ctx, Some(engine(&s)), &slug, &params).await
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
        pages::search(tx, ctx, Some(engine(&s)), &params).await
    })
    .await
    .map(Json)
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecommendationsQuery {
    /// `product:<id>`, `category:<id>`, `collection:<id>`, `home` (default), `cart` or
    /// `recent`.
    pub context: Option<String>,
    /// Products, 1-24 (default 8).
    pub limit: Option<u32>,
    /// `recent` only: the device's recently viewed product ids, comma-separated (at most 12).
    pub ids: Option<String>,
}

/// The visitor data the edge forwards on `/_p/recommendations` only (never on SSR or
/// `/_p/public/*`), documentation only.
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct VisitorHeaders {
    /// Shop cart capability (`cart` cookie): cross-sell for the cart's products.
    #[param(rename = "X-Cart-Token")]
    x_cart_token: Option<String>,
    /// Anonymous consent subject: personalization and recently viewed when its records grant
    /// `personalization` (A20).
    #[param(rename = "X-Consent-Subject")]
    x_consent_subject: Option<String>,
}

/// Recommended products (spec §11.2): bought together, bestsellers (per market, time-decayed),
/// seasonal collections, personalized picks and recently viewed, each filtered by market
/// visibility, status and availability, falling back to bestsellers. Without visitor headers
/// the answer is public and cacheable; with a cart or consent subject it is private
/// (`Cache-Control: private, no-store`, A2). Personal signals are used only while the
/// subject's `personalization` consent is granted (A20).
#[utoipa::path(
    get,
    path = "/storefront/v1/recommendations",
    tag = "storefront",
    params(StorefrontHeaders, VisitorHeaders, RecommendationsQuery),
    responses(
        (status = 200, body = Recommendations),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn recommendations(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<RecommendationsQuery>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?;
    let target = Target::parse(q.context.as_deref().unwrap_or("home"), q.ids.as_deref())?;
    let limit = engine::limit(q.limit);
    let cart_token = header_str(&headers, CART_HEADER);
    let subject = header_str(&headers, CONSENT_SUBJECT_HEADER).filter(|s| well_formed_anon(s));
    let private = cart_token.is_some() || subject.is_some();
    let recs = with_ctx(&s, &shopper, async |tx, ctx| {
        let mut visitor = Visitor::default();
        if let Some(token) = cart_token {
            visitor.cart = cart::product_ids(tx, ctx, token).await?;
        }
        if let Some(subject) = subject
            && consent::current(
                tx,
                &Subject::Anon(subject.to_owned()),
                ConsentPurpose::Personalization,
            )
            .await?
        {
            visitor.personalization = true;
            if matches!(target, Target::Home | Target::Cart) {
                let anon = analytics::anon_id(tx.tenant_id(), subject);
                visitor.affinity = Some(engine::anon_affinity(tx, &anon, ctx.now).await?);
            }
        }
        pages::recommendations(tx, ctx, &target, &visitor, limit, private).await
    })
    .await?;
    let mut res = Json(recs).into_response();
    if private {
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    Ok(res)
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
