use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Days, NaiveDate, Utc};
use commerce::ai::settings::{self, AiSettingsChange};
use commerce::ai::{Ai, Outcome};
use commerce::insights::detect::{Detection, Pair, Transition, Vital};
use commerce::insights::{self, Insight, Measure, Reopened, Status, ThemeChange, Window};
use commerce::themes::{self, RevisionSummary};
use platform::ai::{Client, Fake};
use platform::db::tenant_tx;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const SESSION: &str = "0197a8a0-0000-7000-8000-00000000beef";
const ANON: &str = "0123456789abcdef0123456789abcdef";

fn yesterday() -> NaiveDate {
    Utc::now().date_naive() - Days::new(1)
}

async fn setup(db: &PgPool, slug: &str) -> (PgPool, Shop) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let shop = testkit::storefront::shop(&runtime, slug).await;
    (runtime, shop)
}

async fn metric(
    runtime: &PgPool,
    shop: &Shop,
    date: NaiveDate,
    market: Uuid,
    rows: &[(&str, f64)],
) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    for (name, value) in rows {
        sqlx::query(
            "INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
             VALUES ($1, $2, $3, $4, '{}', $5)",
        )
        .bind(shop.tenant)
        .bind(date)
        .bind(market)
        .bind(name)
        .bind(value)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn product_metrics(
    runtime: &PgPool,
    shop: &Shop,
    date: NaiveDate,
    product: Uuid,
    views: f64,
    adds: f64,
) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    for (name, value) in [("product.views", views), ("product.add_to_carts", adds)] {
        sqlx::query(
            "INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
             VALUES ($1, $2, $3, $4, jsonb_build_object('product_id', $5::text), $6)",
        )
        .bind(shop.tenant)
        .bind(date)
        .bind(shop.cz)
        .bind(name)
        .bind(product.to_string())
        .bind(value)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn vitals(
    runtime: &PgPool,
    shop: &Shop,
    day: NaiveDate,
    (template, name, value, n, requests): (&str, &str, f64, i32, i64),
) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO events (id, tenant_id, at, type, anon_id, session_id, market_id, props)
         SELECT gen_random_uuid(), $1, $2 + time '12:00', 'web_vitals', $3, $4::uuid, $5,
                jsonb_build_object('template', $6::text, 'name', $7::text, 'value', $8::float8)
         FROM generate_series(1, $9)",
    )
    .bind(shop.tenant)
    .bind(day)
    .bind(ANON)
    .bind(SESSION)
    .bind(shop.cz)
    .bind(template)
    .bind(name)
    .bind(value)
    .bind(n)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO analytics_counters (tenant_id, market_id, day, template, requests)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(shop.tenant)
    .bind(shop.cz)
    .bind(day)
    .bind(template)
    .bind(requests)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn funnel(v: [f64; 5]) -> [(&'static str, f64); 5] {
    [
        ("sessions", v[0]),
        ("funnel.view_item", v[1]),
        ("funnel.add_to_cart", v[2]),
        ("funnel.begin_checkout", v[3]),
        ("funnel.purchase", v[4]),
    ]
}

async fn fixture(runtime: &PgPool, shop: &Shop, day: NaiveDate, home: bool) -> Uuid {
    let recent = day - Days::new(1);
    metric(
        runtime,
        shop,
        recent,
        shop.cz,
        &funnel([1000.0, 500.0, 60.0, 50.0, 20.0]),
    )
    .await;
    let earlier = day - Days::new(10);
    metric(
        runtime,
        shop,
        earlier,
        shop.cz,
        &funnel([2000.0, 1000.0, 200.0, 100.0, 40.0]),
    )
    .await;
    metric(
        runtime,
        shop,
        earlier,
        shop.sk,
        &funnel([1000.0, 500.0, 100.0, 50.0, 20.0]),
    )
    .await;
    metric(
        runtime,
        shop,
        day - Days::new(28),
        shop.cz,
        &funnel([90_000.0, 90_000.0, 0.0, 0.0, 0.0]),
    )
    .await;

    let other = testkit::catalog::create(
        runtime,
        shop.tenant,
        &testkit::catalog::product_input("OTHER", 1),
    )
    .await
    .id;
    product_metrics(runtime, shop, recent, shop.product, 400.0, 8.0).await;
    product_metrics(runtime, shop, day - Days::new(3), other, 600.0, 60.0).await;
    product_metrics(runtime, shop, recent, Uuid::now_v7(), 5000.0, 0.0).await;

    vitals(runtime, shop, day, ("product", "LCP", 4000.0, 60, 20_000)).await;
    if home {
        vitals(runtime, shop, day, ("home", "INP", 260.0, 60, 4000)).await;
    }
    other
}

async fn run(runtime: &PgPool, ai: &Ai, shop: &Shop, day: NaiveDate) {
    assert_eq!(
        insights::run(runtime, ai, shop.tenant, day, false)
            .await
            .unwrap(),
        Outcome::Done
    );
}

async fn list(runtime: &PgPool, tenant: Uuid) -> Vec<Insight> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    insights::list(&mut tx, Utc::now().date_naive())
        .await
        .unwrap()
}

type Ranked = (String, String, f64, Option<String>, NaiveDate, NaiveDate);

async fn ranked(runtime: &PgPool, tenant: Uuid) -> Vec<Ranked> {
    list(runtime, tenant)
        .await
        .into_iter()
        .map(|i| {
            (
                i.detection.kind().to_owned(),
                i.detection.subject(),
                (i.score * 100.0).round() / 100.0,
                i.product_name,
                i.first_detected_on,
                i.last_detected_on,
            )
        })
        .collect()
}

fn row(
    kind: &str,
    subject: &str,
    score: f64,
    name: Option<&str>,
    first: NaiveDate,
    last: NaiveDate,
) -> Ranked {
    (
        kind.into(),
        subject.into(),
        score,
        name.map(Into::into),
        first,
        last,
    )
}

async fn switch(runtime: &PgPool, shop: &Shop, insights: bool) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    settings::put(
        &mut tx,
        "owner",
        &AiSettingsChange {
            insights: Some(insights),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn usage(runtime: &PgPool, shop: &Shop) -> i64 {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE feature = 'insight_hypothesis'")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn detects_ranks_and_lists_signals_idempotently(db: PgPool) {
    let (runtime, shop) = setup(&db, "ins").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, false).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    let tee = shop.product.to_string();
    let expected = vec![
        row("slow_page", "product:LCP", 300.0, None, day, day),
        row(
            "funnel_drop",
            "view_item_to_add_to_cart",
            40.0,
            None,
            day,
            day,
        ),
        row("low_add_to_cart", &tee, 9.6, Some("Product TEE"), day, day),
    ];
    assert_eq!(ranked(&runtime, shop.tenant).await, expected);
    let first = list(&runtime, shop.tenant).await;
    assert_eq!(
        first[1].detection,
        Detection::FunnelDrop {
            transition: Transition::ViewItemToAddToCart,
            recent: Pair { from: 500, to: 60 },
            baseline: Pair {
                from: 1500,
                to: 300
            },
        }
    );
    assert_eq!(
        first[2].detection,
        Detection::LowAddToCart {
            product_id: shop.product,
            views: 400,
            adds: 8,
            shop_views: 1000,
            shop_adds: 68,
        }
    );
    assert!(first.iter().all(|i| i.hypothesis.is_none()));

    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(
        list(&runtime, shop.tenant).await,
        first,
        "a rerun changes nothing"
    );
    assert_eq!(usage(&runtime, &shop).await, 0, "the switch is off");
}

async fn scores(runtime: &PgPool, tenant: Uuid) -> Vec<(String, NaiveDate, f64)> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_as(
        "SELECT s.subject, h.day, h.score FROM insight_scores h
         JOIN insight_signals s ON s.id = h.signal_id ORDER BY s.subject, h.day",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_run_keeps_the_days_score_and_a_rerun_overwrites_it(db: PgPool) {
    let (runtime, shop) = setup(&db, "hist").await;
    let day = yesterday();
    let before = day - Days::new(1);
    vitals(&runtime, &shop, before, ("home", "LCP", 5000.0, 60, 2000)).await;
    run(&runtime, &Ai::fake(), &shop, before).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(
        scores(&runtime, shop.tenant).await,
        vec![
            ("home:LCP".into(), before, 50.0),
            ("home:LCP".into(), day, 50.0)
        ]
    );

    vitals(&runtime, &shop, day, ("home", "LCP", 5000.0, 60, 2000)).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    run(&runtime, &Ai::fake(), &shop, before).await;
    assert_eq!(
        scores(&runtime, shop.tenant).await,
        vec![
            ("home:LCP".into(), before, 50.0),
            ("home:LCP".into(), day, 100.0)
        ]
    );
    assert_eq!(
        ranked(&runtime, shop.tenant).await,
        vec![row("slow_page", "home:LCP", 100.0, None, before, day)],
        "a late run of an older day keeps the newer score"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_run_rolls_up_its_day_before_detecting(db: PgPool) {
    let (runtime, shop) = setup(&db, "roll").await;
    let day = yesterday();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO events (id, tenant_id, at, type, market_id)
         SELECT gen_random_uuid(), $1, $2 + time '12:00', 'page_view', $3
         FROM generate_series(1, 7)",
    )
    .bind(shop.tenant)
    .bind(day)
    .bind(shop.cz)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &Ai::fake(), &shop, day).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let views: Option<f64> = sqlx::query_scalar(
        "SELECT sum(value) FROM daily_metrics WHERE date = $1 AND metric = 'page_views'",
    )
    .bind(day)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(views, Some(7.0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn dismissed_stays_dismissed_and_gone_signals_resolve_and_reopen(db: PgPool) {
    let (runtime, shop) = setup(&db, "life").await;
    let day = yesterday();
    let (next, after) = (day + Days::new(1), day + Days::new(2));
    let other = fixture(&runtime, &shop, day, false).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    let slow = list(&runtime, shop.tenant).await[0].id;

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let dismissed = insights::dismiss(&mut tx, "clerk", slow, day)
        .await
        .unwrap();
    let again = insights::dismiss(&mut tx, "clerk", slow, day)
        .await
        .unwrap();
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'insight.dismissed' AND entity_id = $1",
    )
    .bind(slow.to_string())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((dismissed.id, &dismissed, audits), (slow, &again, 1));

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("DELETE FROM daily_metrics WHERE metric LIKE 'product.%'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &Ai::fake(), &shop, next).await;
    assert_eq!(
        ranked(&runtime, shop.tenant).await,
        vec![row(
            "funnel_drop",
            "view_item_to_add_to_cart",
            40.0,
            None,
            day,
            next
        )]
    );

    let recent = day - Days::new(1);
    product_metrics(&runtime, &shop, recent, shop.product, 400.0, 8.0).await;
    product_metrics(&runtime, &shop, recent, other, 600.0, 60.0).await;
    run(&runtime, &Ai::fake(), &shop, after).await;
    let tee = shop.product.to_string();
    let reopened = vec![
        row(
            "funnel_drop",
            "view_item_to_add_to_cart",
            40.0,
            None,
            day,
            after,
        ),
        row(
            "low_add_to_cart",
            &tee,
            9.6,
            Some("Product TEE"),
            day,
            after,
        ),
    ];
    assert_eq!(ranked(&runtime, shop.tenant).await, reopened);

    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(ranked(&runtime, shop.tenant).await, reopened);
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let status: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, status FROM insight_signals ORDER BY kind")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        status,
        vec![
            ("funnel_drop".into(), "open".into()),
            ("low_add_to_cart".into(), "open".into()),
            ("slow_page".into(), "dismissed".into()),
        ]
    );
}

async fn reopens(runtime: &PgPool, shop: &Shop, id: Uuid) -> Vec<String> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query_scalar(
        "SELECT actor FROM audit_log WHERE action = 'insight.reopened' AND entity_id = $1",
    )
    .bind(id.to_string())
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

async fn dismiss(runtime: &PgPool, shop: &Shop, id: Uuid) -> Insight {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let dismissed = insights::dismiss(&mut tx, "clerk", id, Utc::now().date_naive())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    dismissed
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_dismissed_signal_reopens_at_one_and_a_half_times_its_dismissal_score(db: PgPool) {
    let (runtime, shop) = setup(&db, "reopen").await;
    let d = |n: u64| yesterday() - Days::new(6) + Days::new(n);
    let requests = async |n: u64, requests: i64| {
        vitals(&runtime, &shop, d(n), ("home", "LCP", 5000.0, 60, requests)).await;
        run(&runtime, &Ai::fake(), &shop, d(n)).await;
    };
    let listed = async || -> Vec<(Uuid, f64, Option<Reopened>)> {
        list(&runtime, shop.tenant)
            .await
            .into_iter()
            .map(|i| (i.id, i.score, i.reopened))
            .collect()
    };

    requests(0, 2000).await;
    let id = list(&runtime, shop.tenant).await[0].id;
    dismiss(&runtime, &shop, id).await;
    requests(1, 999).await;
    assert_eq!(listed().await, vec![], "74.975 is below 1.5 times 50");

    requests(2, 1).await;
    let first = Some(Reopened {
        on: d(2),
        dismissed_score: 50.0,
    });
    assert_eq!(listed().await, vec![(id, 75.0, first)]);
    assert_eq!(reopens(&runtime, &shop, id).await, vec!["system"]);

    run(&runtime, &Ai::fake(), &shop, d(2)).await;
    requests(3, 1000).await;
    assert_eq!(listed().await, vec![(id, 100.0, first)]);
    assert_eq!(reopens(&runtime, &shop, id).await, vec!["system"]);

    let again = dismiss(&runtime, &shop, id).await;
    assert_eq!((again.status, again.reopened), (Status::Dismissed, None));
    requests(4, 1999).await;
    assert_eq!(listed().await, vec![], "149.975 is below 1.5 times 100");
    requests(5, 1).await;
    let second = Some(Reopened {
        on: d(5),
        dismissed_score: 100.0,
    });
    assert_eq!(listed().await, vec![(id, 150.0, second)]);
    assert_eq!(reopens(&runtime, &shop, id).await, vec!["system", "system"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn another_tenant_cannot_list_or_dismiss_signals(db: PgPool) {
    let (runtime, a) = setup(&db, "ta").await;
    let b = testkit::storefront::shop(&runtime, "tb").await;
    let day = yesterday();
    fixture(&runtime, &a, day, false).await;
    run(&runtime, &Ai::fake(), &a, day).await;
    run(&runtime, &Ai::fake(), &b, day).await;
    let id = list(&runtime, a.tenant).await[0].id;
    assert_eq!(list(&runtime, b.tenant).await, vec![]);
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let err = insights::dismiss(&mut tx, "intruder", id, day)
        .await
        .unwrap_err();
    assert!(matches!(err, platform::Error::NotFound), "{err:?}");
    drop(tx);
    assert_eq!(list(&runtime, a.tenant).await.len(), 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_call_while_the_switch_is_off_or_without_a_provider(db: PgPool) {
    let (runtime, shop) = setup(&db, "nocall").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, false).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    switch(&runtime, &shop, true).await;
    run(&runtime, &Ai::fake_with_client(None), &shop, day).await;
    assert_eq!(usage(&runtime, &shop).await, 0);
    assert!(
        list(&runtime, shop.tenant)
            .await
            .iter()
            .all(|i| i.hypothesis.is_none())
    );
}

type Generated = (String, String, String);

#[sqlx::test(migrations = "../../migrations")]
async fn the_top_three_get_one_fake_hypothesis_per_day(db: PgPool) {
    let (runtime, shop) = setup(&db, "hyp").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, true).await;
    switch(&runtime, &shop, true).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    let got: Vec<(String, Option<Generated>)> = list(&runtime, shop.tenant)
        .await
        .into_iter()
        .map(|i| {
            (
                i.detection.subject(),
                i.hypothesis.map(|h| (h.text, h.theme_prompt, h.model)),
            )
        })
        .collect();
    let fake = |h: &str, p: &str| Some((h.to_owned(), p.to_owned(), "fake".to_owned()));
    assert_eq!(
        got,
        vec![
            (
                "product:LCP".into(),
                fake(
                    "Demo AI: the product pages are slow, LCP p75 4000.0 against a good bound of 2500.0.",
                    "Make the product template lighter with smaller images and fewer blocks above the fold."
                )
            ),
            (
                "view_item_to_add_to_cart".into(),
                fake(
                    "Demo AI: fewer sessions continue at view_item_to_add_to_cart, rate 0.12 against 0.2 before.",
                    "Make the next step after view_item_to_add_to_cart easier to find and more inviting on the storefront."
                )
            ),
            (
                "home:INP".into(),
                fake(
                    "Demo AI: the home pages are slow, INP p75 260.0 against a good bound of 200.0.",
                    "Make the home template lighter with smaller images and fewer blocks above the fold."
                )
            ),
            (shop.product.to_string(), None),
        ]
    );
    assert_eq!(
        usage(&runtime, &shop).await,
        3,
        "three calls, none on the second run"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn calls_whose_results_were_lost_still_count_toward_the_daily_cap(db: PgPool) {
    let (runtime, shop) = setup(&db, "lost").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, true).await;
    switch(&runtime, &shop, true).await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "UPDATE insight_signals SET hypothesis = NULL, theme_prompt = NULL,
                hypothesis_model = NULL, hypothesis_at = NULL, hypothesis_score = NULL",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(usage(&runtime, &shop).await, 3);
    assert!(
        list(&runtime, shop.tenant)
            .await
            .iter()
            .all(|i| i.hypothesis.is_none())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_run_while_another_holds_the_tenant_does_nothing(db: PgPool) {
    let (runtime, shop) = setup(&db, "busy").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, true).await;
    switch(&runtime, &shop, true).await;
    let mut other = db.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('insights:' || $1::text, 0))")
        .bind(shop.tenant.to_string())
        .execute(&mut *other)
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        insights::run(&runtime, &Ai::fake(), shop.tenant, day, false),
    )
    .await
    .expect("the run must not wait for the other one")
    .unwrap();
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        (
            usage(&runtime, &shop).await,
            list(&runtime, shop.tenant).await
        ),
        (0, vec![])
    );
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended('insights:' || $1::text, 0))")
        .bind(shop.tenant.to_string())
        .execute(&mut *other)
        .await
        .unwrap();
    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(usage(&runtime, &shop).await, 3);
}

async fn move_usage_back_a_day(db: &PgPool) {
    let mut owner = db.begin().await.unwrap();
    for sql in [
        "ALTER TABLE ai_usage NO FORCE ROW LEVEL SECURITY",
        "UPDATE ai_usage SET at = at - interval '1 day'",
        "ALTER TABLE ai_usage FORCE ROW LEVEL SECURITY",
    ] {
        sqlx::query(sql).execute(&mut *owner).await.unwrap();
    }
    owner.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejected_output_is_recorded_and_never_retried(db: PgPool) {
    let (runtime, shop) = setup(&db, "bad").await;
    let day = yesterday();
    fixture(&runtime, &shop, day, true).await;
    switch(&runtime, &shop, true).await;
    let fake = Fake::new([(
        "insight_hypothesis",
        r#"{"hypothesis": "See https://evil.example", "theme_prompt": "Make the add to cart button larger."}"#,
    )])
    .unwrap();
    let scripted = Ai::fake_with_client(Some(Client::Fake(Arc::new(fake))));
    run(&runtime, &scripted, &shop, day).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE insight_signals SET hypothesis_at = hypothesis_at - interval '1 day'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    move_usage_back_a_day(&db).await;
    run(&runtime, &Ai::fake(), &shop, day + Days::new(1)).await;

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let rows: Vec<(String, Option<String>, bool)> = sqlx::query_as(
        "SELECT subject, hypothesis_error, hypothesis IS NOT NULL FROM insight_signals
         ORDER BY score DESC",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    let invalid = || Some("ai_invalid_output".to_owned());
    assert_eq!(
        rows,
        vec![
            ("product:LCP".into(), invalid(), false),
            ("view_item_to_add_to_cart".into(), invalid(), false),
            ("home:INP".into(), invalid(), false),
            (shop.product.to_string(), None, true),
        ]
    );
    assert_eq!(
        usage(&runtime, &shop).await,
        4,
        "rejected calls are metered"
    );
}

fn searched(query: &str, times: i64) -> String {
    format!("Demo AI: shoppers searched for {query} {times} times in 14 days and found nothing.")
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_hypothesis_is_rewritten_when_the_score_moves_by_half_either_way(db: PgPool) {
    let (runtime, shop) = setup(&db, "stale").await;
    switch(&runtime, &shop, true).await;
    let d = |n: u64| yesterday() - Days::new(14) + Days::new(n);
    let searches = async |n: u64, count: i32| {
        zero_results(&runtime, &shop, &[(d(n), "cs", "modre tricko", count)]).await;
        run(&runtime, &Ai::fake(), &shop, d(n)).await;
        let i = list(&runtime, shop.tenant).await.remove(0);
        (i.score, i.hypothesis.unwrap(), usage(&runtime, &shop).await)
    };

    let (score, first, calls) = searches(0, 20).await;
    assert_eq!(
        (score, first.text.as_str(), calls),
        (10.0, searched("modre tricko", 20).as_str(), 1)
    );

    let (score, kept, calls) = searches(1, 9).await;
    assert_eq!(
        (score, &kept, calls),
        (14.5, &first, 1),
        "14.5 is below 1.5 times 10"
    );

    let (score, up, calls) = searches(2, 1).await;
    assert_eq!(
        (score, up.text.as_str(), calls),
        (15.0, searched("modre tricko", 30).as_str(), 2)
    );
    assert!(up.generated_at > first.generated_at);

    let (score, down, calls) = searches(14, 10).await;
    assert_eq!(
        (score, down.text.as_str(), calls),
        (10.0, searched("modre tricko", 20).as_str(), 3),
        "10 is 15 divided by 1.5"
    );
    assert!(down.generated_at > up.generated_at);
}

#[sqlx::test(migrations = "../../migrations")]
async fn new_and_stale_signals_share_the_three_calls_by_score(db: PgPool) {
    let (runtime, shop) = setup(&db, "share").await;
    switch(&runtime, &shop, true).await;
    let day = yesterday();
    let before = day - Days::new(1);
    zero_results(
        &runtime,
        &shop,
        &[
            (before, "cs", "steady", 40),
            (before, "cs", "risen", 30),
            (before, "cs", "climbed", 20),
            (before, "cs", "waiting", 12),
        ],
    )
    .await;
    run(&runtime, &Ai::fake(), &shop, before).await;
    move_usage_back_a_day(&db).await;
    zero_results(
        &runtime,
        &shop,
        &[
            (day, "cs", "steady", 10),
            (day, "cs", "risen", 16),
            (day, "cs", "climbed", 12),
            (day, "cs", "new", 40),
        ],
    )
    .await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    run(&runtime, &Ai::fake(), &shop, day).await;

    let got: Vec<(String, f64, Option<String>)> = list(&runtime, shop.tenant)
        .await
        .into_iter()
        .map(|i| (i.detection.subject(), i.score, i.hypothesis.map(|h| h.text)))
        .collect();
    assert_eq!(
        got,
        vec![
            ("steady".into(), 25.0, Some(searched("steady", 40))),
            ("risen".into(), 23.0, Some(searched("risen", 46))),
            ("new".into(), 20.0, Some(searched("new", 40))),
            ("climbed".into(), 16.0, Some(searched("climbed", 32))),
            ("waiting".into(), 6.0, None),
        ]
    );
    assert_eq!(usage(&runtime, &shop).await, 6, "three calls on each day");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_rejected_regeneration_keeps_the_text_and_date(db: PgPool) {
    let (runtime, shop) = setup(&db, "keep").await;
    switch(&runtime, &shop, true).await;
    let day = yesterday();
    let before = day - Days::new(1);
    zero_results(&runtime, &shop, &[(before, "cs", "modre tricko", 20)]).await;
    run(&runtime, &Ai::fake(), &shop, before).await;
    let first = list(&runtime, shop.tenant).await.remove(0).hypothesis;
    assert!(first.is_some());

    zero_results(&runtime, &shop, &[(day, "cs", "modre tricko", 10)]).await;
    let fake = Fake::new([(
        "insight_hypothesis",
        r#"{"hypothesis": "See https://evil.example", "theme_prompt": "Make the search page more helpful."}"#,
    )])
    .unwrap();
    let scripted = Ai::fake_with_client(Some(Client::Fake(Arc::new(fake))));
    run(&runtime, &scripted, &shop, day).await;
    let shown = async || {
        let i = list(&runtime, shop.tenant).await.remove(0);
        (i.score, i.hypothesis, usage(&runtime, &shop).await)
    };
    assert_eq!(shown().await, (15.0, first.clone(), 2));

    run(&runtime, &Ai::fake(), &shop, day).await;
    assert_eq!(
        shown().await,
        (15.0, first, 2),
        "the rejected score is not retried"
    );
}

async fn stub_anthropic() -> (reqwest::Url, Arc<Mutex<Vec<Value>>>) {
    use axum::routing::post;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |axum::Json(body): axum::Json<Value>| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(body);
                let text = json!({ "hypothesis": "Nákupní tlačítko může být málo vidět.",
                                   "theme_prompt": "Zvětši tlačítko Do košíku na stránce produktu." });
                axum::Json(json!({
                    "model": "claude-sonnet-5", "stop_reason": "end_turn",
                    "content": [{ "type": "text", "text": text.to_string() }],
                    "usage": { "input_tokens": 100, "output_tokens": 20 }
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/").parse().unwrap(), seen)
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_model_sees_only_aggregates_in_the_shop_language(db: PgPool) {
    let (runtime, shop) = setup(&db, "agg").await;
    let day = yesterday();
    let other = fixture(&runtime, &shop, day, false).await;
    switch(&runtime, &shop, true).await;
    let (url, seen) = stub_anthropic().await;
    let client =
        platform::ai::Anthropic::new(&url, "sk-test".into(), Duration::from_secs(10), 0).unwrap();
    let ai = Ai::fake_with_client(Some(Client::Anthropic(Arc::new(client))));
    run(&runtime, &ai, &shop, day).await;

    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    let users: Vec<String> = bodies
        .iter()
        .map(|b| b["messages"][0]["content"].to_string())
        .collect();
    let all = users.join("\n");
    for secret in [
        shop.tenant.to_string(),
        shop.product.to_string(),
        other.to_string(),
        shop.cz.to_string(),
        SESSION.to_owned(),
        ANON.to_owned(),
    ] {
        assert!(!all.contains(&secret), "{secret} leaked");
    }
    let product_call = users
        .iter()
        .find(|u| u.contains("low_add_to_cart"))
        .unwrap();
    assert!(product_call.contains("Product TEE"), "{product_call}");
    assert!(
        users.iter().all(|u| u.contains(r#"\"locale\":\"cs\""#)),
        "{all}"
    );
    let hyp = list(&runtime, shop.tenant).await[2]
        .hypothesis
        .clone()
        .unwrap();
    let searching = testkit::storefront::shop(&runtime, "agg-search").await;
    switch(&runtime, &searching, true).await;
    zero_results(&runtime, &searching, &[(day, "sk", "modre tricko", 10)]).await;
    run(&runtime, &ai, &searching, day).await;
    let search_call = seen.lock().unwrap()[3]["messages"][0]["content"].to_string();
    for fact in [
        r#"\"query\":\"modre tricko\""#,
        r#"\"searches_without_results\":10"#,
    ] {
        assert!(search_call.contains(fact), "{search_call}");
    }
    assert_eq!(
        (
            hyp.text.as_str(),
            hyp.theme_prompt.as_str(),
            hyp.model.as_str()
        ),
        (
            "Nákupní tlačítko může být málo vidět.",
            "Zvětši tlačítko Do košíku na stránce produktu.",
            "claude-sonnet-5"
        )
    );
}

async fn signal(runtime: &PgPool, shop: &Shop, d: &Detection, status: &str) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO insight_signals (tenant_id, id, kind, subject, detection, score, status,
                                      first_detected_on, last_detected_on)
         VALUES ($1, $2, $3, $4, $5, 1, $6, current_date, current_date)",
    )
    .bind(shop.tenant)
    .bind(id)
    .bind(d.kind())
    .bind(d.subject())
    .bind(serde_json::to_value(d).unwrap())
    .bind(status)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn publish_from(runtime: &PgPool, shop: &Shop, insight: Option<Uuid>) -> RevisionSummary {
    let artifact = "a0000000000000000000000000000001";
    let (run, rev) = (Uuid::now_v7(), Uuid::now_v7());
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO platform.theme_artifacts (id, kind) VALUES ($1, 'theme')
         ON CONFLICT DO NOTHING",
    )
    .bind(artifact)
    .execute(&mut *tx)
    .await
    .unwrap();
    let base: Uuid = sqlx::query_scalar(
        "INSERT INTO theme_revisions (tenant_id, number, artifact_id, status, created_by)
         SELECT $1, coalesce(max(number), 0) + 1, $2, 'ready', 'owner' FROM theme_revisions
         RETURNING id",
    )
    .bind(shop.tenant)
    .bind(artifact)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO ai_theme_runs (tenant_id, id, base_revision_id, prompt, status, created_by,
                                    insight_id)
         VALUES ($1, $2, $3, 'Make it faster.', 'accepted', 'owner', $4)",
    )
    .bind(shop.tenant)
    .bind(run)
    .bind(base)
    .bind(insight)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO theme_revisions (tenant_id, id, number, artifact_id, origin, change,
                                      source_key, status, created_by, ai_run_id)
         SELECT $1, $2, max(number) + 1, $3, 'custom', 'ai', 'x', 'ready', 'owner', $4
         FROM theme_revisions",
    )
    .bind(shop.tenant)
    .bind(rev)
    .bind(artifact)
    .bind(run)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE ai_theme_runs SET revision_id = $2 WHERE id = $1")
        .bind(run)
        .bind(rev)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    republish(runtime, shop, rev).await
}

async fn republish(runtime: &PgPool, shop: &Shop, rev: Uuid) -> RevisionSummary {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let published = themes::publish(&mut tx, "owner", rev).await.unwrap();
    tx.commit().await.unwrap();
    published
}

async fn change(
    runtime: &PgPool,
    shop: &Shop,
    insight: Uuid,
    today: NaiveDate,
) -> Option<ThemeChange> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    insights::list(&mut tx, today)
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.id == insight)
        .unwrap()
        .theme_change
}

fn days(d: NaiveDate, n: i64) -> NaiveDate {
    d + chrono::Duration::days(n)
}

fn measure(value: Option<f64>, samples: i64) -> Option<Measure> {
    Some(Measure { value, samples })
}

fn readout(
    rev: &RevisionSummary,
    before: Option<Measure>,
    after: Option<Measure>,
) -> Option<ThemeChange> {
    let published_at = rev.published_at.unwrap();
    let d = published_at.date_naive();
    Some(ThemeChange {
        revision_id: rev.id,
        revision_number: rev.number,
        published_at,
        before: Window {
            from: days(d, -14),
            to: days(d, -1),
            measure: before,
        },
        after: Window {
            from: days(d, 1),
            to: days(d, 14),
            measure: after,
        },
    })
}

fn funnel_drop(transition: Transition) -> Detection {
    let pair = Pair { from: 500, to: 60 };
    Detection::FunnelDrop {
        transition,
        recent: pair,
        baseline: pair,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_theme_change_reads_out_the_full_weeks_around_its_publish_day(db: PgPool) {
    let (runtime, shop) = setup(&db, "win").await;
    let insight = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::ViewItemToAddToCart),
        "open",
    )
    .await;
    let rev = publish_from(&runtime, &shop, Some(insight)).await;
    let d = rev.published_at.unwrap().date_naive();
    for (day, market, v) in [
        (-15, shop.cz, [9000.0, 1000.0, 1000.0, 9000.0, 9000.0]),
        (-14, shop.cz, [5000.0, 100.0, 10.0, 3000.0, 1.0]),
        (-14, shop.sk, [5000.0, 300.0, 30.0, 3000.0, 1.0]),
        (-1, shop.cz, [5000.0, 400.0, 60.0, 3000.0, 1.0]),
        (0, shop.cz, [9000.0, 5000.0, 5000.0, 9000.0, 9000.0]),
        (1, shop.cz, [5000.0, 400.0, 40.0, 3000.0, 1.0]),
        (14, shop.sk, [5000.0, 100.0, 60.0, 3000.0, 1.0]),
        (15, shop.cz, [9000.0, 7000.0, 0.0, 9000.0, 9000.0]),
    ] {
        metric(&runtime, &shop, days(d, day), market, &funnel(v)).await;
    }

    let before = measure(Some(0.125), 800);
    assert_eq!(
        change(&runtime, &shop, insight, days(d, 14)).await,
        readout(&rev, before, None),
        "the after window collects until its last day is over"
    );
    assert_eq!(
        change(&runtime, &shop, insight, days(d, 15)).await,
        readout(&rev, before, measure(Some(0.2), 500))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_and_slow_page_insights_read_out_their_own_metric(db: PgPool) {
    let (runtime, shop) = setup(&db, "own").await;
    let product = signal(
        &runtime,
        &shop,
        &Detection::LowAddToCart {
            product_id: shop.product,
            views: 400,
            adds: 8,
            shop_views: 1000,
            shop_adds: 68,
        },
        "open",
    )
    .await;
    let rev = publish_from(&runtime, &shop, Some(product)).await;
    let d = rev.published_at.unwrap().date_naive();
    let unrelated = Uuid::now_v7();
    for (day, p, views, adds) in [
        (-15, shop.product, 1000.0, 0.0),
        (-14, shop.product, 100.0, 2.0),
        (-3, shop.product, 400.0, 8.0),
        (-3, unrelated, 600.0, 60.0),
        (2, unrelated, 300.0, 30.0),
    ] {
        product_metrics(&runtime, &shop, days(d, day), p, views, adds).await;
    }
    assert_eq!(
        change(&runtime, &shop, product, days(d, 15)).await,
        readout(&rev, measure(Some(0.02), 500), measure(None, 0))
    );

    let slow = signal(
        &runtime,
        &shop,
        &Detection::SlowPage {
            template: "product".into(),
            metric: Vital::Lcp,
            p75: 4000.0,
            samples: 60,
            requests: 20_000,
        },
        "open",
    )
    .await;
    let rev = publish_from(&runtime, &shop, Some(slow)).await;
    let d = rev.published_at.unwrap().date_naive();
    for (day, sample) in [
        (-15, ("product", "LCP", 100.0, 100)),
        (-3, ("product", "LCP", 5000.0, 10)),
        (-2, ("product", "LCP", 3000.0, 30)),
        (-2, ("home", "LCP", 9000.0, 10)),
        (-4, ("product", "INP", 900.0, 10)),
        (0, ("product", "LCP", 9000.0, 10)),
        (1, ("product", "LCP", 2000.0, 10)),
        (15, ("product", "LCP", 9000.0, 10)),
    ] {
        let (template, name, value, n) = sample;
        vitals(
            &runtime,
            &shop,
            days(d, day),
            (template, name, value, n, 100),
        )
        .await;
    }
    assert_eq!(
        change(&runtime, &shop, slow, days(d, 15)).await,
        readout(&rev, measure(Some(3500.0), 40), measure(Some(2000.0), 10))
    );
}

async fn zero_results(runtime: &PgPool, shop: &Shop, rows: &[(NaiveDate, &str, &str, i32)]) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    for (day, locale, query, count) in rows {
        sqlx::query(
            "INSERT INTO search_zero_results (tenant_id, day, locale, query, count)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(shop.tenant)
        .bind(day)
        .bind(locale)
        .bind(query)
        .bind(count)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn zero_result_searches_become_a_signal_with_a_before_after_readout(db: PgPool) {
    let (runtime, shop) = setup(&db, "zero").await;
    let day = yesterday() - Days::new(30);
    zero_results(
        &runtime,
        &shop,
        &[
            (days(day, -14), "cs", "modre tricko", 50),
            (days(day, -13), "cs", "modre tricko", 6),
            (day, "sk", "modre tricko", 4),
            (day, "cs", "cervene saty", 9),
        ],
    )
    .await;
    run(&runtime, &Ai::fake(), &shop, day).await;
    let found = list(&runtime, shop.tenant).await;
    assert_eq!(
        found
            .iter()
            .map(|i| (&i.detection, i.score))
            .collect::<Vec<_>>(),
        vec![(
            &Detection::ZeroResultSearch {
                query: "modre tricko".into(),
                searches: 10
            },
            5.0
        )]
    );

    let rev = publish_from(&runtime, &shop, Some(found[0].id)).await;
    let d = rev.published_at.unwrap().date_naive();
    zero_results(
        &runtime,
        &shop,
        &[
            (days(d, -15), "cs", "modre tricko", 90),
            (days(d, -14), "cs", "modre tricko", 3),
            (days(d, -1), "sk", "modre tricko", 4),
            (days(d, -5), "cs", "cervene saty", 20),
            (d, "cs", "modre tricko", 100),
            (days(d, 15), "cs", "modre tricko", 90),
        ],
    )
    .await;
    assert_eq!(
        change(&runtime, &shop, found[0].id, days(d, 15)).await,
        readout(&rev, measure(Some(7.0), 7), measure(Some(0.0), 0)),
        "no search without results after the change is a real zero"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_latest_publish_from_an_insight_wins(db: PgPool) {
    let (runtime, shop) = setup(&db, "latest").await;
    let insight = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::ViewItemToAddToCart),
        "open",
    )
    .await;
    let first = publish_from(&runtime, &shop, Some(insight)).await;
    let second = publish_from(&runtime, &shop, Some(insight)).await;
    let shown = |c: Option<ThemeChange>| c.map(|c| (c.revision_id, c.published_at));
    let today = Utc::now().date_naive();
    assert_eq!(
        shown(change(&runtime, &shop, insight, today).await),
        Some((second.id, second.published_at.unwrap()))
    );
    let rollback = republish(&runtime, &shop, first.id).await;
    assert!(rollback.published_at > second.published_at);
    assert_eq!(
        shown(change(&runtime, &shop, insight, today).await),
        Some((first.id, rollback.published_at.unwrap()))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_resolved_insight_stays_listed_while_a_change_from_it_is_published(db: PgPool) {
    let (runtime, shop) = setup(&db, "resolved").await;
    let open = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::SessionsToViewItem),
        "open",
    )
    .await;
    let linked = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::ViewItemToAddToCart),
        "resolved",
    )
    .await;
    signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::AddToCartToBeginCheckout),
        "resolved",
    )
    .await;
    let dismissed = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::BeginCheckoutToPurchase),
        "open",
    )
    .await;
    publish_from(&runtime, &shop, Some(linked)).await;
    let rev = publish_from(&runtime, &shop, Some(dismissed)).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let gone = insights::dismiss(&mut tx, "clerk", dismissed, Utc::now().date_naive())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (gone.status, gone.theme_change.map(|c| c.revision_id)),
        (Status::Dismissed, Some(rev.id))
    );

    let listed: Vec<(Uuid, Status, bool)> = list(&runtime, shop.tenant)
        .await
        .into_iter()
        .map(|i| (i.id, i.status, i.theme_change.is_some()))
        .collect();
    assert_eq!(
        listed,
        vec![
            (open, Status::Open, false),
            (linked, Status::Resolved, true)
        ]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_publish_from_a_run_without_an_insight_links_nothing(db: PgPool) {
    let (runtime, shop) = setup(&db, "unlinked").await;
    let insight = signal(
        &runtime,
        &shop,
        &funnel_drop(Transition::ViewItemToAddToCart),
        "open",
    )
    .await;
    publish_from(&runtime, &shop, None).await;
    assert_eq!(
        change(&runtime, &shop, insight, Utc::now().date_naive()).await,
        None
    );
}
