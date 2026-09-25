//! WP19 against Postgres as app_runtime: consent and exits, one-use capabilities, watch
//! double opt-in, review tokens and RLS isolation.
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use commerce::consent::{self, ConsentPurpose, Subject};
use commerce::flows::{self, WatchInput};
use commerce::storefront::{self, PublicUrls};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::{PgPool, Row};
use testkit::storefront::{raw_order, shop};
use uuid::Uuid;

fn token_in(body: &str, marker: &str) -> String {
    let at = body.find(marker).unwrap() + marker.len();
    body[at..at + 64].to_owned()
}

async fn cart_with_consent(
    tx: &mut platform::db::TenantTx,
    market: Uuid,
    variant: Uuid,
    email: &str,
    customer: Option<Uuid>,
) -> Uuid {
    let cart: Uuid = sqlx::query_scalar(
        "INSERT INTO carts(tenant_id,market_id,email,customer_id,locale,currency,last_activity_at)
        VALUES($1,$2,$3,$4,'cs','CZK',$5) RETURNING id",
    )
    .bind(tx.tenant_id())
    .bind(market)
    .bind(email)
    .bind(customer)
    .bind(Utc::now() - Duration::hours(2))
    .fetch_one(&mut **tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cart_lines(tenant_id,cart_id,variant_id,quantity) VALUES($1,$2,$3,1)")
        .bind(tx.tenant_id())
        .bind(cart)
        .bind(variant)
        .execute(&mut **tx)
        .await
        .unwrap();
    cart
}

#[sqlx::test(migrations = "../../migrations")]
async fn active_run_keeps_its_schedule_after_definition_shrinks(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowsnapshot").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    cart_with_consent(&mut tx, s.cz, s.variants[0], "schedule@example.com", None).await;
    consent::record_server(
        &mut tx,
        &Subject::Email("schedule@example.com".into()),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
        .await
        .unwrap();
    flows::configure(
        &mut tx,
        "boss",
        "abandoned_cart",
        &flows::DefinitionChange {
            enabled: true,
            config: flows::FlowConfig {
                delays_hours: vec![1],
                coupon_percent: None,
            },
        },
    )
    .await
    .unwrap();
    flows::execute_due(
        &mut tx,
        &PublicUrls::default(),
        Utc::now() + Duration::hours(25),
    )
    .await
    .unwrap();
    let sent: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(sent, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn simultaneous_cart_runs_receive_distinct_random_coupons(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowcoupons").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    flows::configure(
        &mut tx,
        "boss",
        "abandoned_cart",
        &flows::DefinitionChange {
            enabled: true,
            config: flows::FlowConfig {
                delays_hours: vec![1],
                coupon_percent: Some(10),
            },
        },
    )
    .await
    .unwrap();
    for i in 0..3 {
        let email = format!("coupon{i}@example.com");
        cart_with_consent(&mut tx, s.cz, s.variants[0], &email, None).await;
        consent::record_server(
            &mut tx,
            &Subject::Email(email),
            ConsentPurpose::EmailMarketing,
            true,
            consent::TEXT_VERSION,
            "checkout",
            None,
        )
        .await
        .unwrap();
    }
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
        .await
        .unwrap();
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT coupon_code FROM flow_runs WHERE source_kind='cart' ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    assert_eq!(codes.len(), 3);
    assert_eq!(
        codes.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    assert!(codes.iter().all(|c| c.len() == 32 && c.starts_with("FLOW")));
}

#[sqlx::test(migrations = "../../migrations")]
async fn enrollment_honors_latest_customer_and_email_consent(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowconsent").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let customer: Uuid = sqlx::query_scalar("INSERT INTO customers(tenant_id,email,locale) VALUES($1,'choice@example.com','cs') RETURNING id")
        .bind(s.tenant).fetch_one(&mut *tx).await.unwrap();
    let cart = cart_with_consent(
        &mut tx,
        s.cz,
        s.variants[0],
        "choice@example.com",
        Some(customer),
    )
    .await;
    consent::record_server(
        &mut tx,
        &Subject::Customer(customer),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "preferences",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    let enrolled: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_runs WHERE source_id=$1")
        .bind(cart)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(enrolled, 1);
    sqlx::query("DELETE FROM flow_runs WHERE source_id=$1")
        .bind(cart)
        .execute(&mut *tx)
        .await
        .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("choice@example.com".into()),
        ConsentPurpose::EmailMarketing,
        false,
        consent::TEXT_VERSION,
        "preferences",
        None,
    )
    .await
    .unwrap();
    flows::on_event(
        &mut tx,
        &PublicUrls::default(),
        "cart.changed",
        &json!({"cart_id":cart}),
        Utc::now(),
    )
    .await
    .unwrap();
    let enrolled: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_runs WHERE source_id=$1")
        .bind(cart)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(enrolled, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn cancelled_run_and_disabled_watchdog_block_queued_delivery(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowdeliver").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    cart_with_consent(&mut tx, s.cz, s.variants[0], "cancel@example.com", None).await;
    consent::record_server(
        &mut tx,
        &Subject::Email("cancel@example.com".into()),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
        .await
        .unwrap();
    let cart_message: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let run: Uuid = sqlx::query_scalar("SELECT id FROM flow_runs WHERE source_kind='cart'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    flows::cancel_run(&mut tx, "boss", run).await.unwrap();

    let ctx = storefront::context(&mut tx, &PublicUrls::default(), s.cz, None, Utc::now())
        .await
        .unwrap();
    flows::subscribe_watch(
        &mut tx,
        &ctx,
        &WatchInput {
            variant_id: s.variants[0],
            kind: "back_in_stock".into(),
            target_minor: None,
            email: "disable@example.com".into(),
        },
    )
    .await
    .unwrap();
    let body: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_confirm'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    flows::confirm_watch(&mut tx, &token_in(&body, "token="), Utc::now())
        .await
        .unwrap();
    let event =
        json!({"variant_id":s.variants[0],"before":{"available":0},"after":{"available":10}});
    flows::watch_event(
        &mut tx,
        &PublicUrls::default(),
        "inventory.changed",
        &event,
        Utc::now(),
    )
    .await
    .unwrap();
    let watch_message: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='watch_alert'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    flows::configure(
        &mut tx,
        "boss",
        "watchdog",
        &flows::DefinitionChange {
            enabled: false,
            config: flows::FlowConfig::for_kind("watchdog"),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    for id in [cart_message, watch_message] {
        assert!(matches!(
            commerce::notifications::begin_send(&rt, s.tenant, id)
                .await
                .unwrap(),
            Err(commerce::notifications::Step::Done)
        ));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn erasure_removes_flow_subject_and_prevents_post_erasure_alert(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowerasure").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), s.cz, None, Utc::now())
        .await
        .unwrap();
    flows::subscribe_watch(
        &mut tx,
        &ctx,
        &WatchInput {
            variant_id: s.variants[0],
            kind: "back_in_stock".into(),
            target_minor: None,
            email: "erase@example.com".into(),
        },
    )
    .await
    .unwrap();
    let body: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_confirm'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    flows::confirm_watch(&mut tx, &token_in(&body, "token="), Utc::now())
        .await
        .unwrap();
    let cart = cart_with_consent(&mut tx, s.cz, s.variants[0], "erase@example.com", None).await;
    consent::record_server(
        &mut tx,
        &Subject::Email("erase@example.com".into()),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
        .await
        .unwrap();
    let doc = commerce::privacy::access(&mut tx, "boss", "erase@example.com")
        .await
        .unwrap();
    assert_eq!(doc["flow_watches"].as_array().unwrap().len(), 1);
    assert_eq!(doc["flow_runs"].as_array().unwrap().len(), 1);
    assert_eq!(doc["flow_runs"][0]["steps"].as_array().unwrap().len(), 1);
    assert_eq!(doc["flow_restore_tokens"].as_array().unwrap().len(), 1);
    assert_eq!(doc["flow_unsubscribe_tokens"].as_array().unwrap().len(), 1);
    assert!(doc["flow_watches"][0].get("confirm_hash").is_none());
    let message: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    commerce::privacy::erase(
        &mut tx,
        "boss",
        &commerce::privacy::ErasureRequest {
            email: "erase@example.com".into(),
            confirm_email: "erase@example.com".into(),
        },
    )
    .await
    .unwrap();
    let event =
        json!({"variant_id":s.variants[0],"before":{"available":0},"after":{"available":10}});
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "inventory.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        0
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM flow_watches WHERE email='erase@example.com'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_runs WHERE source_id=$1")
        .bind(cart)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    tx.commit().await.unwrap();
    assert!(matches!(
        commerce::notifications::begin_send(&rt, s.tenant, message)
            .await
            .unwrap(),
        Err(commerce::notifications::Step::Done)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn watch_subscription_generation_sends_again_after_unsubscribe(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "watchagain").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), s.cz, None, Utc::now())
        .await
        .unwrap();
    let input = WatchInput {
        variant_id: s.variants[0],
        kind: "back_in_stock".into(),
        target_minor: None,
        email: "again@example.com".into(),
    };
    let event =
        json!({"variant_id":s.variants[0],"before":{"available":0},"after":{"available":10}});
    for generation in 1..=2 {
        flows::subscribe_watch(&mut tx, &ctx, &input).await.unwrap();
        let body: String = sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_confirm' AND to_email='again@example.com' ORDER BY created_at DESC LIMIT 1")
            .fetch_one(&mut *tx).await.unwrap();
        assert!(
            flows::confirm_watch(&mut tx, &token_in(&body, "token="), Utc::now())
                .await
                .unwrap()
        );
        assert_eq!(
            flows::watch_event(
                &mut tx,
                &PublicUrls::default(),
                "inventory.changed",
                &event,
                Utc::now()
            )
            .await
            .unwrap(),
            1
        );
        let alerts: i64 = sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='watch_alert' AND to_email='again@example.com'")
            .fetch_one(&mut *tx).await.unwrap();
        assert_eq!(alerts, generation);
        let alert: String = sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_alert' AND to_email='again@example.com' ORDER BY created_at DESC LIMIT 1")
            .fetch_one(&mut *tx).await.unwrap();
        assert!(
            flows::unsubscribe_watch(&mut tx, &token_in(&alert, "token="))
                .await
                .unwrap()
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn watch_event_continues_past_ineligible_and_first_hundred(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "watchbatch").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    flows::ensure_defaults(&mut tx).await.unwrap();
    for i in 0..125 {
        let email = format!("person{i}@example.com");
        sqlx::query("INSERT INTO flow_watches(tenant_id,market_id,variant_id,kind,email,locale,status,unsubscribe_hash) VALUES($1,$2,$3,'back_in_stock',$4,'cs','confirmed',$5)")
            .bind(s.tenant).bind(s.cz).bind(s.variants[0]).bind(email)
            .bind(vec![i as u8; 32]).execute(&mut *tx).await.unwrap();
    }
    let event =
        json!({"variant_id":s.variants[0],"before":{"available":0},"after":{"available":10}});
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "inventory.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        100
    );
    tx.commit().await.unwrap();
    let continuation: serde_json::Value = sqlx::query_scalar(
        "SELECT payload->'payload' FROM queue.jobs WHERE kind='flows.event' LIMIT 1",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    flows::on_event(
        &mut tx,
        &PublicUrls::default(),
        "inventory.changed",
        &continuation,
        Utc::now(),
    )
    .await
    .unwrap();
    let alerts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='watch_alert'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(alerts, 125);
}

#[sqlx::test(migrations = "../../migrations")]
async fn watch_event_filters_market_and_threshold_before_page(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "watchfilter").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    flows::ensure_defaults(&mut tx).await.unwrap();
    for i in 0..101 {
        let market = if i < 100 { s.sk } else { s.cz };
        sqlx::query("INSERT INTO flow_watches(tenant_id,market_id,variant_id,kind,target_minor,email,locale,status,unsubscribe_hash)
            VALUES($1,$2,$3,'price_drop',13000,$4,'cs','confirmed',$5)")
            .bind(s.tenant).bind(market).bind(s.variants[0])
            .bind(format!("filter{i}@example.com")).bind(vec![i as u8; 32])
            .execute(&mut *tx).await.unwrap();
    }
    sqlx::query("INSERT INTO flow_watches(tenant_id,market_id,variant_id,kind,target_minor,email,locale,status,unsubscribe_hash)
        VALUES($1,$2,$3,'price_drop',12000,'too-low@example.com','cs','confirmed',$4)")
        .bind(s.tenant).bind(s.cz).bind(s.variants[0]).bind(vec![222_u8; 32])
        .execute(&mut *tx).await.unwrap();
    let event = json!({"variant_id":s.variants[0],"price_list_id":s.czk,
        "before_minor":14_900,"after_minor":12_900});
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "price.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirmation_quota_is_recipient_wide_under_concurrency(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 8).await;
    let s = shop(&rt, "watchquota").await;
    let cases = [
        (s.cz, s.variants[0], "back_in_stock"),
        (s.cz, s.variants[1], "back_in_stock"),
        (s.sk, s.variants[0], "price_drop"),
        (s.sk, s.variants[1], "price_drop"),
    ];
    let mut tasks = Vec::new();
    for (market, variant_id, kind) in cases {
        let rt = rt.clone();
        let tenant = s.tenant;
        tasks.push(tokio::spawn(async move {
            let mut tx = tenant_tx(&rt, tenant).await.unwrap();
            let ctx =
                storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
                    .await
                    .unwrap();
            let input = WatchInput {
                variant_id,
                kind: kind.into(),
                target_minor: None,
                email: "victim@example.com".into(),
            };
            flows::subscribe_watch_with_ip(&mut tx, &ctx, &input, Some(&[7_u8; 32]))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let confirmations: i64 = sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='watch_confirm' AND to_email='victim@example.com'")
        .fetch_one(&mut *tx).await.unwrap();
    assert_eq!(confirmations, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn confirmation_quota_is_ip_wide_across_recipients(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "watchipquota").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), s.cz, None, Utc::now())
        .await
        .unwrap();
    for i in 0..21 {
        flows::subscribe_watch_with_ip(
            &mut tx,
            &ctx,
            &WatchInput {
                variant_id: s.variants[0],
                kind: "back_in_stock".into(),
                target_minor: None,
                email: format!("ipvictim{i}@example.com"),
            },
            Some(&[9_u8; 32]),
        )
        .await
        .unwrap();
    }
    let confirmations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='watch_confirm'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(confirmations, 20);
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_confirmation_quotas_are_purged(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "quotaretention").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO flow_watch_mail_quotas(tenant_id,scope,identifier,window_started_at)
        VALUES($1,'recipient','old@example.com',now()-interval '3 days')",
    )
    .bind(s.tenant)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let purged: i64 = sqlx::query_scalar("SELECT platform.purge_flow_watch_mail_quotas()")
        .fetch_one(&rt)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM flow_watch_mail_quotas")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn abandoned_cart_rechecks_consent_and_restore_is_single_use(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowcart").await;
    let other = shop(&rt, "flowother").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let cart: Uuid = sqlx::query_scalar(
        "INSERT INTO carts(tenant_id,market_id,email,locale,currency,last_activity_at)
        VALUES($1,$2,'buyer@example.com','cs','CZK',$3) RETURNING id",
    )
    .bind(s.tenant)
    .bind(s.cz)
    .bind(Utc::now() - Duration::hours(2))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cart_lines(tenant_id,cart_id,variant_id,quantity) VALUES($1,$2,$3,1)")
        .bind(s.tenant)
        .bind(cart)
        .bind(s.variants[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("buyer@example.com".into()),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    assert_eq!(
        flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
            .await
            .unwrap(),
        1
    );
    let body: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let restore = token_in(&body, "token=");
    sqlx::query("INSERT INTO flow_test_clocks(tenant_id) VALUES($1)")
        .bind(s.tenant)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO flow_watches(tenant_id,market_id,variant_id,kind,email,locale,unsubscribe_hash)
        VALUES($1,$2,$3,'back_in_stock','watch@example.com','cs',$4)")
        .bind(s.tenant).bind(s.cz).bind(s.variants[0]).bind(vec![4_u8;32])
        .execute(&mut *tx).await.unwrap();
    sqlx::query(
        "INSERT INTO flow_watch_mail_quotas(tenant_id,scope,identifier)
        VALUES($1,'recipient','watch@example.com')",
    )
    .bind(s.tenant)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut alien = tenant_tx(&rt, other.tenant).await.unwrap();
    for (table, query) in [
        ("flow_definitions", "SELECT count(*) FROM flow_definitions"),
        ("flow_runs", "SELECT count(*) FROM flow_runs"),
        ("flow_steps", "SELECT count(*) FROM flow_steps"),
        (
            "flow_restore_tokens",
            "SELECT count(*) FROM flow_restore_tokens",
        ),
        ("flow_watches", "SELECT count(*) FROM flow_watches"),
        ("flow_test_clocks", "SELECT count(*) FROM flow_test_clocks"),
        (
            "flow_watch_mail_quotas",
            "SELECT count(*) FROM flow_watch_mail_quotas",
        ),
        (
            "flow_unsubscribe_tokens",
            "SELECT count(*) FROM flow_unsubscribe_tokens",
        ),
    ] {
        let visible: i64 = sqlx::query_scalar(query)
            .fetch_one(&mut *alien)
            .await
            .unwrap();
        assert_eq!(visible, 0, "{table} leaked across tenants");
    }
    assert!(
        flows::restore_cart(&mut alien, other.cz, &restore, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    alien.commit().await.unwrap();

    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    assert!(
        flows::restore_cart(&mut tx, s.cz, &restore, Utc::now())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        flows::restore_cart(&mut tx, s.cz, &restore, Utc::now())
            .await
            .unwrap()
            .is_none()
    );
    consent::record_server(
        &mut tx,
        &Subject::Email("buyer@example.com".into()),
        ConsentPurpose::EmailMarketing,
        false,
        consent::TEXT_VERSION,
        "unsubscribe",
        None,
    )
    .await
    .unwrap();
    let later = Utc::now() + Duration::days(2);
    flows::execute_due(&mut tx, &PublicUrls::default(), later)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let message_id: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='abandoned_cart'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        commerce::notifications::begin_send(&rt, s.tenant, message_id)
            .await
            .unwrap(),
        Err(commerce::notifications::Step::Done)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn watchdog_requires_confirmation_fires_once_and_unsubscribes(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowwatch").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), s.cz, None, Utc::now())
        .await
        .unwrap();
    flows::subscribe_watch(
        &mut tx,
        &ctx,
        &WatchInput {
            variant_id: s.variants[0],
            kind: "back_in_stock".into(),
            target_minor: None,
            email: "watch@example.com".into(),
        },
    )
    .await
    .unwrap();
    let body: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_confirm'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let token = token_in(&body, "token=");
    let event =
        json!({"variant_id":s.variants[0],"before":{"available":0},"after":{"available":10}});
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "inventory.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        0
    );
    assert!(
        flows::confirm_watch(&mut tx, &token, Utc::now())
            .await
            .unwrap()
    );
    assert!(
        !flows::confirm_watch(&mut tx, &token, Utc::now())
            .await
            .unwrap()
    );
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "inventory.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "inventory.changed",
            &event,
            Utc::now()
        )
        .await
        .unwrap(),
        0
    );
    let alert: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='watch_alert'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let unsub = token_in(&alert, "token=");
    let message_id: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='watch_alert'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let run_id: Uuid = sqlx::query_scalar("SELECT id FROM flow_runs WHERE source_kind='watch'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        flows::run_detail(&mut tx, run_id)
            .await
            .unwrap()
            .steps
            .len(),
        1
    );
    assert!(flows::unsubscribe_watch(&mut tx, &unsub).await.unwrap());

    flows::subscribe_watch(
        &mut tx,
        &ctx,
        &WatchInput {
            variant_id: s.variants[0],
            kind: "price_drop".into(),
            target_minor: Some(13_000),
            email: "price@example.com".into(),
        },
    )
    .await
    .unwrap();
    let confirmation: String = sqlx::query_scalar(
        "SELECT body_text FROM email_messages
        WHERE template='watch_confirm' AND to_email='price@example.com'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let price_token = token_in(&confirmation, "token=");
    assert!(
        flows::confirm_watch(&mut tx, &price_token, Utc::now())
            .await
            .unwrap()
    );
    let price_event = json!({"variant_id":s.variants[0],"price_list_id":s.czk,
        "before_minor":14_900,"after_minor":12_900});
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "price.changed",
            &price_event,
            Utc::now()
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        flows::watch_event(
            &mut tx,
            &PublicUrls::default(),
            "price.changed",
            &price_event,
            Utc::now()
        )
        .await
        .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    assert!(matches!(
        commerce::notifications::begin_send(&rt, s.tenant, message_id)
            .await
            .unwrap(),
        Err(commerce::notifications::Step::Done)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn delivered_order_invite_issues_review_token_only_with_consent(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowreview").await;
    let order = raw_order(&rt, &s, s.cz, "CZK", 12_900, 1, "delivered").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO shipments(tenant_id,order_id,carrier,status,delivered_at,created_by)
        VALUES($1,$2,'personal_pickup','delivered',$3,'test')",
    )
    .bind(s.tenant)
    .bind(order)
    .bind(Utc::now() - Duration::days(8))
    .execute(&mut *tx)
    .await
    .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("buyer@example.com".into()),
        ConsentPurpose::ReviewInvites,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    flows::enroll_due(&mut tx, Utc::now()).await.unwrap();
    assert_eq!(
        flows::execute_due(&mut tx, &PublicUrls::default(), Utc::now())
            .await
            .unwrap(),
        1
    );
    let body: String =
        sqlx::query_scalar("SELECT body_text FROM email_messages WHERE template='review_invite'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let token = token_in(&body, "token=");
    assert!(commerce::reviews::invitation(&mut tx, &token).await.is_ok());
    let message_id: Uuid =
        sqlx::query_scalar("SELECT id FROM email_messages WHERE template='review_invite'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("buyer@example.com".into()),
        ConsentPurpose::ReviewInvites,
        false,
        consent::TEXT_VERSION,
        "preferences",
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        commerce::notifications::begin_send(&rt, s.tenant, message_id)
            .await
            .unwrap(),
        Err(commerce::notifications::Step::Done)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn poison_step_stops_after_three_attempts(db: PgPool) {
    let rt = testkit::runtime_pool(&db, 4).await;
    let s = shop(&rt, "flowretry").await;
    let mut tx = tenant_tx(&rt, s.tenant).await.unwrap();
    flows::ensure_defaults(&mut tx).await.unwrap();
    // Bypass the API validator to model a corrupted stored definition: the percent coupon
    // violates the existing coupon constraint after the restore token has been written.
    sqlx::query(
        r#"UPDATE flow_definitions SET config='{"delays_hours":[1],"coupon_percent":999}'
        WHERE kind='abandoned_cart'"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let cart: Uuid = sqlx::query_scalar(
        "INSERT INTO carts(tenant_id,market_id,email,locale,currency,last_activity_at)
        VALUES($1,$2,'retry@example.com','cs','CZK',$3) RETURNING id",
    )
    .bind(s.tenant)
    .bind(s.cz)
    .bind(Utc::now() - Duration::hours(2))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cart_lines(tenant_id,cart_id,variant_id,quantity) VALUES($1,$2,$3,1)")
        .bind(s.tenant)
        .bind(cart)
        .bind(s.variants[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    consent::record_server(
        &mut tx,
        &Subject::Email("retry@example.com".into()),
        ConsentPurpose::EmailMarketing,
        true,
        consent::TEXT_VERSION,
        "checkout",
        None,
    )
    .await
    .unwrap();
    let start = Utc::now();
    flows::enroll_due(&mut tx, start).await.unwrap();
    for attempt in 1..=3 {
        flows::execute_due(
            &mut tx,
            &PublicUrls::default(),
            start + Duration::minutes(6 * attempt),
        )
        .await
        .unwrap();
        let row = sqlx::query("SELECT attempts,status FROM flow_runs WHERE source_id=$1")
            .bind(cart)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(row.try_get::<i16, _>("attempts").unwrap(), attempt as i16);
        assert_eq!(
            row.try_get::<String, _>("status").unwrap(),
            if attempt == 3 { "failed" } else { "active" }
        );
    }
    let messages: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_messages WHERE to_email='retry@example.com'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(messages, 0);
    tx.commit().await.unwrap();
}
