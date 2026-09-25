//! Search against a real Postgres and a real Meilisearch (spec §11.1, A23, A27).
//! `#[ignore]`d: run with `make test-search` (needs `make dev-infra` or the wp stack).
#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use commerce::catalog::products::{self, OptionValue, ProductInput, ProductOption, VariantInput};
use commerce::inventory::{self, Adjustment};
use commerce::money::Currency;
use commerce::pricing::{self, NewPriceList};
use commerce::search::index::{self, Indexed, Rebuilt};
use commerce::search::query::{self, SearchRequest, SearchResult, Sort};
use commerce::search::{Meili, index_uid};
use platform::db::tenant_tx;
use sqlx::PgPool;
use testkit::catalog::ACTOR;
use uuid::Uuid;

struct Shop {
    runtime: PgPool,
    meili: Meili,
    search_meili: Meili,
    tenant: Uuid,
    cz: Uuid,
    sk: Uuid,
    cz_list: Uuid,
    sk_list: Uuid,
}

/// A tenant with markets `cz` (cs, CZK) and `sk` (sk, EUR), each with its own price list.
async fn shop(db: &PgPool) -> Shop {
    let runtime = testkit::runtime_pool(db, 8).await;
    let slug = format!("s{}", &Uuid::now_v7().simple().to_string()[20..]);
    let (tenant, cz) = testkit::tenant(&runtime, &slug).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let sk: Uuid = sqlx::query_scalar(
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale, locales)
         VALUES ($1, 'sk', 'Slovensko', '{SK}', 'EUR', 'sk', '{sk}') RETURNING id",
    )
    .bind(tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let list = |code: &str, currency, market| NewPriceList {
        code: code.into(),
        name: code.into(),
        currency,
        market_ids: vec![market],
    };
    let cz_list = pricing::create_price_list(&mut tx, ACTOR, &list("cz", Currency::Czk, cz))
        .await
        .unwrap()
        .id;
    let sk_list = pricing::create_price_list(&mut tx, ACTOR, &list("sk", Currency::Eur, sk))
        .await
        .unwrap()
        .id;
    tx.commit().await.unwrap();
    Shop {
        runtime,
        meili: testkit::meili(),
        search_meili: testkit::meili_search(),
        tenant,
        cz,
        sk,
        cz_list,
        sk_list,
    }
}

fn option(code: &str, values: &[&str]) -> ProductOption {
    ProductOption {
        code: code.into(),
        name_i18n: [("cs".to_owned(), code.to_owned())].into(),
        values: values
            .iter()
            .map(|v| OptionValue {
                code: (*v).into(),
                name_i18n: [("cs".to_owned(), (*v).to_owned())].into(),
            })
            .collect(),
    }
}

/// A variant spec: option values, cz price, sk price, stock.
struct V<'a> {
    options: &'a [(&'a str, &'a str)],
    cz: Option<i64>,
    sk: Option<i64>,
    stock: i32,
}

impl Shop {
    /// Creates an active product (cs + sk names) with `variants`, prices and stock.
    async fn product(&self, sku: &str, cs: &str, sk: &str, variants: &[V<'_>]) -> Uuid {
        let mut input: ProductInput = testkit::catalog::product_input(sku, 1);
        input.translations[0].name = cs.into();
        input.translations[1].locale = "sk".into();
        input.translations[1].name = sk.into();
        let mut codes: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for v in variants {
            for (o, val) in v.options {
                codes.entry(o).or_default().insert(val);
            }
        }
        input.options = codes
            .iter()
            .map(|(o, vals)| option(o, &vals.iter().copied().collect::<Vec<_>>()))
            .collect();
        input.variants = variants
            .iter()
            .enumerate()
            .map(|(i, v)| VariantInput {
                id: None,
                sku: format!("{sku}-{i}"),
                ean: None,
                option_values: v
                    .options
                    .iter()
                    .map(|(o, val)| ((*o).to_owned(), (*val).to_owned()))
                    .collect(),
                weight_g: None,
                is_default: false,
            })
            .collect();
        let product = testkit::catalog::create(&self.runtime, self.tenant, &input).await;
        let pairs = |f: fn(&V) -> Option<i64>| -> Vec<(Uuid, i64)> {
            product
                .variants
                .iter()
                .zip(variants)
                .filter_map(|(pv, v)| f(v).map(|p| (pv.id, p)))
                .collect()
        };
        for (list, prices) in [
            (self.cz_list, pairs(|v| v.cz)),
            (self.sk_list, pairs(|v| v.sk)),
        ] {
            if !prices.is_empty() {
                testkit::pricing::set_prices(&self.runtime, self.tenant, list, &prices).await;
            }
        }
        let mut tx = tenant_tx(&self.runtime, self.tenant).await.unwrap();
        for (pv, v) in product.variants.iter().zip(variants) {
            if v.stock > 0 {
                let adj = Adjustment {
                    delta: Some(v.stock),
                    on_hand: None,
                    note: None,
                };
                inventory::adjust(&mut tx, ACTOR, pv.id, &pv.id.to_string(), &adj)
                    .await
                    .unwrap();
            }
        }
        tx.commit().await.unwrap();
        product.id
    }

    /// Indexes as a job dispatched now would.
    async fn index(&self, product: Uuid) -> Indexed {
        let version = commerce::search::next_version(&self.runtime).await.unwrap();
        index::index_product(
            &self.runtime,
            &self.meili,
            self.tenant,
            product,
            Some(version),
        )
        .await
        .unwrap()
    }

    async fn settle(&self) {
        for locale in ["cs", "sk"] {
            self.meili
                .wait_idle(&index_uid(self.tenant, locale), Duration::from_secs(60))
                .await
                .unwrap();
        }
    }

    async fn search(&self, market: Uuid, locale: &str, req: SearchRequest) -> SearchResult {
        let mut tx = tenant_tx(&self.runtime, self.tenant).await.unwrap();
        let scope = query::scope(&mut tx, market, locale)
            .await
            .unwrap()
            .unwrap();
        let r = query::search(
            &mut tx,
            &self.search_meili,
            &testkit::memory_storage(),
            &scope,
            &req,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        r
    }
}

fn req(filters: &[(&str, &[&str])]) -> SearchRequest {
    SearchRequest {
        filters: filters
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.iter().map(|s| (*s).to_owned()).collect()))
            .collect(),
        page: 1,
        per_page: 24,
        ..Default::default()
    }
}

fn ids(r: &SearchResult) -> BTreeSet<Uuid> {
    r.items.iter().map(|h| h.product_id).collect()
}

fn available(r: &SearchResult, facet: &str) -> BTreeSet<String> {
    r.facets
        .iter()
        .find(|f| f.key == facet)
        .map(|f| {
            f.values
                .iter()
                .filter(|v| v.available)
                .map(|v| v.value.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn shown(r: &SearchResult, facet: &str) -> BTreeSet<String> {
    r.facets
        .iter()
        .find(|f| f.key == facet)
        .map(|f| f.values.iter().map(|v| v.value.clone()).collect())
        .unwrap_or_default()
}

fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

/// A23 adversarial fixtures: cross-variant false matches, three options, unavailable variants,
/// market prices, multi-select filters.
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn filters_and_facets_are_variant_correct(db: PgPool) {
    let s = shop(&db).await;
    let c = |color, size| [("color", color), ("size", size)];
    // A: red/m + blue/xl. "red + xl" must not match (cross-variant).
    let (ar, ab) = (c("red", "m"), c("blue", "xl"));
    let a = s
        .product(
            "AAA",
            "Tričko",
            "Tričko",
            &[
                V {
                    options: &ar,
                    cz: Some(300),
                    sk: Some(12),
                    stock: 5,
                },
                V {
                    options: &ab,
                    cz: Some(320),
                    sk: Some(13),
                    stock: 5,
                },
            ],
        )
        .await;
    // B: red/xl, in stock.
    let br = c("red", "xl");
    let b = s
        .product(
            "BBB",
            "Mikina",
            "Mikina",
            &[V {
                options: &br,
                cz: Some(900),
                sk: Some(40),
                stock: 2,
            }],
        )
        .await;
    // C: three options; no variant is red + xl + regular.
    let (c1, c2, c3) = (
        [("color", "red"), ("size", "xl"), ("fit", "slim")],
        [("color", "red"), ("size", "m"), ("fit", "regular")],
        [("color", "blue"), ("size", "xl"), ("fit", "regular")],
    );
    let cc = s
        .product(
            "CCC",
            "Bunda",
            "Bunda",
            &[
                V {
                    options: &c1,
                    cz: Some(2000),
                    sk: Some(80),
                    stock: 1,
                },
                V {
                    options: &c2,
                    cz: Some(2100),
                    sk: Some(85),
                    stock: 1,
                },
                V {
                    options: &c3,
                    cz: Some(2200),
                    sk: Some(90),
                    stock: 1,
                },
            ],
        )
        .await;
    // D: red/xl sold only in SK.
    let dr = c("red", "xl");
    let d = s
        .product(
            "DDD",
            "Kalhoty",
            "Nohavice",
            &[V {
                options: &dr,
                cz: None,
                sk: Some(30),
                stock: 3,
            }],
        )
        .await;
    // E: red/xl out of stock, blue/m in stock.
    let (er, eb) = (c("red", "xl"), c("blue", "m"));
    let e = s
        .product(
            "EEE",
            "Ponožky",
            "Ponožky",
            &[
                V {
                    options: &er,
                    cz: Some(100),
                    sk: Some(4),
                    stock: 0,
                },
                V {
                    options: &eb,
                    cz: Some(110),
                    sk: Some(5),
                    stock: 4,
                },
            ],
        )
        .await;
    for p in [a, b, cc, d, e] {
        assert_eq!(s.index(p).await, Indexed::Done);
    }
    s.settle().await;

    let all = s.search(s.cz, "cs", req(&[])).await;
    assert_eq!(
        ids(&all),
        BTreeSet::from([a, b, cc, e]),
        "D is not sold in CZ"
    );
    assert_eq!(all.total, 4);
    assert_eq!(shown(&all, "opt.color"), set(&["blue", "red"]));
    assert_eq!(available(&all, "opt.fit"), set(&["regular", "slim"]));

    // An exact SKU returns exactly its product (and facets for it), however it is typed.
    let sku = s
        .search(
            s.cz,
            "cs",
            SearchRequest {
                q: "bbb-0".into(),
                ..req(&[])
            },
        )
        .await;
    assert_eq!(ids(&sku), BTreeSet::from([b]));
    assert_eq!((sku.total, sku.total_pages), (1, 1));
    assert_eq!(shown(&sku, "opt.color"), set(&["red"]));

    // Cross-variant: A has red and xl, but not in one variant.
    let red_xl = s
        .search(
            s.cz,
            "cs",
            req(&[("opt.color", &["red"]), ("opt.size", &["xl"])]),
        )
        .await;
    assert_eq!(ids(&red_xl), BTreeSet::from([b, cc, e]));
    // The hit is the matching variant, priced as that variant.
    let hit = red_xl.items.iter().find(|h| h.product_id == cc).unwrap();
    assert_eq!(hit.price.amount_minor, 2000);

    // Unavailable variants: E's red/xl is out of stock.
    let mut in_stock = req(&[("opt.color", &["red"]), ("opt.size", &["xl"])]);
    in_stock.in_stock = true;
    assert_eq!(
        ids(&s.search(s.cz, "cs", in_stock).await),
        BTreeSet::from([b, cc])
    );

    // Three options: no single variant of C is red + xl + regular.
    let three = s
        .search(
            s.cz,
            "cs",
            req(&[
                ("opt.color", &["red"]),
                ("opt.size", &["xl"]),
                ("opt.fit", &["regular"]),
            ]),
        )
        .await;
    assert!(three.items.is_empty(), "{:?}", ids(&three));
    assert_eq!(three.total, 0);
    // Facet values stay listed but disabled; the ones that would match again are available.
    assert_eq!(shown(&three, "opt.fit"), set(&["regular", "slim"]));
    assert_eq!(available(&three, "opt.fit"), set(&["slim"]));

    // Multi-select: OR within a facet, AND across facets.
    let multi = s
        .search(
            s.cz,
            "cs",
            req(&[("opt.color", &["red", "blue"]), ("opt.size", &["xl"])]),
        )
        .await;
    assert_eq!(ids(&multi), BTreeSet::from([a, b, cc, e]));

    // Facet availability ignores the facet's own selection, not the others'.
    let red_m = s
        .search(
            s.cz,
            "cs",
            req(&[("opt.color", &["red"]), ("opt.size", &["m"])]),
        )
        .await;
    assert_eq!(ids(&red_m), BTreeSet::from([a, cc]));
    assert_eq!(
        available(&red_m, "opt.color"),
        set(&["blue", "red"]),
        "blue/m exists (E)"
    );
    assert_eq!(
        available(&red_m, "opt.size"),
        set(&["m", "xl"]),
        "red/xl exists (B)"
    );
    let facet = red_m.facets.iter().find(|f| f.key == "opt.color").unwrap();
    assert!(facet.values.iter().any(|v| v.value == "red" && v.selected));

    // Market prices: each market filters and sorts on its own price.
    let mut cheap = req(&[]);
    cheap.price_max = Some(310);
    cheap.sort = Sort::PriceAsc;
    let cz_cheap = s.search(s.cz, "cs", cheap.clone()).await;
    assert_eq!(
        cz_cheap
            .items
            .iter()
            .map(|h| h.product_id)
            .collect::<Vec<_>>(),
        [e, a]
    );
    assert_eq!(cz_cheap.items[0].price.currency, Currency::Czk);
    let sk_all = s
        .search(
            s.sk,
            "sk",
            SearchRequest {
                sort: Sort::PriceDesc,
                ..req(&[])
            },
        )
        .await;
    assert_eq!(
        sk_all
            .items
            .iter()
            .map(|h| h.product_id)
            .collect::<Vec<_>>(),
        [cc, b, d, a, e],
        "sk prices: 90, 40, 30, 13, 5"
    );
    assert_eq!(sk_all.items[0].price.currency, Currency::Eur);
    assert_eq!(
        sk_all.price_range,
        Some(query::PriceRange {
            min_minor: 4,
            max_minor: 90
        })
    );
    // SK has no translation-less products: every name is the Slovak one.
    assert!(sk_all.items.iter().any(|h| h.name == "Nohavice"));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn stale_versions_changes_and_rehydration(db: PgPool) {
    let s = shop(&db).await;
    let (r, bl) = ([("color", "red")], [("color", "blue")]);
    let p = s
        .product(
            "TS",
            "Tričko",
            "Tričko",
            &[
                V {
                    options: &r,
                    cz: Some(300),
                    sk: None,
                    stock: 1,
                },
                V {
                    options: &bl,
                    cz: Some(300),
                    sk: None,
                    stock: 1,
                },
            ],
        )
        .await;
    // A job dispatched before an indexing run read the catalog is stale and dropped; one
    // dispatched after it is not.
    let dispatched_before = commerce::search::next_version(&s.runtime).await.unwrap();
    assert_eq!(s.index(p).await, Indexed::Done);
    let late = index::index_product(&s.runtime, &s.meili, s.tenant, p, Some(dispatched_before))
        .await
        .unwrap();
    assert_eq!(late, Indexed::Stale);
    assert_eq!(s.index(p).await, Indexed::Done);
    s.settle().await;
    assert_eq!(
        s.search(s.cz, "cs", req(&[("opt.color", &["blue"])]))
            .await
            .total,
        1
    );

    // Removing a variant removes its document.
    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let mut product = products::get(&mut tx, p).await.unwrap();
    let mut input = testkit::catalog::product_input("TS", 1);
    input.translations[0].name = "Tričko".into();
    input.options = vec![option("color", &["red"])];
    input.variants = vec![VariantInput {
        id: Some(product.variants[0].id),
        sku: product.variants[0].sku.clone(),
        ean: None,
        option_values: [("color".to_owned(), "red".to_owned())].into(),
        weight_g: None,
        is_default: true,
    }];
    product = products::replace(&mut tx, ACTOR, p, &input).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(product.variants.len(), 1);
    s.index(p).await;
    s.settle().await;
    assert_eq!(
        s.search(s.cz, "cs", req(&[("opt.color", &["blue"])]))
            .await
            .total,
        0
    );
    assert_eq!(
        s.search(s.cz, "cs", req(&[("opt.color", &["red"])]))
            .await
            .total,
        1
    );

    // Rehydration never substitutes another variant for one that stopped matching.
    let red = products::get(&mut tenant_tx(&s.runtime, s.tenant).await.unwrap(), p)
        .await
        .unwrap()
        .variants[0]
        .id;
    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let adj = Adjustment {
        delta: None,
        on_hand: Some(0),
        note: None,
    };
    inventory::adjust(&mut tx, ACTOR, red, "sold-out", &adj)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut only_in_stock = req(&[("opt.color", &["red"])]);
    only_in_stock.in_stock = true;
    let r = s.search(s.cz, "cs", only_in_stock).await;
    assert_eq!(r.total, 1, "not reindexed yet");
    assert!(r.items.is_empty(), "the red variant is sold out now");

    // Rehydration drops products that stopped being visible before the index caught up.
    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE products SET status = 'archived' WHERE id = $1")
        .bind(p)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let stale = s.search(s.cz, "cs", req(&[])).await;
    assert_eq!(stale.total, 1, "still in the index");
    assert!(stale.items.is_empty(), "but not shown");
    // Reindexing removes it.
    s.index(p).await;
    s.settle().await;
    assert_eq!(s.search(s.cz, "cs", req(&[])).await.total, 0);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn rebuild_swaps_in_a_complete_index(db: PgPool) {
    let s = shop(&db).await;
    let one = [("size", "m")];
    let a = s
        .product(
            "RA",
            "Tričko",
            "Tričko",
            &[V {
                options: &one,
                cz: Some(1),
                sk: Some(1),
                stock: 1,
            }],
        )
        .await;
    let b = s
        .product(
            "RB",
            "Mikina",
            "Mikina",
            &[V {
                options: &one,
                cz: Some(2),
                sk: Some(2),
                stock: 1,
            }],
        )
        .await;
    s.index(a).await; // b is not indexed incrementally
    s.settle().await;
    assert_eq!(s.search(s.cz, "cs", req(&[])).await.total, 1);

    assert_eq!(
        index::rebuild(&s.runtime, &s.meili, s.tenant, 1000, None)
            .await
            .unwrap(),
        Rebuilt::Done
    );
    s.settle().await;
    let after = s.search(s.cz, "cs", req(&[])).await;
    assert_eq!(ids(&after), BTreeSet::from([a, b]));
    assert_eq!(
        ids(&s.search(s.sk, "sk", req(&[])).await),
        BTreeSet::from([a, b])
    );

    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let status = index::status(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(status.len(), 2);
    for st in status {
        assert!(!st.rebuilding && st.rebuilt_at.is_some());
        assert_eq!(st.documents, Some(2));
        assert_eq!(st.index, index_uid(s.tenant, &st.locale));
    }
    // The temporary index is gone.
    let tmp = format!("{}__r1000", index_uid(s.tenant, "cs"));
    assert!(!s.meili.index_exists(&tmp).await.unwrap());

    // A request dispatched before that rebuild started is already served.
    assert_eq!(
        index::rebuild(&s.runtime, &s.meili, s.tenant, 1001, Some(1))
            .await
            .unwrap(),
        Rebuilt::Stale
    );
    // ...unless a locale appeared since: it was never rebuilt, so the request is not covered.
    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale, locales)
         VALUES ($1, 'at', 'Österreich', '{AT}', 'EUR', 'de', '{de}')",
    )
    .bind(s.tenant)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        index::rebuild(&s.runtime, &s.meili, s.tenant, 1003, Some(1))
            .await
            .unwrap(),
        Rebuilt::Done
    );

    // An interrupted rebuild left a registered, half-filled index behind: the next one
    // removes it and completes.
    let stuck = format!("{}__r999", index_uid(s.tenant, "cs"));
    s.meili.create_index(&stuck).await.unwrap();
    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    sqlx::query("UPDATE search_indexes SET building_uid = $1 WHERE locale = 'cs'")
        .bind(&stuck)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        index::rebuild(&s.runtime, &s.meili, s.tenant, 1002, None)
            .await
            .unwrap(),
        Rebuilt::Done
    );
    s.settle().await;
    assert!(!s.meili.index_exists(&stuck).await.unwrap());
    assert_eq!(
        ids(&s.search(s.cz, "cs", req(&[])).await),
        BTreeSet::from([a, b])
    );
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn zero_result_queries_are_counted_without_personal_data(db: PgPool) {
    let s = shop(&db).await;
    let one = [("size", "m")];
    let a = s
        .product(
            "ZA",
            "Tričko",
            "Tričko",
            &[V {
                options: &one,
                cz: Some(1),
                sk: None,
                stock: 1,
            }],
        )
        .await;
    s.index(a).await;
    s.settle().await;
    for q in ["Xyzzy Quux", "xyzzy quux"] {
        let r = s
            .search(
                s.cz,
                "cs",
                SearchRequest {
                    q: q.into(),
                    ..req(&[])
                },
            )
            .await;
        assert_eq!(r.total, 0);
    }
    // With a refinement the query itself is not the problem: not recorded.
    let mut refined = req(&[("opt.size", &["xl"])]);
    refined.q = "tričko".into();
    assert_eq!(s.search(s.cz, "cs", refined).await.total, 0);

    let mut tx = tenant_tx(&s.runtime, s.tenant).await.unwrap();
    let rows: Vec<(String, String, i32)> =
        sqlx::query_as("SELECT locale, query, count FROM search_zero_results")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rows, vec![("cs".to_owned(), "xyzzy quux".to_owned(), 2)]);
    // RLS: another tenant sees none of it.
    let other = testkit::tenant(&s.runtime, "zero-other").await.0;
    let mut tx = tenant_tx(&s.runtime, other).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM search_zero_results")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// The relevance fixtures (≥ 200 query → expected product pairs) against the real engine.
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "needs Meilisearch: make test-search"]
async fn cs_sk_query_fixtures(db: PgPool) {
    let s = shop(&db).await;
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/search/");
    let catalog = std::fs::read_to_string(format!("{root}cs-sk-catalog.tsv")).unwrap();
    let mut by_sku: HashMap<String, Uuid> = HashMap::new();
    for line in catalog
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        let (sku, cs, sk, brand, ean) = (f[0], f[1], f[2], f[3], f[4]);
        let mut input = testkit::catalog::product_input(sku, 1);
        input.brand = Some(brand.into());
        input.translations[0].name = cs.into();
        input.translations[1].locale = "sk".into();
        input.translations[1].name = sk.into();
        input.translations[1].slug = format!("{}-sk", sku.to_ascii_lowercase());
        input.variants[0].sku = sku.into();
        input.variants[0].ean = Some(ean.into());
        let product = testkit::catalog::create(&s.runtime, s.tenant, &input).await;
        let v = product.variants[0].id;
        testkit::pricing::set_prices(&s.runtime, s.tenant, s.cz_list, &[(v, 10_000)]).await;
        testkit::pricing::set_prices(&s.runtime, s.tenant, s.sk_list, &[(v, 400)]).await;
        by_sku.insert(sku.to_owned(), product.id);
    }
    assert_eq!(
        index::rebuild(&s.runtime, &s.meili, s.tenant, 1, None)
            .await
            .unwrap(),
        Rebuilt::Done
    );
    s.settle().await;

    let queries = std::fs::read_to_string(format!("{root}cs-sk-queries.tsv")).unwrap();
    let (mut total, mut failures) = (0, vec![]);
    for line in queries
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = line.split('\t').collect();
        let (locale, q, sku) = (f[0], f[1], f[2]);
        let market = if locale == "sk" { s.sk } else { s.cz };
        let r = s
            .search(
                market,
                locale,
                SearchRequest {
                    q: q.into(),
                    per_page: 5,
                    ..req(&[])
                },
            )
            .await;
        total += 1;
        if !r
            .items
            .iter()
            .any(|h| Some(&h.product_id) == by_sku.get(sku))
        {
            failures.push(format!(
                "{line}  -> got {:?}",
                r.items.iter().map(|h| &h.name).collect::<Vec<_>>()
            ));
        }
    }
    assert!(total >= 200, "only {total} fixtures");
    assert!(
        failures.is_empty(),
        "{} of {total} failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// RLS on the search tables (spec §5.2): no reads or writes across tenants. No Meilisearch.
#[sqlx::test(migrations = "../../migrations")]
async fn search_tables_are_tenant_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let (b, _) = testkit::tenant(&runtime, "beta").await;
    let inserts = [
        "INSERT INTO search_indexes (tenant_id, locale, settings_version) VALUES ($1, 'cs', 1)",
        "INSERT INTO search_product_state (tenant_id, product_id) VALUES ($1, gen_random_uuid())",
        "INSERT INTO search_zero_results (tenant_id, day, locale, query)
         VALUES ($1, current_date, 'cs', 'x')",
    ];
    for sql in inserts {
        // Own rows: fine. Another tenant's: rejected by the policy.
        let mut tx = tenant_tx(&runtime, b).await.unwrap();
        sqlx::query(sql).bind(b).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = tenant_tx(&runtime, a).await.unwrap();
        let err = sqlx::query(sql)
            .bind(b)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("row-level security"),
            "{sql}: {err}"
        );
    }
    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    for table in [
        "search_indexes",
        "search_product_state",
        "search_zero_results",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
        let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = tenant_id"
        )))
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(updated, 0, "{table}");
        let deleted = sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(deleted, 0, "{table}");
    }
    tx.commit().await.unwrap();
    // An own row cannot be handed to another tenant.
    for (sql, table) in inserts.iter().zip([
        "search_indexes",
        "search_product_state",
        "search_zero_results",
    ]) {
        let mut tx = tenant_tx(&runtime, a).await.unwrap();
        sqlx::query(*sql).bind(a).execute(&mut *tx).await.unwrap();
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = $1"
        )))
        .bind(b)
        .execute(&mut *tx)
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("row-level security"),
            "{table}: {err}"
        );
    }
}
