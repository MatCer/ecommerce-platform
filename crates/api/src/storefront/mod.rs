//! Storefront API (`/storefront/v1`, spec §5.5, §8.2, A4). Called only by the edge (theme
//! SSR through the restricted `STOREFRONT` binding, islands through `/_p/*`), never by
//! browsers directly.
//!
//! Every call carries the tenant's public storefront token (`X-Storefront-Token`) and the
//! market the edge resolved from the host (`X-Market`). The token selects the tenant; the
//! market is loaded inside that tenant's RLS scope, so a market of another tenant is refused
//! (`403 market_mismatch`). A client `X-Tenant` header that disagrees with the token is
//! refused as well. Cart calls add the cart capability (`X-Cart-Token`).

mod cart;
mod files;
mod pages;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use chrono::Utc;
use commerce::storefront::{self, Context};
use commerce::tenancy;
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use uuid::Uuid;

use crate::AppState;

pub const TOKEN_HEADER: &str = "x-storefront-token";
pub const MARKET_HEADER: &str = "x-market";
pub const TENANT_HEADER: &str = "x-tenant";
pub const LOCALE_HEADER: &str = "x-locale";
pub const CART_HEADER: &str = "x-cart-token";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(pages::routes())
        .merge(cart::routes())
        .merge(files::routes())
}

fn header<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

/// A storefront caller: tenant from the token, market (and locale hint) from the edge.
#[derive(Debug, Clone)]
pub struct Shopper {
    pub tenant_id: Uuid,
    pub market_id: Uuid,
    pub locale: Option<String>,
}

impl FromRequestParts<AppState> for Shopper {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Error> {
        let token = header(parts, TOKEN_HEADER).ok_or(Error::Unauthorized {
            code: "missing_storefront_token",
        })?;
        let tenant_id = tenancy::storefront_token_tenant(&state.db, token)
            .await?
            .ok_or(Error::Unauthorized {
                code: "invalid_storefront_token",
            })?;
        if let Some(claimed) = header(parts, TENANT_HEADER)
            && Uuid::parse_str(claimed).ok() != Some(tenant_id)
        {
            return Err(Error::Forbidden {
                code: "tenant_mismatch",
            });
        }
        let market_id = header(parts, MARKET_HEADER)
            .and_then(|m| Uuid::parse_str(m).ok())
            .ok_or_else(|| Error::BadRequest {
                code: "market_required",
                detail: "X-Market must be the market id resolved by the edge".into(),
            })?;
        let locale = header(parts, LOCALE_HEADER)
            .filter(|l| l.len() <= 8)
            .map(str::to_owned);
        Ok(Self {
            tenant_id,
            market_id,
            locale,
        })
    }
}

/// Opens the tenant transaction, builds the context (validating the market) and runs `f`.
pub(crate) async fn with_ctx<T>(
    s: &AppState,
    shopper: &Shopper,
    f: impl AsyncFnOnce(&mut TenantTx, &Context) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut tx = tenant_tx(&s.db, shopper.tenant_id).await?;
    let ctx = storefront::context(
        &mut tx,
        &s.public_urls,
        shopper.market_id,
        shopper.locale.as_deref(),
        Utc::now(),
    )
    .await?;
    let out = f(&mut tx, &ctx).await?;
    tx.commit().await?;
    Ok(out)
}

/// The headers the edge injects (documentation only; `Shopper` reads them).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct StorefrontHeaders {
    /// The tenant's public storefront token.
    #[param(rename = "X-Storefront-Token")]
    x_storefront_token: String,
    /// Market id resolved from the shop host by the edge.
    #[param(rename = "X-Market")]
    x_market: Uuid,
    /// Locale hint (one of the market's locales).
    #[param(rename = "X-Locale")]
    x_locale: Option<String>,
}

/// The cart capability (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct CartHeader {
    /// Cart capability from the `cart` cookie (shop) or `__Host-cart` (checkout origin).
    #[param(rename = "X-Cart-Token")]
    x_cart_token: String,
}
