//! Recommendations (spec §11.2, A20): rollups from orders and consented events, support
//! thresholds, decay, popularity debounce, the engine's fallback chain and visibility filter,
//! consent gating and tenant isolation.
#![allow(clippy::unwrap_used)]

use chrono::{DateTime, Duration, Utc};
use commerce::analytics;
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject, new_anon_id};
use commerce::inventory::{self, Adjustment};
use commerce::recommendations::engine::{self, SkipReason, Target, Visitor};
use commerce::recommendations::settings::{self, RecommendationSettings};
use commerce::recommendations::{Strategy, collections, rollup};
use commerce::storefront::{self, PublicUrls};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::catalog::{self, ACTOR};
use testkit::storefront::{Shop, shop};
use uuid::Uuid;

/// A sellable single-variant product (CZK + EUR prices, `stock` on hand); returns
/// (product, variant).
async fn sellable(runtime: &PgPool, shop: &Shop, sku: &str, stock: i32) -> (Uuid, Uuid) {
    let mut input = catalog::product_input(sku, 1);
    input.category_ids = vec![shop.category];
    let p = catalog::create(runtime, shop.tenant, &input).await;
    let v = p.variants[0].id;
    testkit::pricing::set_prices(runtime, shop.tenant, shop.czk, &[(v, 10_000)]).await;
    testkit::pricing::set_prices(runtime, shop.tenant, shop.eur, &[(v, 400)]).await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    inventory::adjust(
        &mut tx,
        ACTOR,
        v,
        &format!("init-{v}"),
        &Adjustment {
            delta: None,
            on_hand: Some(stock),
            note: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (p.id, v)
}

/// A placed CZ order with one unit of each product at `at`.
async fn order(
    runtime: &PgPool,
    shop: &Shop,
    lines: &[(Uuid, Uuid)],
    at: DateTime<Utc>,
    status: &str,
) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let cart: Uuid = sqlx::query_scalar(
        "INSERT INTO carts (tenant_id, market_id, locale, currency, status)
         VALUES ($1, $2, 'cs', 'CZK', 'converted') RETURNING id",
    )
    .bind(shop.tenant)
    .bind(shop.cz)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let number: i64 = sqlx::query_scalar("SELECT coalesce(max(number), 0) + 1 FROM orders")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let total = 10_000 * lines.len() as i64;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (tenant_id, number, market_id, cart_id, email, locale, currency, status,
                             payment_status, fulfillment_status, ship_to_country, vat_payer,
                             subtotal_minor, discount_minor, shipping_minor, payment_fee_minor,
                             tax_minor, rounding_minor, total_minor, vat_recap,
                             shipping_method_snapshot, payment_method, placed_at)
         VALUES ($1, $2, $3, $4, 'buyer@example.com', 'cs', 'CZK', $5, 'paid', 'unfulfilled',
                 'CZ', true, $6, 0, 0, 0, 0, 0, $6, '[]', '{}', 'cod', $7)
         RETURNING id",
    )
    .bind(shop.tenant)
    .bind(number)
    .bind(shop.cz)
    .bind(cart)
    .bind(status)
    .bind(total)
    .bind(at)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    for (i, (product, variant)) in lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO order_lines (tenant_id, order_id, position, variant_id, product_id, sku,
                                      name, quantity, unit_gross_minor, base_minor,
                                      discount_minor, total_minor, tax_rate, tax_minor, net_minor)
             VALUES ($1, $2, $3, $4, $5, 'X', 'X', 1, 10000, 10000, 0, 10000, '21', 0, 10000)",
        )
        .bind(shop.tenant)
        .bind(id)
        .bind(i as i32 + 1)
        .bind(variant)
        .bind(product)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn run(runtime: &PgPool, shop: &Shop, now: DateTime<Utc>) -> Vec<Uuid> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let changed = rollup::run(&mut tx, now, rollup::BACKFILL_DAYS)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    changed
}

async fn pairs(runtime: &PgPool, tenant: Uuid) -> Vec<(Uuid, Uuid, i32)> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let rows =
        sqlx::query_as("SELECT product_a, product_b, count_90d FROM co_purchases ORDER BY 1, 2")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    rows
}

async fn consent(runtime: &PgPool, tenant: Uuid, subject: Subject, purposes: Purposes) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    consent::record(
        &mut tx,
        &subject,
        &ConsentChoice {
            purposes,
            text_version: "v1".into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn recommend(
    runtime: &PgPool,
    shop: &Shop,
    market: Uuid,
    target: Target,
    visitor: &Visitor,
    limit: usize,
) -> engine::Explained {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap();
    let s = settings::get(&mut tx).await.unwrap();
    let out = engine::recommend(&mut tx, &ctx, &s, &target, visitor, limit)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    out
}

fn ids(e: &engine::Explained) -> Vec<Uuid> {
    e.items.iter().map(|i| i.product.id).collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn co_purchases_need_support_within_90_days_and_are_symmetric(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-pairs").await;
    let a = sellable(&runtime, &shop, "A", 10).await;
    let b = sellable(&runtime, &shop, "B", 10).await;
    let c = sellable(&runtime, &shop, "C", 10).await;
    let d = sellable(&runtime, &shop, "D", 10).await;
    let e = sellable(&runtime, &shop, "E", 10).await;
    let now = Utc::now();
    let day = Duration::days(1);
    for i in 0..3 {
        order(&runtime, &shop, &[a, b], now - day * (i + 1), "confirmed").await; // support 3
        order(
            &runtime,
            &shop,
            &[a, d],
            now - day * (i + 1),
            if i == 0 { "cancelled" } else { "confirmed" },
        )
        .await;
        // One of the three is outside the 90-day window.
        order(
            &runtime,
            &shop,
            &[a, e],
            now - day * if i == 0 { 100 } else { i + 1 },
            "confirmed",
        )
        .await;
    }
    for i in 0..2 {
        order(&runtime, &shop, &[a, c], now - day * (i + 1), "confirmed").await; // support 2
    }
    run(&runtime, &shop, now).await;
    assert_eq!(pairs(&runtime, shop.tenant).await, {
        let mut v = vec![(a.0, b.0, 3), (b.0, a.0, 3)];
        v.sort();
        v
    });

    // Stats: purchases and revenue from orders (cancelled ones left out).
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let (units, revenue): (i64, i64) = sqlx::query_as(
        "SELECT sum(purchases)::bigint, sum(revenue_minor)::bigint FROM product_stats_daily
         WHERE product_id = $1",
    )
    .bind(a.0)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(units, 10, "3 (b) + 2 (d, one cancelled) + 3 (e) + 2 (c)");
    assert_eq!(revenue, 100_000);

    // Rerunning converges on the same result.
    run(&runtime, &shop, now).await;
    assert_eq!(pairs(&runtime, shop.tenant).await.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn bestsellers_decay_and_popularity_is_debounced(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-decay").await;
    let old = sellable(&runtime, &shop, "OLD", 10).await;
    let fresh = sellable(&runtime, &shop, "FRESH", 10).await;
    let now = Utc::now();
    // 6 units 60 days ago (weight 0.05 each) against 3 units yesterday (~0.95 each).
    for _ in 0..6 {
        order(
            &runtime,
            &shop,
            &[old],
            now - Duration::days(60),
            "confirmed",
        )
        .await;
    }
    for _ in 0..3 {
        order(
            &runtime,
            &shop,
            &[fresh],
            now - Duration::days(1),
            "confirmed",
        )
        .await;
    }
    let changed = run(&runtime, &shop, now).await;
    assert!(changed.contains(&old.0) && changed.contains(&fresh.0));

    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Category(shop.category),
        &Visitor::default(),
        2,
    )
    .await;
    assert_eq!(ids(&got), [fresh.0, old.0]);
    assert_eq!(got.strategy, Some(Strategy::Bestsellers));
    assert!(got.items[0].score > got.items[1].score);

    // Nothing moved: nothing to reindex.
    assert!(run(&runtime, &shop, now).await.is_empty());
    // One more unit of `fresh` is under 10 % of its popularity: still nothing.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let before: i32 =
        sqlx::query_scalar("SELECT popularity FROM product_popularity WHERE product_id = $1")
            .bind(fresh.0)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert!(before > 20, "3 units ~ 28 points: {before}");
    // The search documents carry it (closes the WP7 placeholder).
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let docs = commerce::search::documents::load_products(&mut tx, &[fresh.0], now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(docs[0].popularity, before);
    order(
        &runtime,
        &shop,
        &[fresh],
        now - Duration::days(60),
        "confirmed",
    )
    .await;
    assert!(
        run(&runtime, &shop, now).await.is_empty(),
        "a tiny change is debounced"
    );
    for _ in 0..3 {
        order(&runtime, &shop, &[fresh], now, "confirmed").await;
    }
    assert_eq!(run(&runtime, &shop, now).await, [fresh.0]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn engine_filters_visibility_and_falls_back(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-filter").await;
    let p = sellable(&runtime, &shop, "P", 10).await;
    let sold_out = sellable(&runtime, &shop, "SOLDOUT", 0).await;
    let draft = sellable(&runtime, &shop, "DRAFT", 10).await;
    let excluded = sellable(&runtime, &shop, "EXCL", 10).await;
    let good = sellable(&runtime, &shop, "GOOD", 10).await;
    let other = sellable(&runtime, &shop, "OTHER", 10).await;
    let now = Utc::now();
    for partner in [sold_out, draft, excluded, good] {
        for _ in 0..3 {
            order(
                &runtime,
                &shop,
                &[p, partner],
                now - Duration::days(2),
                "confirmed",
            )
            .await;
        }
    }
    // `other` is the market's best seller, never bought with `p`.
    for _ in 0..8 {
        order(
            &runtime,
            &shop,
            &[other],
            now - Duration::days(1),
            "confirmed",
        )
        .await;
    }
    run(&runtime, &shop, now).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE products SET status = 'draft' WHERE id = $1")
        .bind(draft.0)
        .execute(&mut *tx)
        .await
        .unwrap();
    settings::put(
        &mut tx,
        "t",
        &RecommendationSettings {
            excluded_product_ids: vec![excluded.0],
            ..RecommendationSettings::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Product(p.0),
        &Visitor::default(),
        3,
    )
    .await;
    assert_eq!(got.items[0].product.id, good.0);
    assert_eq!(got.items[0].strategy, Strategy::BoughtTogether);
    assert_eq!(got.strategy, Some(Strategy::BoughtTogether));
    assert_eq!(got.items[1].product.id, other.0, "then bestsellers");
    let reason = |id: Uuid| {
        got.skipped
            .iter()
            .find(|s| s.product_id == id && s.strategy == Strategy::BoughtTogether)
            .map(|s| s.reason)
    };
    assert_eq!(reason(sold_out.0), Some(SkipReason::OutOfStock));
    assert_eq!(reason(draft.0), Some(SkipReason::NotSold));
    assert_eq!(reason(excluded.0), Some(SkipReason::Excluded));
    let all = ids(&got);
    assert!(!all.contains(&p.0), "never the current product");
    assert!(!all.contains(&sold_out.0) && !all.contains(&draft.0) && !all.contains(&excluded.0));
    let unique: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "no duplicates");

    // The cart's products are never recommended to it.
    let cart = Visitor {
        cart: vec![p.0, good.0],
        ..Visitor::default()
    };
    let got = recommend(&runtime, &shop, shop.cz, Target::Cart, &cart, 4).await;
    assert!(!ids(&got).contains(&p.0) && !ids(&got).contains(&good.0));
    assert_eq!(got.items[0].product.id, other.0);

    // A shop without sales still gets products: the newest ones.
    let empty = testkit::storefront::shop(&runtime, "reco-empty").await;
    let got = recommend(
        &runtime,
        &empty,
        empty.cz,
        Target::Home,
        &Visitor::default(),
        4,
    )
    .await;
    assert_eq!(ids(&got), [empty.product]);
    assert_eq!(got.strategy, Some(Strategy::Newest));
}

#[sqlx::test(migrations = "../../migrations")]
async fn personalization_uses_only_consented_signals(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-personal").await;
    let viewed = sellable(&runtime, &shop, "VIEWED", 10).await;
    let now = Utc::now();
    let beacon = |product: Uuid| {
        serde_json::to_vec(&json!({ "events": [
            { "type": "view_item", "template": "product", "product_id": product },
            { "type": "view_item", "template": "product", "product_id": product },
        ]}))
        .unwrap()
    };
    let visit = async |subject: &str| {
        let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
        analytics::ingest(&mut tx, shop.cz, Some(subject), &beacon(viewed.0), now)
            .await
            .unwrap();
        let anon = analytics::anon_id(shop.tenant, subject);
        let affinity = engine::anon_affinity(&mut tx, &anon, now).await.unwrap();
        tx.commit().await.unwrap();
        affinity
    };

    // Analytics only: events are stored, but they may not personalize (A20).
    let analytics_only = new_anon_id();
    consent(
        &runtime,
        shop.tenant,
        Subject::Anon(analytics_only.clone()),
        Purposes {
            analytics: Some(true),
            personalization: Some(false),
            ..Purposes::default()
        },
    )
    .await;
    assert!(visit(&analytics_only).await.is_empty());

    let both = new_anon_id();
    consent(
        &runtime,
        shop.tenant,
        Subject::Anon(both.clone()),
        Purposes {
            analytics: Some(true),
            personalization: Some(true),
            ..Purposes::default()
        },
    )
    .await;
    let affinity = visit(&both).await;
    assert!(!affinity.is_empty());
    assert_eq!(affinity.seen, [viewed.0]);

    let visitor = Visitor {
        personalization: true,
        affinity: Some(affinity.clone()),
        ..Visitor::default()
    };
    let got = recommend(&runtime, &shop, shop.cz, Target::Home, &visitor, 2).await;
    assert_eq!(got.strategy, Some(Strategy::Personalized));
    // Same category/brand: the shop's other product first, the viewed one ranked lower.
    assert_eq!(ids(&got), [shop.product, viewed.0]);

    // Without the consent flag the same signals are ignored.
    let not_granted = Visitor {
        personalization: false,
        affinity: Some(affinity),
        ..Visitor::default()
    };
    let got = recommend(&runtime, &shop, shop.cz, Target::Home, &not_granted, 2).await;
    assert_ne!(got.strategy, Some(Strategy::Personalized));
    let recent = Target::Recent(vec![viewed.0, Uuid::now_v7()]);
    assert!(
        recommend(&runtime, &shop, shop.cz, recent.clone(), &not_granted, 4)
            .await
            .items
            .is_empty()
    );
    let granted = Visitor {
        personalization: true,
        ..Visitor::default()
    };
    let got = recommend(&runtime, &shop, shop.cz, recent, &granted, 4).await;
    assert_eq!(ids(&got), [viewed.0], "unknown ids are dropped");
    assert_eq!(got.skipped[0].reason, SkipReason::NotSold);
}

#[sqlx::test(migrations = "../../migrations")]
async fn customer_affinity_follows_consent(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-customer").await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let customer: Uuid = sqlx::query_scalar(
        "INSERT INTO customers (tenant_id, email, locale) VALUES ($1, 'c@example.com', 'cs') RETURNING id",
    )
    .bind(shop.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    order(
        &runtime,
        &shop,
        &[(shop.product, shop.variants[0])],
        Utc::now(),
        "confirmed",
    )
    .await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET customer_id = $1")
        .bind(customer)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let affinity = async || {
        run(&runtime, &shop, Utc::now()).await;
        let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
        let a = engine::customer_affinity(&mut tx, customer).await.unwrap();
        tx.commit().await.unwrap();
        a
    };
    assert!(affinity().await.is_empty(), "no consent, no affinity");
    let grant = |on| Purposes {
        personalization: Some(on),
        ..Purposes::default()
    };
    consent(
        &runtime,
        shop.tenant,
        Subject::Customer(customer),
        grant(true),
    )
    .await;
    let a = affinity().await;
    assert_eq!(a.scores.len(), 2, "category + brand: {a:?}");
    consent(
        &runtime,
        shop.tenant,
        Subject::Customer(customer),
        grant(false),
    )
    .await;
    assert!(
        affinity().await.is_empty(),
        "withdrawn consent drops the rows"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn collections_schedule_and_seasonal(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-coll").await;
    let x = sellable(&runtime, &shop, "X", 10).await;
    let now = Utc::now();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let input = |kind, starts: Option<DateTime<Utc>>, ends: Option<DateTime<Utc>>| {
        collections::CollectionInput {
            name: "Winter".into(),
            title_i18n: [("cs".to_owned(), "Zimní tipy".to_owned())].into(),
            kind,
            starts_at: starts,
            ends_at: ends,
            product_ids: vec![x.0, shop.product],
        }
    };
    let future = collections::create(
        &mut tx,
        "t",
        &input(
            collections::CollectionKind::Seasonal,
            Some(now + Duration::days(5)),
            Some(now + Duration::days(9)),
        ),
    )
    .await
    .unwrap();
    assert!(!future.active);
    let unknown = collections::CollectionInput {
        product_ids: vec![Uuid::now_v7()],
        ..input(collections::CollectionKind::Manual, None, None)
    };
    assert!(collections::create(&mut tx, "t", &unknown).await.is_err());
    tx.commit().await.unwrap();

    // A seasonal collection outside its window is not on the home page.
    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Home,
        &Visitor::default(),
        4,
    )
    .await;
    assert_ne!(got.strategy, Some(Strategy::Seasonal));

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let open = collections::update(
        &mut tx,
        "t",
        future.id,
        &input(
            collections::CollectionKind::Seasonal,
            Some(now - Duration::days(1)),
            Some(now + Duration::days(9)),
        ),
    )
    .await
    .unwrap();
    assert!(open.active);
    tx.commit().await.unwrap();
    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Home,
        &Visitor::default(),
        4,
    )
    .await;
    assert_eq!(got.strategy, Some(Strategy::Seasonal));
    assert_eq!(got.title.as_deref(), Some("Zimní tipy"));
    assert_eq!(ids(&got), [x.0, shop.product]);

    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Collection(open.id),
        &Visitor::default(),
        1,
    )
    .await;
    assert_eq!(got.strategy, Some(Strategy::Collection));
    assert_eq!(ids(&got), [x.0]);

    // A switched-off strategy is skipped.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    settings::put(
        &mut tx,
        "t",
        &RecommendationSettings {
            seasonal: false,
            ..RecommendationSettings::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Home,
        &Visitor::default(),
        4,
    )
    .await;
    assert!(!got.chain.contains(&Strategy::Seasonal));
}

#[sqlx::test(migrations = "../../migrations")]
async fn recommendation_tables_are_tenant_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let a = shop(&runtime, "reco-iso-a").await;
    let b = shop(&runtime, "reco-iso-b").await;
    let x = sellable(&runtime, &a, "ISO", 10).await;
    let now = Utc::now();
    for _ in 0..3 {
        order(
            &runtime,
            &a,
            &[(a.product, a.variants[0]), x],
            now,
            "confirmed",
        )
        .await;
    }
    run(&runtime, &a, now).await;
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    collections::create(
        &mut tx,
        "t",
        &collections::CollectionInput {
            name: "A only".into(),
            title_i18n: Default::default(),
            kind: collections::CollectionKind::Manual,
            starts_at: None,
            ends_at: None,
            product_ids: vec![x.0],
        },
    )
    .await
    .unwrap();
    settings::put(&mut tx, "t", &RecommendationSettings::default())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tables = [
        "product_stats_daily",
        "co_purchases",
        "product_scores",
        "product_popularity",
        "collections",
        "recommendation_settings",
        "customer_affinity",
    ];
    for t in tables {
        let count = async |tenant| {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let n: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {t}")))
                    .fetch_one(&mut *tx)
                    .await
                    .unwrap();
            tx.commit().await.unwrap();
            n
        };
        if t != "customer_affinity" {
            assert!(count(a.tenant).await > 0, "{t} filled for A");
        }
        assert_eq!(count(b.tenant).await, 0, "{t} leaks into B");
    }
    // B's rollup and engine never see A's pairs.
    run(&runtime, &b, now).await;
    assert!(pairs(&runtime, b.tenant).await.is_empty());
    let got = recommend(
        &runtime,
        &b,
        b.cz,
        Target::Product(b.product),
        &Visitor::default(),
        4,
    )
    .await;
    assert!(!ids(&got).contains(&x.0));
    // Writing a row for another tenant is refused.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let denied =
        sqlx::query("INSERT INTO collections (tenant_id, name, kind) VALUES ($1, 'x', 'manual')")
            .bind(a.tenant)
            .execute(&mut *tx)
            .await;
    assert!(denied.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn last_years_month_fills_in_after_current_bestsellers(db: PgPool) {
    use chrono::Datelike;
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "reco-season").await;
    let now_seller = sellable(&runtime, &shop, "NOW", 10).await;
    let last_year = sellable(&runtime, &shop, "LASTYEAR", 10).await;
    let now = Utc::now();
    order(
        &runtime,
        &shop,
        &[now_seller],
        now - Duration::days(1),
        "confirmed",
    )
    .await;
    // The 15th of this month, a year ago.
    let then = chrono::NaiveDate::from_ymd_opt(now.year() - 1, now.month(), 15)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc();
    order(&runtime, &shop, &[last_year], then, "confirmed").await;
    run(&runtime, &shop, now).await;

    let got = recommend(
        &runtime,
        &shop,
        shop.cz,
        Target::Home,
        &Visitor::default(),
        3,
    )
    .await;
    assert_eq!(ids(&got), [now_seller.0, last_year.0, shop.product]);
    let strategies: Vec<Strategy> = got.items.iter().map(|i| i.strategy).collect();
    assert_eq!(
        strategies,
        [Strategy::Bestsellers, Strategy::Seasonal, Strategy::Newest],
        "this year's best sellers first, last year's month next"
    );
}
