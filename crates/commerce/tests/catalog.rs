//! Catalog services against a real Postgres, as the runtime role (RLS on).
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use chrono::NaiveDate;
use commerce::catalog::categories::{
    self, CategoryMove, CategoryTranslation, CategoryUpdate, NewCategory,
};
use commerce::catalog::parameters::{self, ParameterInput, ParameterKind};
use commerce::catalog::products::{
    self, OptionValue, ParameterValue, ProductFilter, ProductInput, ProductMedia, ProductOption,
    ProductStatus, ProductTranslation, VariantInput,
};
use commerce::catalog::{I18n, tax};
use platform::db::{TenantTx, tenant_tx};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn i18n(pairs: &[(&str, &str)]) -> I18n {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn translation(locale: &str, name: &str, slug: &str) -> ProductTranslation {
    ProductTranslation {
        locale: locale.into(),
        name: name.into(),
        slug: slug.into(),
        description_html: "<p onclick=\"x()\">Bavlna<script>alert(1)</script></p>".into(),
        short_description: String::new(),
        seo_title: None,
        seo_description: None,
    }
}

fn option(code: &str, values: &[&str]) -> ProductOption {
    ProductOption {
        code: code.into(),
        name_i18n: i18n(&[("cs", code)]),
        values: values
            .iter()
            .map(|v| OptionValue {
                code: (*v).into(),
                name_i18n: i18n(&[("cs", v)]),
            })
            .collect(),
    }
}

fn variant(sku: &str, color: &str, size: &str) -> VariantInput {
    VariantInput {
        id: None,
        sku: sku.into(),
        ean: None,
        option_values: [("color", color), ("size", size)]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        weight_g: Some(180),
        is_default: false,
    }
}

fn tshirt(prefix: &str, slug: &str) -> ProductInput {
    ProductInput {
        status: ProductStatus::Active,
        brand: Some("Basic".into()),
        gpsr: Default::default(),
        unit_measure: None,
        unit_quantity: None,
        heureka_category: Some("Oblečení | Trička".into()),
        google_category: None,
        translations: vec![
            translation("cs", "Tričko Basic", slug),
            translation("sk", "Tričko Basic", slug),
            translation("en", "Basic T-shirt", &format!("{slug}-en")),
        ],
        options: vec![
            option("color", &["red", "blue"]),
            option("size", &["s", "m"]),
        ],
        variants: vec![
            VariantInput {
                ean: Some("4006381333931".into()),
                ..variant(&format!("{prefix}-R-S"), "red", "s")
            },
            variant(&format!("{prefix}-R-M"), "red", "m"),
            variant(&format!("{prefix}-B-S"), "blue", "s"),
            variant(&format!("{prefix}-B-M"), "blue", "m"),
        ],
        category_ids: vec![],
        media: vec![],
        parameters: vec![],
        tax_categories: BTreeMap::new(),
    }
}

fn cat(name: &str, slug: &str, parent: Option<Uuid>) -> NewCategory {
    NewCategory {
        parent_id: parent,
        image_asset_id: None,
        translations: vec![CategoryTranslation {
            locale: "cs".into(),
            name: name.into(),
            slug: slug.into(),
            description_html: String::new(),
            seo_title: None,
            seo_description: None,
        }],
    }
}

async fn asset(tx: &mut TenantTx) -> Uuid {
    sqlx::query_scalar("INSERT INTO assets (tenant_id, key) VALUES ($1, 'k') RETURNING id")
        .bind(tx.tenant_id())
        .fetch_one(&mut **tx)
        .await
        .unwrap()
}

async fn events(owner: &PgPool, tenant: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT type FROM queue.outbox WHERE tenant_id = $1 ORDER BY id")
        .bind(tenant)
        .fetch_all(owner)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_document_round_trip(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();

    let root = categories::create(&mut tx, "u1", &cat("Oblečení", "obleceni", None))
        .await
        .unwrap();
    let tees = categories::create(&mut tx, "u1", &cat("Trička", "tricka", Some(root.id)))
        .await
        .unwrap();
    let material = parameters::create(
        &mut tx,
        "u1",
        &ParameterInput {
            key: "material".into(),
            name_i18n: i18n(&[("cs", "Materiál")]),
            kind: ParameterKind::Text,
            unit: None,
            filterable: true,
        },
    )
    .await
    .unwrap();
    let chest = parameters::create(
        &mut tx,
        "u1",
        &ParameterInput {
            key: "chest".into(),
            name_i18n: i18n(&[("cs", "Obvod hrudníku")]),
            kind: ParameterKind::Number,
            unit: Some("cm".into()),
            filterable: false,
        },
    )
    .await
    .unwrap();
    let (a1, a2) = (asset(&mut tx).await, asset(&mut tx).await);

    let mut input = tshirt("TS", "tricko-basic");
    input.category_ids = vec![tees.id];
    input.media = vec![
        ProductMedia {
            asset_id: a2,
            variant_sku: None,
            alt_i18n: i18n(&[("cs", "Zepředu")]),
        },
        ProductMedia {
            asset_id: a1,
            variant_sku: Some("TS-B-M".into()),
            alt_i18n: I18n::new(),
        },
    ];
    input.parameters = vec![
        ParameterValue {
            parameter_id: material.id,
            variant_sku: None,
            value: json!({"cs": "bavlna"}),
        },
        ParameterValue {
            parameter_id: chest.id,
            variant_sku: Some("TS-R-M".into()),
            value: json!(104),
        },
    ];
    input.tax_categories.insert("CZ".into(), "reduced".into());
    let product = products::create(&mut tx, "u1", &input).await.unwrap();

    assert_eq!(product.variants.len(), 4);
    assert!(
        product.variants[0].is_default,
        "first variant is the default"
    );
    assert_eq!(product.variants.iter().filter(|v| v.is_default).count(), 1);
    assert_eq!(product.media[0].asset_id, a2, "media keep their order");
    assert_eq!(product.media[1].variant_sku.as_deref(), Some("TS-B-M"));
    assert_eq!(product.parameters[1].variant_sku.as_deref(), Some("TS-R-M"));
    assert_eq!(product.category_ids, vec![tees.id]);
    assert_eq!(product.tax_categories["CZ"], "reduced");
    let cs = product
        .translations
        .iter()
        .find(|t| t.locale == "cs")
        .unwrap();
    assert!(!cs.description_html.contains("script") && !cs.description_html.contains("onclick"));
    assert!(cs.description_html.contains("Bavlna"));
    assert_eq!(products::get(&mut tx, product.id).await.unwrap(), product);

    // A3: mapped category for CZ, standard elsewhere.
    let at = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
    let cz = tax::product_rate(&mut tx, product.id, "CZ", at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((cz.code.as_str(), cz.rate.as_str()), ("reduced", "12"));
    let sk = tax::product_rate(&mut tx, product.id, "SK", at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((sk.code.as_str(), sk.rate.as_str()), ("standard", "23"));

    // Second product, then list with filters and cursor.
    let mut mug = tshirt("MUG", "hrnek");
    mug.status = ProductStatus::Draft;
    for t in &mut mug.translations {
        t.name = "Hrnek".into();
    }
    let other = products::create(&mut tx, "u1", &mug).await.unwrap();
    let all = products::list(&mut tx, &ProductFilter::default(), None, 1)
        .await
        .unwrap();
    assert_eq!(all.items[0].id, other.id, "newest first");
    let rest = products::list(&mut tx, &ProductFilter::default(), all.next_cursor, 1)
        .await
        .unwrap();
    assert_eq!(rest.items[0].id, product.id);
    assert!(rest.next_cursor.is_none());
    for (filter, want) in [
        (
            ProductFilter {
                status: Some(ProductStatus::Draft),
                ..Default::default()
            },
            other.id,
        ),
        (
            ProductFilter {
                category_id: Some(root.id),
                ..Default::default()
            },
            Uuid::nil(),
        ),
        (
            ProductFilter {
                category_id: Some(tees.id),
                ..Default::default()
            },
            product.id,
        ),
        (
            ProductFilter {
                q: Some("ts-b-".into()),
                ..Default::default()
            },
            product.id,
        ),
        (
            ProductFilter {
                q: Some("BASIC T-SH".into()),
                ..Default::default()
            },
            product.id,
        ),
        (
            ProductFilter {
                q: Some("100%".into()),
                ..Default::default()
            },
            Uuid::nil(),
        ),
    ] {
        let found: Vec<Uuid> = products::list(&mut tx, &filter, None, 10)
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|p| p.id)
            .collect();
        let expected = if want.is_nil() { vec![] } else { vec![want] };
        assert_eq!(found, expected, "{filter:?}");
    }
    let summary = &products::list(&mut tx, &ProductFilter::default(), None, 10)
        .await
        .unwrap()
        .items[1];
    assert_eq!(summary.default_sku.as_deref(), Some("TS-R-S"));
    assert_eq!(summary.variant_count, 4);
    assert_eq!(summary.name["en"], "Basic T-shirt");

    products::delete(&mut tx, "u1", other.id).await.unwrap();
    assert_eq!(
        products::get(&mut tx, other.id).await.unwrap_err().code(),
        "not_found"
    );
    tx.commit().await.unwrap();

    let types = events(&db, tenant).await;
    for t in [
        "category.created",
        "parameter.created",
        "product.created",
        "product.deleted",
    ] {
        assert!(types.iter().any(|x| x == t), "{t} in {types:?}");
    }
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let audit = commerce::audit::list(&mut tx, None, 100).await.unwrap();
    assert!(
        audit
            .items
            .iter()
            .any(|e| e.action == "product.created" && e.actor == "u1")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn replace_upserts_variants_by_id(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let created = products::create(&mut tx, "u1", &tshirt("TS", "tricko"))
        .await
        .unwrap();
    let ids: Vec<Uuid> = created.variants.iter().map(|v| v.id).collect();

    // Swap the SKUs of the first two variants, drop the last, add a new one, move the default.
    let mut input = tshirt("TS", "tricko");
    input.options = vec![
        option("color", &["red", "blue", "green"]),
        option("size", &["s", "m"]),
    ];
    input.variants = vec![
        VariantInput {
            id: Some(ids[0]),
            ..variant("TS-R-M", "red", "s")
        },
        VariantInput {
            id: Some(ids[1]),
            ..variant("TS-R-S", "red", "m")
        },
        VariantInput {
            id: Some(ids[2]),
            is_default: true,
            ..variant("TS-B-S", "blue", "s")
        },
        variant("TS-G-S", "green", "s"),
    ];
    let replaced = products::replace(&mut tx, "u1", created.id, &input)
        .await
        .unwrap();
    assert_eq!(replaced.variants[0].id, ids[0]);
    assert_eq!(replaced.variants[0].sku, "TS-R-M");
    assert_eq!(replaced.variants[1].id, ids[1]);
    assert!(replaced.variants[2].is_default);
    assert_eq!(replaced.variants.iter().filter(|v| v.is_default).count(), 1);
    assert!(!replaced.variants.iter().any(|v| v.id == ids[3]));
    assert_eq!(replaced.variants.len(), 4);

    // An id of another product's variant is rejected.
    let other = products::create(&mut tx, "u1", &tshirt("MUG", "hrnek"))
        .await
        .unwrap();
    let mut input = tshirt("TS", "tricko");
    input.variants[0].id = Some(other.variants[0].id);
    let err = products::replace(&mut tx, "u1", created.id, &input)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unknown_variant");
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn conflicts_and_bad_references(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let (other_tenant, _) = testkit::tenant(&runtime, "other").await;
    let mut tx = tenant_tx(&runtime, other_tenant).await.unwrap();
    let foreign_cat = categories::create(&mut tx, "u2", &cat("Cizí", "cizi", None))
        .await
        .unwrap();
    // Same SKU and slug in another tenant are fine.
    products::create(&mut tx, "u2", &tshirt("TS", "tricko"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    products::create(&mut tx, "u1", &tshirt("TS", "tricko"))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let cases: Vec<(&str, ProductInput)> = vec![
        ("sku_taken", tshirt("TS", "jine-tricko")),
        ("slug_taken", tshirt("XX", "tricko")),
        (
            "unknown_reference",
            ProductInput {
                category_ids: vec![foreign_cat.id],
                ..tshirt("A", "a")
            },
        ),
        (
            "unknown_reference",
            ProductInput {
                media: vec![ProductMedia {
                    asset_id: Uuid::now_v7(),
                    variant_sku: None,
                    alt_i18n: I18n::new(),
                }],
                ..tshirt("B", "b")
            },
        ),
        (
            "unknown_reference",
            ProductInput {
                parameters: vec![ParameterValue {
                    parameter_id: Uuid::now_v7(),
                    variant_sku: None,
                    value: json!(1),
                }],
                ..tshirt("C", "c")
            },
        ),
        (
            "invalid_tax_category",
            ProductInput {
                tax_categories: [("DK".to_owned(), "reduced".to_owned())].into(),
                ..tshirt("D", "d")
            },
        ),
    ];
    for (code, input) in cases {
        let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
        let err = products::create(&mut tx, "u1", &input).await.unwrap_err();
        assert_eq!(err.code(), code, "{input:?}");
    }

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let p = parameters::create(
        &mut tx,
        "u1",
        &ParameterInput {
            key: "weight".into(),
            name_i18n: i18n(&[("cs", "Váha")]),
            kind: ParameterKind::Number,
            unit: None,
            filterable: false,
        },
    )
    .await
    .unwrap();
    let err = products::create(
        &mut tx,
        "u1",
        &ProductInput {
            parameters: vec![ParameterValue {
                parameter_id: p.id,
                variant_sku: None,
                value: json!("heavy"),
            }],
            ..tshirt("E", "e")
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "invalid_parameters");
    let dup = parameters::create(
        &mut tx,
        "u1",
        &ParameterInput {
            key: "weight".into(),
            name_i18n: i18n(&[("cs", "Váha")]),
            kind: ParameterKind::Text,
            unit: None,
            filterable: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(dup.code(), "key_taken");
}

#[sqlx::test(migrations = "../../migrations")]
async fn category_tree_moves_and_cycles(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let a = categories::create(&mut tx, "u1", &cat("A", "a", None))
        .await
        .unwrap();
    let b = categories::create(&mut tx, "u1", &cat("B", "b", None))
        .await
        .unwrap();
    let a1 = categories::create(&mut tx, "u1", &cat("A1", "a1", Some(a.id)))
        .await
        .unwrap();
    let a2 = categories::create(&mut tx, "u1", &cat("A2", "a2", Some(a.id)))
        .await
        .unwrap();
    let a11 = categories::create(&mut tx, "u1", &cat("A11", "a11", Some(a1.id)))
        .await
        .unwrap();
    assert_eq!(
        (a.position, b.position, a1.position, a2.position),
        (0, 1, 0, 1)
    );

    for (id, parent) in [(a.id, a11.id), (a.id, a.id), (a1.id, a11.id)] {
        let err = categories::move_to(
            &mut tx,
            "u1",
            id,
            &CategoryMove {
                parent_id: Some(parent),
                position: 0,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "category_cycle");
    }

    // Move A2 to the front of the root level: A2, A, B; A keeps only A1.
    categories::move_to(
        &mut tx,
        "u1",
        a2.id,
        &CategoryMove {
            parent_id: None,
            position: 0,
        },
    )
    .await
    .unwrap();
    let tree = categories::tree(&mut tx).await.unwrap();
    let roots: Vec<(Uuid, i32)> = tree
        .iter()
        .map(|n| (n.category.id, n.category.position))
        .collect();
    assert_eq!(roots, vec![(a2.id, 0), (a.id, 1), (b.id, 2)]);
    assert_eq!(tree[1].children.len(), 1);
    assert_eq!(tree[1].children[0].children[0].category.id, a11.id);

    // Move A under B at a large position: appended.
    categories::move_to(
        &mut tx,
        "u1",
        a.id,
        &CategoryMove {
            parent_id: Some(b.id),
            position: 99,
        },
    )
    .await
    .unwrap();
    let moved = categories::get(&mut tx, a.id).await.unwrap();
    assert_eq!((moved.parent_id, moved.position), (Some(b.id), 0));
    assert_eq!(
        categories::get(&mut tx, b.id).await.unwrap().position,
        1,
        "root renumbered"
    );

    let err = categories::delete(&mut tx, "u1", a.id).await.unwrap_err();
    assert_eq!(err.code(), "has_children");
    categories::delete(&mut tx, "u1", a11.id).await.unwrap();

    let renamed = categories::update(
        &mut tx,
        "u1",
        a1.id,
        &CategoryUpdate {
            image_asset_id: None,
            translations: cat("A1 new", "a1-new", None).translations,
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed.translations[0].slug, "a1-new");
    let err = categories::create(&mut tx, "u1", &cat("Dup", "a1-new", None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "slug_taken");
}

#[sqlx::test(migrations = "../../migrations")]
async fn tax_categories_follow_valid_from(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();

    let cz = tax::list(&mut tx, Some("CZ"), day(2026, 9, 25))
        .await
        .unwrap();
    let cz: Vec<(&str, &str)> = cz
        .iter()
        .map(|t| (t.code.as_str(), t.rate.as_str()))
        .collect();
    assert_eq!(cz, vec![("reduced", "12"), ("standard", "21")]);

    for (d, want) in [(day(2025, 12, 31), "14"), (day(2026, 1, 1), "13.5")] {
        let fi = tax::rate(&mut tx, "FI", "reduced", d)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fi.rate, want, "{d}");
    }

    let all = tax::list(&mut tx, None, day(2026, 9, 25)).await.unwrap();
    let countries: std::collections::BTreeSet<&str> =
        all.iter().map(|t| t.country.as_str()).collect();
    assert_eq!(countries.len(), 27, "EU-27");
    assert!(all.iter().filter(|t| t.code == "standard").count() == 27);

    // Reference data is read-only for the application.
    let err = sqlx::query("UPDATE platform.tax_categories SET rate = 0")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some("42501")
    );
}

const CATALOG_TABLES: &[&str] = &[
    "assets",
    "products",
    "product_translations",
    "product_options",
    "variants",
    "parameters",
    "product_parameter_values",
    "categories",
    "category_translations",
    "product_categories",
    "product_media",
    "product_tax_categories",
];

/// Fills every catalog table for the tenant.
async fn full_catalog(runtime: &PgPool, tenant: Uuid, prefix: &str) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let c = categories::create(&mut tx, "u", &cat("C", "c", None))
        .await
        .unwrap();
    let p = parameters::create(
        &mut tx,
        "u",
        &ParameterInput {
            key: "p".into(),
            name_i18n: i18n(&[("cs", "P")]),
            kind: ParameterKind::Bool,
            unit: None,
            filterable: true,
        },
    )
    .await
    .unwrap();
    let a = asset(&mut tx).await;
    let mut input = tshirt(prefix, "t");
    input.category_ids = vec![c.id];
    input.media = vec![ProductMedia {
        asset_id: a,
        variant_sku: None,
        alt_i18n: I18n::new(),
    }];
    input.parameters = vec![ParameterValue {
        parameter_id: p.id,
        variant_sku: None,
        value: json!(true),
    }];
    input
        .tax_categories
        .insert("SK".into(), "second_reduced".into());
    let product = products::create(&mut tx, "u", &input).await.unwrap();
    tx.commit().await.unwrap();
    product.id
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenants_cannot_reach_each_others_catalog(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let (b, _) = testkit::tenant(&runtime, "beta").await;
    let product_a = full_catalog(&runtime, a, "A").await;
    full_catalog(&runtime, b, "B").await;

    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    assert_eq!(
        products::get(&mut tx, product_a).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        products::replace(&mut tx, "u", product_a, &tshirt("X", "x"))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        products::delete(&mut tx, "u", product_a)
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
    let listed = products::list(&mut tx, &ProductFilter::default(), None, 100)
        .await
        .unwrap();
    assert!(listed.items.iter().all(|p| p.id != product_a));
    tx.rollback().await.unwrap();

    for table in CATALOG_TABLES {
        let mut tx = tenant_tx(&runtime, b).await.unwrap();
        let (own, foreign): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FILTER (WHERE tenant_id = $1), count(*) FILTER (WHERE tenant_id <> $1) FROM {table}"
        )))
        .bind(b)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(own > 0, "{table}: fixture row missing");
        assert_eq!(foreign, 0, "{table}: A's rows visible to B");
        let touched = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE tenant_id = $1"
        )))
        .bind(a)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(touched, 0, "{table}: deleted A's rows");
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = $1"
        )))
        .bind(a)
        .execute(&mut *tx)
        .await
        .unwrap_err();
        assert_eq!(
            err.as_database_error().unwrap().code().as_deref(),
            Some("42501"),
            "{table}"
        );
        tx.rollback().await.unwrap();
    }

    // B's own rows cannot reference A's product: composite (tenant_id, id) foreign keys.
    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    let err = sqlx::query(
        "INSERT INTO product_translations (tenant_id, product_id, locale, name, slug)
         VALUES ($1, $2, 'de', 'x', 'x')",
    )
    .bind(b)
    .bind(product_a)
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn testkit_fixtures_build_valid_products(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let root = testkit::catalog::category(&runtime, tenant, "root", None).await;
    let child = testkit::catalog::category(&runtime, tenant, "child", Some(root.id)).await;
    assert_eq!(child.parent_id, Some(root.id));

    let multi = testkit::catalog::product(&runtime, tenant, "FX", 3).await;
    let skus: Vec<&str> = multi.variants.iter().map(|v| v.sku.as_str()).collect();
    assert_eq!(skus, ["FX-1", "FX-2", "FX-3"]);
    assert!(multi.variants[0].is_default);
    assert_eq!(multi.options[0].values.len(), 3);

    let mut input = testkit::catalog::product_input("ONE", 1);
    input.category_ids = vec![child.id];
    let single = testkit::catalog::create(&runtime, tenant, &input).await;
    assert!(single.options.is_empty());
    assert_eq!(single.category_ids, vec![child.id]);
}

fn lock_timeout(err: &platform::Error) -> bool {
    matches!(err, platform::Error::Database(e)
        if e.as_database_error().and_then(|d| d.code()).as_deref() == Some("55P03"))
}

async fn impatient_tx(runtime: &PgPool, tenant: Uuid) -> TenantTx {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '300ms'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_writers_are_serialized(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "shop").await;
    let mut setup = tenant_tx(&runtime, tenant).await.unwrap();
    let param = parameters::create(
        &mut setup,
        "u1",
        &ParameterInput {
            key: "width".into(),
            name_i18n: i18n(&[("cs", "Šířka")]),
            kind: ParameterKind::Number,
            unit: None,
            filterable: false,
        },
    )
    .await
    .unwrap();
    setup.commit().await.unwrap();

    // A product save holding a number value blocks changing the parameter to text.
    let mut saving = tenant_tx(&runtime, tenant).await.unwrap();
    let mut input = tshirt("W", "w");
    input.parameters = vec![ParameterValue {
        parameter_id: param.id,
        variant_sku: None,
        value: json!(42),
    }];
    let product = products::create(&mut saving, "u1", &input).await.unwrap();
    let mut changing = impatient_tx(&runtime, tenant).await;
    let to_text = ParameterInput {
        key: "width".into(),
        name_i18n: i18n(&[("cs", "Šířka")]),
        kind: ParameterKind::Text,
        unit: None,
        filterable: false,
    };
    let err = parameters::update(&mut changing, "u2", param.id, &to_text)
        .await
        .unwrap_err();
    assert!(lock_timeout(&err), "{err:?}");
    changing.rollback().await.unwrap();
    saving.commit().await.unwrap();
    let mut changing = tenant_tx(&runtime, tenant).await.unwrap();
    let err = parameters::update(&mut changing, "u2", param.id, &to_text)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "parameter_in_use");
    changing.rollback().await.unwrap();

    // A read waits for an in-flight replace instead of mixing two versions.
    let mut replacing = tenant_tx(&runtime, tenant).await.unwrap();
    products::replace(&mut replacing, "u1", product.id, &tshirt("W", "w2"))
        .await
        .unwrap();
    let mut reading = impatient_tx(&runtime, tenant).await;
    let err = products::get(&mut reading, product.id).await.unwrap_err();
    assert!(lock_timeout(&err), "{err:?}");
    reading.rollback().await.unwrap();
    replacing.commit().await.unwrap();
}
