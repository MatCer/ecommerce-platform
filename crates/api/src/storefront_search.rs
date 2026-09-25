//! Storefront search endpoints (`/storefront/v1`, spec §8.2, §11.1): full search with facets
//! and typeahead. Public catalog data only. Same authorization as every storefront call
//! (A4, A7): the storefront token selects the tenant, the edge's market is loaded under that
//! tenant's RLS (see [`crate::storefront::Shopper`]). Islands reach them as
//! `/_p/public/search` and `/_p/public/search/suggest` through the edge.
//!
//! Meilisearch down → `503 service_unavailable` (search is a degraded component, A27); the
//! rest of the storefront keeps working (category and search pages fall back to Postgres).

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use commerce::media::AssetVariant;
use commerce::search::query::{
    self, DEFAULT_PER_PAGE, SearchHit, SearchRequest, SearchResult, Sort, Suggestions,
};
use platform::Error;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::query_params;
use crate::storefront::{Shopper, StorefrontHeaders, with_ctx};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(search))
        .routes(routes!(suggest))
}

/// Search hits link images by the public-bucket URL; storefront pages must use the shop's own
/// origin (`/media/...`, CSP `img-src 'self'`).
fn same_origin(items: &mut [SearchHit]) {
    for v in items.iter_mut().flat_map(|h| h.image.iter_mut()) {
        let AssetVariant { key, url, .. } = v;
        *url = format!("/{key}");
    }
}

/// Results depend on the context headers only; the edge caches per tenant/market/locale.
fn cacheable(body: impl serde::Serialize) -> Response {
    let mut res = Json(body).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    h.insert(
        header::VARY,
        HeaderValue::from_static("x-storefront-token, x-market, x-locale"),
    );
    res
}

/// Documentation only: the query string is parsed by [`parse_request`].
#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
pub struct SearchParams {
    /// Search text (≤ 200 characters). Empty: browse (e.g. a category listing).
    q: Option<String>,
    #[param(inline)]
    sort: Option<Sort>,
    /// 1-based page (results beyond the first 1000 are not available).
    page: Option<u32>,
    /// 1-48, default 24.
    per_page: Option<u32>,
    /// Only products in this category or its subcategories.
    category: Option<Uuid>,
    /// Only variants that can be bought now.
    in_stock: Option<bool>,
    /// Gross price bounds in minor units (inclusive).
    price_min: Option<i64>,
    price_max: Option<i64>,
    /// Facet filters: `f.<facet key>=<value>`, repeatable (`f.opt.color=red&f.opt.color=blue`).
    /// Keys are the `facets[].key` of a previous response. Values of one facet are OR-ed,
    /// facets are AND-ed, and all of them must hold for one variant.
    #[param(rename = "f.{facet}")]
    facets: Option<String>,
}

fn parse_request(pairs: Vec<(String, String)>) -> Result<SearchRequest, Error> {
    let bad = |detail: String| Error::BadRequest {
        code: "invalid_query",
        detail,
    };
    let mut r = SearchRequest {
        page: 1,
        per_page: DEFAULT_PER_PAGE,
        ..Default::default()
    };
    for (key, value) in pairs {
        let num = |v: &str| {
            v.parse::<i64>()
                .map_err(|_| bad(format!("{key}: not a number")))
        };
        match key.as_str() {
            "q" => r.q = value,
            "sort" => {
                r.sort = serde_json::from_value(serde_json::Value::String(value))
                    .map_err(|_| bad("sort: relevance, price_asc, price_desc or newest".into()))?;
            }
            "page" | "per_page" => {
                let n =
                    u32::try_from(num(&value)?).map_err(|_| bad(format!("{key}: out of range")))?;
                if key == "page" {
                    r.page = n;
                } else {
                    r.per_page = n;
                }
            }
            "category" => {
                r.category_id =
                    Some(Uuid::parse_str(&value).map_err(|_| bad("category: not a uuid".into()))?);
            }
            "in_stock" => r.in_stock = matches!(value.as_str(), "true" | "1"),
            "price_min" => r.price_min = Some(num(&value)?),
            "price_max" => r.price_max = Some(num(&value)?),
            _ => match key.strip_prefix("f.") {
                Some(facet) => {
                    let values = r.filters.entry(facet.to_owned()).or_default();
                    if values.len() > 100 {
                        return Err(bad("too many filter values".into()));
                    }
                    values.push(value);
                }
                None => return Err(bad(format!("unknown parameter {key}"))),
            },
        }
    }
    Ok(r)
}

/// Product search with variant-correct facets (spec §11.1, A23).
///
/// Results are distinct products; `variant_id` is the variant that matched best. Facets list
/// every value found for the query and category; `available: false` values would match
/// nothing with the other active filters and are shown disabled. No counts (A23).
#[utoipa::path(
    get,
    path = "/storefront/v1/search",
    tag = "storefront",
    params(StorefrontHeaders, SearchParams),
    responses(
        (status = 200, body = SearchResult),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "Search is temporarily unavailable", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn search(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, Error> {
    let req = parse_request(query_params(query)?)?;
    let mut result = with_ctx(&s, &shopper, async |tx, ctx| {
        let scope = commerce::storefront::search_scope(tx, ctx).await?;
        query::search(tx, &s.meili, &s.storage, &scope, &req).await
    })
    .await?;
    same_origin(&mut result.items);
    Ok(cacheable(result))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestParams {
    /// What the shopper typed so far (≤ 200 characters).
    q: String,
}

/// Typeahead: up to 8 suggestions, matching categories (≤ 3) first, then products.
#[utoipa::path(
    get,
    path = "/storefront/v1/search/suggest",
    tag = "storefront",
    params(StorefrontHeaders, SuggestParams),
    responses(
        (status = 200, body = Suggestions),
        (status = 400, body = platform::Problem, content_type = "application/problem+json"),
        (status = 401, body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 503, description = "Search is temporarily unavailable", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn suggest(
    shopper: Shopper,
    State(s): State<AppState>,
    query: Result<Query<SuggestParams>, QueryRejection>,
) -> Result<Response, Error> {
    let q = query_params(query)?.q;
    let mut result = with_ctx(&s, &shopper, async |tx, ctx| {
        let scope = commerce::storefront::search_scope(tx, ctx).await?;
        query::suggest(tx, &s.meili, &s.storage, &scope, &q).await
    })
    .await?;
    same_origin(&mut result.products);
    Ok(cacheable(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn parses_repeated_facet_values_and_options() {
        let r = parse_request(pairs(&[
            ("q", "tričko"),
            ("f.opt.color", "red"),
            ("f.opt.color", "blue"),
            ("f.brand", "Acme, Inc."),
            ("sort", "price_asc"),
            ("page", "2"),
            ("in_stock", "true"),
            ("price_max", "50000"),
        ]))
        .unwrap();
        assert_eq!(r.q, "tričko");
        assert_eq!(r.filters["opt.color"], ["red", "blue"]);
        assert_eq!(r.filters["brand"], ["Acme, Inc."]);
        assert_eq!(r.sort, Sort::PriceAsc);
        assert_eq!((r.page, r.per_page), (2, DEFAULT_PER_PAGE));
        assert!(r.in_stock);
        assert_eq!(r.price_max, Some(50_000));
    }

    #[test]
    fn rejects_unknown_and_malformed_parameters() {
        for p in [
            pairs(&[("filter", "x")]),
            pairs(&[("sort", "cheapest")]),
            pairs(&[("page", "-1")]),
            pairs(&[("category", "nope")]),
            pairs(&[("price_min", "1e3")]),
        ] {
            assert!(parse_request(p).is_err());
        }
    }
}
