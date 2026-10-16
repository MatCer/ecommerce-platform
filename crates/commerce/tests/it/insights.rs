use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Days, NaiveDate, Utc};
use commerce::ai::settings::{self, AiSettingsChange};
use commerce::ai::{Ai, Outcome};
use commerce::insights::detect::{Detection, Pair, Transition};
use commerce::insights::{self, Insight};
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

async fn fixture(runtime: &PgPool, shop: &Shop, day: NaiveDate, home: bool) -> Uuid {
    let funnel = |v: [f64; 5]| {
        [
            ("sessions", v[0]),
            ("funnel.view_item", v[1]),
            ("funnel.add_to_cart", v[2]),
            ("funnel.begin_checkout", v[3]),
            ("funnel.purchase", v[4]),
        ]
    };
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
    insights::list(&mut tx).await.unwrap()
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
    let dismissed = insights::dismiss(&mut tx, "clerk", slow).await.unwrap();
    let again = insights::dismiss(&mut tx, "clerk", slow).await.unwrap();
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
    let err = insights::dismiss(&mut tx, "intruder", id)
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
                hypothesis_model = NULL, hypothesis_at = NULL",
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
    let mut owner = db.begin().await.unwrap();
    for sql in [
        "ALTER TABLE ai_usage NO FORCE ROW LEVEL SECURITY",
        "UPDATE ai_usage SET at = at - interval '1 day'",
        "ALTER TABLE ai_usage FORCE ROW LEVEL SECURITY",
    ] {
        sqlx::query(sql).execute(&mut *owner).await.unwrap();
    }
    owner.commit().await.unwrap();
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
