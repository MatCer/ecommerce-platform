//! Catalog fixtures for later work packages (pricing, search, cart, feeds). They go through
//! the real `commerce::catalog` services, so fixtures obey the same validation and events.
//!
//! ```ignore
//! let product = testkit::catalog::product(&runtime, tenant, "TS", 4).await;
//! let mut input = testkit::catalog::product_input("MUG", 1);
//! input.status = ProductStatus::Draft;
//! let draft = testkit::catalog::create(&runtime, tenant, &input).await;
//! ```

use commerce::catalog::I18n;
use commerce::catalog::categories::{self, Category, CategoryTranslation, NewCategory};
use commerce::catalog::products::{
    self, OptionValue, Product, ProductInput, ProductOption, ProductStatus, ProductTranslation,
    VariantInput,
};
use sqlx::PgPool;
use uuid::Uuid;

/// Actor recorded in the audit log for fixture writes.
pub const ACTOR: &str = "testkit";

fn i18n(cs: &str, en: &str) -> I18n {
    I18n::from([
        ("cs".to_owned(), cs.to_owned()),
        ("en".to_owned(), en.to_owned()),
    ])
}

/// An active product with cs/en translations (slugs derived from `sku_prefix`) and
/// `variants` variants. More than one variant adds a `size` option with values `v1..vN`;
/// SKUs are `<prefix>-1..N`, the first variant is the default.
pub fn product_input(sku_prefix: &str, variants: usize) -> ProductInput {
    let slug = sku_prefix.to_ascii_lowercase();
    let options = if variants > 1 {
        vec![ProductOption {
            code: "size".into(),
            name_i18n: i18n("Velikost", "Size"),
            values: (1..=variants)
                .map(|i| OptionValue {
                    code: format!("v{i}"),
                    name_i18n: i18n(&format!("V{i}"), &format!("V{i}")),
                })
                .collect(),
        }]
    } else {
        vec![]
    };
    ProductInput {
        status: ProductStatus::Active,
        brand: Some("Testkit".into()),
        gpsr: Default::default(),
        unit_measure: None,
        unit_quantity: None,
        heureka_category: None,
        google_category: None,
        translations: ["cs", "en"]
            .iter()
            .map(|locale| ProductTranslation {
                locale: (*locale).into(),
                name: format!("Product {sku_prefix}"),
                slug: format!("{slug}-{locale}"),
                description_html: String::new(),
                short_description: String::new(),
                seo_title: None,
                seo_description: None,
            })
            .collect(),
        options,
        variants: (1..=variants)
            .map(|i| VariantInput {
                id: None,
                sku: format!("{sku_prefix}-{i}"),
                ean: None,
                option_values: if variants > 1 {
                    [("size".to_owned(), format!("v{i}"))].into()
                } else {
                    Default::default()
                },
                weight_g: Some(100),
                is_default: false,
            })
            .collect(),
        category_ids: vec![],
        media: vec![],
        parameters: vec![],
        tax_categories: Default::default(),
    }
}

/// Creates `input` in `tenant` (committed).
pub async fn create(runtime: &PgPool, tenant: Uuid, input: &ProductInput) -> Product {
    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    let product = products::create(&mut tx, ACTOR, input).await.unwrap();
    tx.commit().await.unwrap();
    product
}

/// [`product_input`] + [`create`].
pub async fn product(runtime: &PgPool, tenant: Uuid, sku_prefix: &str, variants: usize) -> Product {
    create(runtime, tenant, &product_input(sku_prefix, variants)).await
}

/// A category with a cs translation (`slug`), under `parent` (committed).
pub async fn category(
    runtime: &PgPool,
    tenant: Uuid,
    slug: &str,
    parent: Option<Uuid>,
) -> Category {
    let mut tx = platform::db::tenant_tx(runtime, tenant).await.unwrap();
    let category = categories::create(
        &mut tx,
        ACTOR,
        &NewCategory {
            parent_id: parent,
            image_asset_id: None,
            translations: vec![CategoryTranslation {
                locale: "cs".into(),
                name: slug.into(),
                slug: slug.into(),
                description_html: String::new(),
                seo_title: None,
                seo_description: None,
            }],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    category
}
