//! A small sellable shop for storefront and cart tests: a tenant with a CZ (CZK, cs) and an SK
//! (EUR, sk) market, verified domains, a storefront token, a CZ tax profile (destination VAT),
//! price lists, one category and a product with two variants in stock.

use commerce::inventory::{self, Adjustment};
use commerce::markets::{self, NewMarket, TaxMode};
use commerce::money::Currency;
use commerce::pricing::{self, NewPriceList};
use commerce::tax::{self, DistanceSalesMode, TaxProfileInput};
use sqlx::PgPool;
use uuid::Uuid;

use crate::catalog::{self, ACTOR};

pub struct Shop {
    pub tenant: Uuid,
    pub cz: Uuid,
    pub sk: Uuid,
    pub token: String,
    pub czk: Uuid,
    pub eur: Uuid,
    pub category: Uuid,
    pub product: Uuid,
    /// Two variants: 12 900 / 14 900 minor CZK, 520 / 600 minor EUR; 10 units each in stock.
    pub variants: [Uuid; 2],
    /// cs slug of the product.
    pub slug: String,
}

/// Creates the shop `slug` (hosts `<slug>.localhost`, `<slug>-sk.localhost`).
pub async fn shop(runtime: &PgPool, slug: &str) -> Shop {
    let (tenant, cz) = crate::tenant(runtime, slug).await;
    let token = format!("sf_{:064x}", Uuid::now_v7().as_u128());
    sqlx::query("INSERT INTO platform.storefront_tokens (token, tenant_id) VALUES ($1, $2)")
        .bind(&token)
        .bind(tenant)
        .execute(runtime)
        .await
        .unwrap();

    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    let sk = markets::create(
        &mut tx,
        ACTOR,
        &NewMarket {
            code: "sk".into(),
            name: "Slovensko".into(),
            country_codes: vec!["SK".into()],
            currency: "EUR".into(),
            default_locale: "sk".into(),
            locales: vec!["sk".into()],
            tax_mode: TaxMode::Gross,
            is_default: false,
        },
    )
    .await
    .unwrap()
    .id;
    for (host, market) in [
        (format!("{slug}.localhost"), cz),
        (format!("{slug}-sk.localhost"), sk),
    ] {
        sqlx::query(
            "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
             VALUES ($1, $2, $3, true, now())",
        )
        .bind(host)
        .bind(tenant)
        .bind(market)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tax::upsert(
        &mut tx,
        ACTOR,
        &TaxProfileInput {
            establishment_country: "CZ".into(),
            vat_payer: true,
            vat_id: Some("CZ12345678".into()),
            sk_ic_dph: None,
            distance_sales_mode: DistanceSalesMode::Destination,
            confirm_origin_threshold: false,
            cash_rounding_in_vat_base: false,
        },
    )
    .await
    .unwrap();
    let mut list = async |code: &str, currency: Currency, market: Uuid| {
        pricing::create_price_list(
            &mut tx,
            ACTOR,
            &NewPriceList {
                code: code.into(),
                name: code.into(),
                currency,
                market_ids: vec![market],
            },
        )
        .await
        .unwrap()
        .id
    };
    let czk = list("czk", Currency::Czk, cz).await;
    let eur = list("eur", Currency::Eur, sk).await;
    tx.commit().await.unwrap();

    let category = catalog::category(runtime, tenant, "trika", None).await;
    let mut input = catalog::product_input("TEE", 2);
    input.category_ids = vec![category.id];
    let product = catalog::create(runtime, tenant, &input).await;
    let variants = [product.variants[0].id, product.variants[1].id];
    crate::pricing::set_prices(
        runtime,
        tenant,
        czk,
        &[(variants[0], 12_900), (variants[1], 14_900)],
    )
    .await;
    crate::pricing::set_prices(
        runtime,
        tenant,
        eur,
        &[(variants[0], 520), (variants[1], 600)],
    )
    .await;
    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    for v in variants {
        inventory::adjust(
            &mut tx,
            ACTOR,
            v,
            &format!("init-{v}"),
            &Adjustment {
                delta: None,
                on_hand: Some(10),
                note: None,
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    Shop {
        tenant,
        cz,
        sk,
        token,
        czk,
        eur,
        category: category.id,
        product: product.id,
        variants,
        slug: "tee-cs".into(),
    }
}
