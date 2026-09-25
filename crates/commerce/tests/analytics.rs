//! Analytics (A20): counters without identifiers, consent-gated events, purchases, rollups,
//! dashboard numbers and tenant isolation.
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use commerce::analytics::{self, CounterBatch, DashboardQuery, PageCounter, SearchCounter};
use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject, new_anon_id};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::{Shop, raw_order, shop};

async fn consent(runtime: &PgPool, shop: &Shop, subject: &str, analytics: bool) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    consent::record(
        &mut tx,
        &Subject::Anon(subject.into()),
        &ConsentChoice {
            purposes: Purposes {
                analytics: Some(analytics),
                ads: Some(false),
                ..Purposes::default()
            },
            text_version: "v1".into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn beacon(shop: &Shop) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "events": [
            { "type": "page_view", "template": "product" },
            { "type": "view_item", "template": "product", "product_id": shop.product },
            { "type": "add_to_cart", "variant_id": shop.variants[0], "quantity": 2 },
            { "type": "web_vital", "template": "product", "name": "LCP", "value": 1800 },
            { "type": "web_vital", "template": "product", "name": "LCP", "value": 2600 },
            { "type": "identify", "email": "a@b.cz" }
        ],
        // Claimed purposes are ignored (A20).
        "purposes": ["analytics"]
    }))
    .unwrap()
}

async fn ingest(
    runtime: &PgPool,
    shop: &Shop,
    subject: Option<&str>,
    at: chrono::DateTime<Utc>,
) -> usize {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let n = analytics::ingest(&mut tx, shop.cz, subject, &beacon(shop), at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    n
}

async fn events(runtime: &PgPool, shop: &Shop) -> i64 {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    n
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_need_server_side_analytics_consent(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "an").await;
    let now = Utc::now();
    let (yes, no, unknown) = (new_anon_id(), new_anon_id(), new_anon_id());
    consent(&runtime, &shop, &yes, true).await;
    consent(&runtime, &shop, &no, false).await;

    assert_eq!(ingest(&runtime, &shop, None, now).await, 0);
    assert_eq!(ingest(&runtime, &shop, Some(&no), now).await, 0);
    assert_eq!(ingest(&runtime, &shop, Some(&unknown), now).await, 0);
    assert_eq!(ingest(&runtime, &shop, Some("not-a-subject"), now).await, 0);
    assert_eq!(events(&runtime, &shop).await, 0, "nothing without consent");

    assert_eq!(ingest(&runtime, &shop, Some(&yes), now).await, 5);
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let rows: Vec<(String, String, Vec<String>)> =
        sqlx::query_as("SELECT DISTINCT anon_id, session_id::text, consent_purposes FROM events")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    let added: serde_json::Value =
        sqlx::query_scalar("SELECT props FROM events WHERE type = 'add_to_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        added["product_id"],
        json!(shop.product),
        "the product comes from the catalog"
    );
    assert_eq!(rows.len(), 1, "one visitor, one session");
    assert_ne!(rows[0].0, yes, "the consent subject is never stored");
    assert_eq!(rows[0].2, ["analytics"]);

    // A withdrawal stops collection at once.
    consent(&runtime, &shop, &yes, false).await;
    assert_eq!(ingest(&runtime, &shop, Some(&yes), now).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sessions_split_after_thirty_idle_minutes(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let shop = shop(&runtime, "an").await;
    let subject = new_anon_id();
    consent(&runtime, &shop, &subject, true).await;
    let t0 = Utc::now() - Duration::hours(2);
    ingest(&runtime, &shop, Some(&subject), t0).await;
    ingest(&runtime, &shop, Some(&subject), t0 + Duration::minutes(20)).await;
    ingest(&runtime, &shop, Some(&subject), t0 + Duration::minutes(55)).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT count(DISTINCT session_id) FROM events")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(sessions, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn purchases_rollups_and_the_dashboard(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let shop = shop(&runtime, "an").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    let now = Utc::now();
    let today = now.date_naive();

    // Two consented visitors; one buys.
    let (buyer, browser) = (new_anon_id(), new_anon_id());
    for s in [&buyer, &browser] {
        consent(&runtime, &shop, s, true).await;
        ingest(&runtime, &shop, Some(s), now).await;
    }
    let paid = raw_order(&runtime, &shop, shop.cz, "CZK", 30_000, 2, "confirmed").await;
    raw_order(&runtime, &shop, shop.cz, "CZK", 10_000, 1, "pending").await;
    raw_order(&runtime, &shop, shop.cz, "CZK", 99_000, 9, "cancelled").await;
    raw_order(&runtime, &shop, shop.sk, "EUR", 1_000, 1, "confirmed").await;
    raw_order(&runtime, &other, other.cz, "CZK", 77_700, 7, "confirmed").await;

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    // The outbox handler and the checkout link write one row, in either order.
    assert!(
        analytics::link_purchase(&mut tx, paid, &buyer, now)
            .await
            .unwrap()
    );
    assert!(!analytics::record_purchase(&mut tx, paid).await.unwrap());
    assert!(
        !analytics::link_purchase(&mut tx, paid, &browser, now)
            .await
            .unwrap()
    );
    let purchases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE type = 'purchase' AND anon_id IS NOT NULL",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(purchases, 1);
    analytics::rollup(&mut tx, today).await.unwrap();
    analytics::rollup(&mut tx, today).await.unwrap(); // idempotent
    tx.commit().await.unwrap();

    analytics::record_counters(
        &runtime,
        &CounterBatch {
            counters: vec![
                PageCounter {
                    tenant_id: shop.tenant,
                    market_id: shop.cz,
                    day: today,
                    template: "product".into(),
                    requests: 40,
                },
                PageCounter {
                    tenant_id: shop.tenant,
                    market_id: shop.cz,
                    day: today,
                    template: "home".into(),
                    requests: 60,
                },
                // Another tenant's market under this tenant: skipped, not an error.
                PageCounter {
                    tenant_id: shop.tenant,
                    market_id: other.cz,
                    day: today,
                    template: "home".into(),
                    requests: 1_000,
                },
            ],
            searches: vec![
                SearchCounter {
                    tenant_id: shop.tenant,
                    day: today,
                    locale: "cs".into(),
                    query: "Modré TRIČKO".into(),
                    count: 3,
                },
                SearchCounter {
                    tenant_id: shop.tenant,
                    day: today,
                    locale: "cs".into(),
                    query: "jan.novak@example.com".into(),
                    count: 1,
                },
            ],
        },
    )
    .await
    .unwrap();

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let d = analytics::dashboard(
        &mut tx,
        &DashboardQuery {
            from: today - chrono::Days::new(6),
            to: today,
            market_id: Some(shop.cz),
        },
    )
    .await
    .unwrap();
    assert_eq!(d.sales.len(), 1);
    assert_eq!(
        (
            d.sales[0].currency.as_str(),
            d.sales[0].revenue_minor,
            d.sales[0].orders,
            d.sales[0].aov_minor
        ),
        ("CZK", 40_000, 2, 20_000)
    );
    assert_eq!(d.daily_traffic.len(), 7);
    assert_eq!(d.traffic.page_requests, 100);
    assert_eq!(d.traffic.consented_sessions, 2);
    assert_eq!(d.traffic.consented_page_views, 2);
    assert_eq!(d.traffic.conversion_rate, Some(0.5));
    let funnel: Vec<i64> = d.traffic.funnel.iter().map(|s| s.sessions).collect();
    assert_eq!(funnel, [2, 2, 2, 0, 1]);
    assert_eq!(d.top_products[0].units, 3);
    assert_eq!(
        d.top_searches.len(),
        1,
        "contact-like queries are not stored"
    );
    assert_eq!(
        (d.top_searches[0].query.as_str(), d.top_searches[0].count),
        ("modre tricko", 3)
    );
    let lcp = &d.web_vitals[0];
    assert_eq!(
        (lcp.template.as_str(), lcp.metric.as_str(), lcp.samples),
        ("product", "LCP", 4)
    );
    assert!((lcp.p75 - 2600.0).abs() < 1e-9, "{}", lcp.p75);

    // All markets: two currencies.
    let all = analytics::dashboard(
        &mut tx,
        &DashboardQuery {
            from: today,
            to: today,
            market_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(all.sales.len(), 2);
    assert!(
        analytics::dashboard(
            &mut tx,
            &DashboardQuery {
                from: today,
                to: today,
                market_id: Some(other.cz),
            },
        )
        .await
        .is_err(),
        "another tenant's market"
    );
    tx.commit().await.unwrap();

    // The other tenant sees none of it.
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    for table in [
        "events",
        "daily_metrics",
        "analytics_counters",
        "search_query_counts",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let o = analytics::dashboard(
        &mut tx,
        &DashboardQuery {
            from: today,
            to: today,
            market_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(o.sales[0].revenue_minor, 77_700);
    let written = sqlx::query(
        "INSERT INTO events (id, tenant_id, at, type) VALUES (gen_random_uuid(), $1, now(), 'page_view')",
    )
    .bind(shop.tenant)
    .execute(&mut *tx)
    .await;
    assert!(written.is_err(), "RLS refuses writes for another tenant");
}

#[sqlx::test(migrations = "../../migrations")]
async fn partitions_are_created_ahead_and_dropped_after_retention(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let created: i32 = sqlx::query_scalar("SELECT platform.ensure_event_partitions(3)")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(created, 1, "one more month ahead");
    // Keep 0 months: every partition that ended before now goes (last month's).
    let dropped: i32 = sqlx::query_scalar("SELECT platform.drop_event_partitions(0)")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(dropped, 1);
    let dropped: i32 = sqlx::query_scalar("SELECT platform.drop_event_partitions(13)")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(dropped, 0);
    // The runtime role cannot read a partition directly.
    let direct = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM events_{}",
        Utc::now().format("%Y_%m")
    )))
    .fetch_all(&runtime)
    .await;
    assert!(direct.is_err());
}
