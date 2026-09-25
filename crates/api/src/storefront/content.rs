//! Content page models (spec §7.5, §8.2): CMS/legal pages and the blog.

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use commerce::storefront::content::{self, BlogIndex, BlogPost, CmsPage};
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(cms_page))
        .routes(routes!(blog_index))
        .routes(routes!(blog_post))
}

/// A published CMS or legal page (`/pages/<slug>` in themes).
#[utoipa::path(
    get,
    path = "/storefront/v1/pages/cms/{slug}",
    tag = "storefront",
    params(StorefrontHeaders, ("slug" = String, Path)),
    responses(
        (status = 200, body = CmsPage),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cms_page(
    shopper: Shopper,
    State(s): State<AppState>,
    slug: Result<Path<String>, PathRejection>,
) -> Result<Json<CmsPage>, Error> {
    let Path(slug) = slug.map_err(|_| Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        content::cms_page(tx, ctx, &slug).await
    })
    .await?
    .map(Json)
    .ok_or(Error::NotFound)
}

/// Published blog posts, newest first.
#[utoipa::path(
    get,
    path = "/storefront/v1/pages/blog",
    tag = "storefront",
    params(StorefrontHeaders),
    responses((status = 200, body = BlogIndex))
)]
async fn blog_index(shopper: Shopper, State(s): State<AppState>) -> Result<Json<BlogIndex>, Error> {
    with_ctx(&s, &shopper, async |tx, ctx| {
        content::blog_index(tx, ctx).await
    })
    .await
    .map(Json)
}

/// One published blog post (`/blog/<slug>` in themes).
#[utoipa::path(
    get,
    path = "/storefront/v1/pages/blog/{slug}",
    tag = "storefront",
    params(StorefrontHeaders, ("slug" = String, Path)),
    responses(
        (status = 200, body = BlogPost),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn blog_post(
    shopper: Shopper,
    State(s): State<AppState>,
    slug: Result<Path<String>, PathRejection>,
) -> Result<Json<BlogPost>, Error> {
    let Path(slug) = slug.map_err(|_| Error::NotFound)?;
    with_ctx(&s, &shopper, async |tx, ctx| {
        content::blog_post(tx, ctx, &slug).await
    })
    .await?
    .map(Json)
    .ok_or(Error::NotFound)
}
