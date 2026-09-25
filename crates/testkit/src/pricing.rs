//! Pricing fixtures for later work packages (cart, search, storefront), through the real
//! `commerce::pricing` services.
//!
//! ```ignore
//! let list = testkit::pricing::price_list(&runtime, tenant, "cz", Currency::Czk).await;
//! testkit::pricing::set_prices(&runtime, tenant, list.id, &[(variant_id, 12_900)]).await;
//! ```

use commerce::money::Currency;
use commerce::pricing::{self, NewPriceList, PriceItem, PriceList, PriceUpsert};
use sqlx::PgPool;
use uuid::Uuid;

use crate::catalog::ACTOR;

/// A price list `code` in `currency`, not attached to any market (committed).
pub async fn price_list(
    runtime: &PgPool,
    tenant: Uuid,
    code: &str,
    currency: Currency,
) -> PriceList {
    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    let list = pricing::create_price_list(
        &mut tx,
        ACTOR,
        &NewPriceList {
            code: code.into(),
            name: code.into(),
            currency,
            market_ids: vec![],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    list
}

/// Sets gross base prices `(variant, amount_minor)` in a list (committed).
pub async fn set_prices(runtime: &PgPool, tenant: Uuid, list: Uuid, prices: &[(Uuid, i64)]) {
    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    pricing::upsert_prices(
        &mut tx,
        ACTOR,
        list,
        &PriceUpsert {
            reason: Default::default(),
            imported: false,
            items: prices
                .iter()
                .map(|(variant_id, amount_minor)| PriceItem {
                    variant_id: *variant_id,
                    amount_minor: *amount_minor,
                    compare_at_minor: None,
                })
                .collect(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
