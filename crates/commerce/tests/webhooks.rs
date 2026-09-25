//! Outbound webhooks (§8.5, A21): subscriptions, fan-out, signed delivery, retries, dead,
//! redeliver, rotation and tenant isolation.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use commerce::webhooks::{self, Attempt, NewSubscription, SubscriptionUpdate, Webhooks, signature};
use platform::crypto::SecretBox;
use platform::db::tenant_tx;
use platform::http::SafeClient;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Default)]
struct Receiver {
    status: Arc<Mutex<u16>>,
    got: Arc<Mutex<Vec<(HeaderMap, Vec<u8>)>>>,
}

async fn receiver() -> (String, Receiver) {
    let r = Receiver {
        status: Arc::new(Mutex::new(200)),
        ..Receiver::default()
    };
    let state = r.clone();
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let state = state.clone();
            async move {
                state.got.lock().unwrap().push((headers, body.to_vec()));
                StatusCode::from_u16(*state.status.lock().unwrap()).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://localhost:{}/hook",
        listener.local_addr().unwrap().port()
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, r)
}

fn hooks(allow: &[&str]) -> Webhooks {
    Webhooks {
        secrets: SecretBox::new(&[42; 32]),
        http: SafeClient::new(allow.iter().map(|h| (*h).to_owned())).unwrap(),
        require_https: false,
    }
}

fn sub(url: &str, events: &[&str]) -> NewSubscription {
    NewSubscription {
        url: url.into(),
        events: events.iter().map(|e| (*e).to_owned()).collect(),
        description: "ERP".into(),
        active: true,
    }
}

async fn jobs(owner: &PgPool, delivery: Uuid) -> Vec<(i32, String)> {
    sqlx::query_as(
        "SELECT (payload->>'attempt')::int, status FROM queue.jobs
         WHERE kind = 'webhooks.deliver' AND payload->>'delivery_id' = $1 ORDER BY id",
    )
    .bind(delivery.to_string())
    .fetch_all(owner)
    .await
    .unwrap()
}

/// Runs attempt `attempt` of the delivery's current retry window (as its job would).
async fn deliver_now(
    runtime: &PgPool,
    h: &Webhooks,
    tenant: Uuid,
    d: Uuid,
    attempt: i32,
) -> Result<Attempt, platform::Error> {
    let window = window_of(runtime, tenant, d).await;
    webhooks::deliver(runtime, h, tenant, d, attempt, window).await
}

async fn window_of(runtime: &PgPool, tenant: Uuid, d: Uuid) -> chrono::DateTime<chrono::Utc> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let w = sqlx::query_scalar("SELECT window_started_at FROM webhook_deliveries WHERE id = $1")
        .bind(d)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    w
}

#[sqlx::test(migrations = "../../migrations")]
async fn signed_delivery_retries_dead_and_redeliver(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let (tenant, _) = testkit::tenant(&runtime, "hooks").await;
    let (other, _) = testkit::tenant(&runtime, "other").await;
    let (url, rx) = receiver().await;
    let h = hooks(&["localhost"]);

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let created = webhooks::create(&mut tx, &h, "staff", &sub(&url, &["order.paid"]))
        .await
        .unwrap();
    assert!(created.secret.starts_with("whsec_") && created.secret.len() == 70);
    assert!(created.secret.ends_with(&created.subscription.secret_hint));
    webhooks::create(&mut tx, &h, "staff", &sub(&url, &["product.created"]))
        .await
        .unwrap();
    let off = webhooks::create(&mut tx, &h, "staff", &sub(&url, &["order.paid"]))
        .await
        .unwrap();
    webhooks::update(
        &mut tx,
        &h,
        "staff",
        off.subscription.id,
        &SubscriptionUpdate {
            active: Some(false),
            ..SubscriptionUpdate::default()
        },
    )
    .await
    .unwrap();
    // The secret is stored encrypted only.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT secret_ciphertext FROM webhook_subscriptions LIMIT 1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("whsec_"));
    tx.commit().await.unwrap();

    // Fan-out: one delivery (the matching, active subscription); idempotent per event.
    let data = json!({ "order_id": Uuid::nil(), "number": "1" });
    assert_eq!(
        webhooks::fanout(&runtime, tenant, 7, "order.paid", &data)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        webhooks::fanout(&runtime, tenant, 7, "order.paid", &data)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        webhooks::fanout(&runtime, tenant, 8, "order.exception", &data)
            .await
            .unwrap(),
        0
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let page = webhooks::deliveries(&mut tx, None, None, 10).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(page.items.len(), 1);
    let d = page.items[0].id;
    assert_eq!(jobs(&db, d).await, [(1, "queued".to_owned())]);

    // Failure → retry scheduled as attempt 2; a duplicate attempt-1 job is skipped.
    *rx.status.lock().unwrap() = 500;
    let a1 = deliver_now(&runtime, &h, tenant, d, 1).await.unwrap();
    assert!(matches!(a1, Attempt::Retrying(_)), "{a1:?}");
    assert_eq!(
        deliver_now(&runtime, &h, tenant, d, 1).await.unwrap(),
        Attempt::Skipped
    );
    assert_eq!(jobs(&db, d).await.len(), 2);

    // Success on attempt 2, signed with the subscription's secret.
    *rx.status.lock().unwrap() = 204;
    assert_eq!(
        deliver_now(&runtime, &h, tenant, d, 2).await.unwrap(),
        Attempt::Succeeded
    );
    let (headers, body) = rx.got.lock().unwrap().last().cloned().unwrap();
    let sig = headers["x-signature"].to_str().unwrap();
    let ts: i64 = sig.split(',').next().unwrap()[2..].parse().unwrap();
    assert_eq!(sig, signature(&created.secret, ts, &body));
    assert_eq!(headers["x-webhook-id"].to_str().unwrap(), d.to_string());
    assert_eq!(headers["x-webhook-event"], "order.paid");
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["id"], "evt_7");
    assert_eq!(payload["data"]["number"], "1");
    assert!(headers.get("cookie").is_none() && headers.get("authorization").is_none());

    // Redeliver a succeeded delivery (a new window: its job key differs from earlier ones).
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let old_window = window_of(&runtime, tenant, d).await;
    let again = webhooks::redeliver(&mut tx, "staff", d).await.unwrap();
    assert_eq!(again.status, "retrying");
    assert!(
        webhooks::redeliver(&mut tx, "staff", d).await.is_err(),
        "in progress"
    );
    tx.commit().await.unwrap();
    assert_eq!(jobs(&db, d).await.len(), 3);
    // A job of the earlier window (e.g. overdue) cannot act on the new one.
    assert_eq!(
        webhooks::deliver(&runtime, &h, tenant, d, 3, old_window)
            .await
            .unwrap(),
        Attempt::Skipped
    );
    // An attempt that only runs after the 24 h window (backlog, outage) sends nothing: dead.
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query("UPDATE webhook_deliveries SET window_started_at = now() - interval '25 hours'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let received = rx.got.lock().unwrap().len();
    assert_eq!(
        deliver_now(&runtime, &h, tenant, d, 3).await.unwrap(),
        Attempt::Dead
    );
    assert_eq!(rx.got.lock().unwrap().len(), received, "nothing sent late");
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let dead = &webhooks::deliveries(&mut tx, Some(created.subscription.id), None, 10)
        .await
        .unwrap()
        .items[0];
    assert_eq!(
        (dead.status.as_str(), dead.attempts, dead.response_code),
        ("dead", 3, None)
    );
    // A delivery whose job was lost (overdue by over an hour) can be redelivered too.
    sqlx::query(
        "UPDATE webhook_deliveries SET status = 'retrying', next_at = now() - interval '2 hours'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    webhooks::redeliver(&mut tx, "staff", d).await.unwrap();
    sqlx::query("UPDATE webhook_deliveries SET status = 'dead', attempts = 3")
        .execute(&mut *tx)
        .await
        .unwrap();

    // Rotation: the next delivery is signed with the new secret.
    let rotated = webhooks::rotate_secret(&mut tx, &h, "staff", created.subscription.id)
        .await
        .unwrap();
    assert_ne!(rotated.secret, created.secret);
    webhooks::redeliver(&mut tx, "staff", d).await.unwrap();
    tx.commit().await.unwrap();
    *rx.status.lock().unwrap() = 200;
    assert_eq!(
        deliver_now(&runtime, &h, tenant, d, 4).await.unwrap(),
        Attempt::Succeeded
    );
    let (headers, body) = rx.got.lock().unwrap().last().cloned().unwrap();
    let sig = headers["x-signature"].to_str().unwrap();
    let ts: i64 = sig.split(',').next().unwrap()[2..].parse().unwrap();
    assert_eq!(sig, signature(&rotated.secret, ts, &body));

    // Tenant isolation.
    let mut tx = tenant_tx(&runtime, other).await.unwrap();
    assert!(webhooks::list(&mut tx).await.unwrap().items.is_empty());
    assert!(
        webhooks::deliveries(&mut tx, None, None, 10)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(webhooks::redeliver(&mut tx, "staff", d).await.is_err());
    assert!(
        webhooks::delete(&mut tx, "staff", created.subscription.id)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn private_destinations_are_refused_at_delivery(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "hooks").await;
    let (url, rx) = receiver().await;
    // No allowlist: `localhost` resolves to loopback.
    let h = hooks(&[]);
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    webhooks::create(&mut tx, &h, "staff", &sub(&url, &["customer.created"]))
        .await
        .unwrap();
    assert!(
        webhooks::create(
            &mut tx,
            &h,
            "staff",
            &sub("http://127.0.0.1:9/x", &["order.paid"])
        )
        .await
        .is_err(),
        "literal private addresses are refused up front"
    );
    tx.commit().await.unwrap();
    webhooks::fanout(&runtime, tenant, 1, "customer.created", &json!({}))
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let d = webhooks::deliveries(&mut tx, None, None, 1)
        .await
        .unwrap()
        .items[0]
        .id;
    tx.commit().await.unwrap();
    let a = deliver_now(&runtime, &h, tenant, d, 1).await.unwrap();
    assert!(matches!(a, Attempt::Retrying(_)));
    assert!(
        rx.got.lock().unwrap().is_empty(),
        "nothing reached the receiver"
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let d = &webhooks::deliveries(&mut tx, None, None, 1)
        .await
        .unwrap()
        .items[0];
    assert!(
        d.last_error.as_deref().unwrap().contains("not a public"),
        "{:?}",
        d.last_error
    );
}
