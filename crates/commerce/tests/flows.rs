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
