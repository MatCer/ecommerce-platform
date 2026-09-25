//! Mail core against Postgres (runtime role) and a scripted SMTP server: A14 send states and
//! retry policy, suppression (A29), idempotent enqueueing, tenant isolation.
#![allow(clippy::unwrap_used)]

use commerce::notifications::{
    self, Brand, Email, Step, SuppressionReason, Template, brand::Colors,
};
use platform::db::tenant_tx;
use platform::mail::Stream;
use serde_json::json;
use sqlx::PgPool;
use testkit::smtp::{FakeSmtp, Mode, mailer_for};
use uuid::Uuid;

fn brand() -> Brand {
    Brand {
        shop_name: "Demo".into(),
        shop_url: "http://demo.localhost".into(),
        colors: Colors::default(),
    }
}

async fn enqueue(db: &PgPool, tenant: Uuid, key: &str, stream: Stream, to: &str) -> Uuid {
    let mut tx = tenant_tx(db, tenant).await.unwrap();
    let id = notifications::enqueue(
        &mut tx,
        &brand(),
        Email {
            template: Template::MagicLink,
            stream,
            to,
            locale: "cs",
            vars: json!({"url": "http://checkout.demo.localhost/account/verify?token=t", "minutes": 15}),
            idempotency_key: key.into(),
            sensitive: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

#[derive(Debug, PartialEq)]
struct Row {
    status: String,
    attempts: i32,
    uncertain_count: i16,
    has_body: bool,
    last_error: Option<String>,
}

async fn row(db: &PgPool, tenant: Uuid, id: Uuid) -> Row {
    let mut tx = tenant_tx(db, tenant).await.unwrap();
    let r = sqlx::query_as::<_, (String, i32, i16, bool, Option<String>)>(
        "SELECT status, attempts, uncertain_count, html IS NOT NULL, last_error
         FROM email_messages WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    Row {
        status: r.0,
        attempts: r.1,
        uncertain_count: r.2,
        has_body: r.3,
        last_error: r.4,
    }
}

async fn setup(db: PgPool) -> (PgPool, Uuid) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "mail").await;
    (runtime, tenant)
}

#[sqlx::test(migrations = "../../migrations")]
async fn accepted_after_250_with_one_job_per_key_and_sensitive_body_removed(db: PgPool) {
    let owner = db.clone();
    let (db, tenant) = setup(db).await;
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let id = enqueue(
        &db,
        tenant,
        "magic_link:1",
        Stream::Transactional,
        "Jana@Example.test",
    )
    .await;
    let again = enqueue(
        &db,
        tenant,
        "magic_link:1",
        Stream::Transactional,
        "jana@example.test",
    )
    .await;
    assert_eq!(id, again, "the same idempotency key is the same message");
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.jobs WHERE kind = 'mail.send' AND payload->>'message_id' = $1",
    )
    .bind(id.to_string())
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(jobs, 1);
    assert_eq!(row(&db, tenant, id).await.status, "pending");

    let step = notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    assert_eq!(step, Step::Done);
    let r = row(&db, tenant, id).await;
    assert_eq!(
        (r.status.as_str(), r.attempts, r.has_body),
        ("accepted", 1, false)
    );
    let raw = smtp.received();
    assert_eq!(raw.len(), 1);
    assert!(raw[0].contains(&format!("Message-ID: <{id}@mail.test>")));
    assert!(raw[0].contains("From: mail <shop@mail.test>"), "{}", raw[0]);

    // Idempotent: a re-run of the job sends nothing.
    assert_eq!(
        notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
            .await
            .unwrap(),
        Step::Done
    );
    assert_eq!(smtp.received().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn suppressed_addresses_get_nothing(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let mut tx = tenant_tx(&db, tenant).await.unwrap();
    notifications::suppress(
        &mut tx,
        "Bounced@Example.test",
        SuppressionReason::Bounce,
        None,
    )
    .await
    .unwrap();
    notifications::suppress(
        &mut tx,
        "angry@example.test",
        SuppressionReason::Complaint,
        None,
    )
    .await
    .unwrap();
    assert!(
        notifications::is_suppressed(&mut tx, "angry@example.test", Stream::Marketing)
            .await
            .unwrap()
    );
    assert!(
        !notifications::is_suppressed(&mut tx, "angry@example.test", Stream::Transactional)
            .await
            .unwrap(),
        "a complaint stops marketing, not order mail"
    );
    tx.commit().await.unwrap();

    let id = enqueue(
        &db,
        tenant,
        "k1",
        Stream::Transactional,
        "bounced@example.test",
    )
    .await;
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    let r = row(&db, tenant, id).await;
    assert_eq!(r.status, "failed");
    assert_eq!(r.last_error.as_deref(), Some("suppressed"));
    assert_eq!(r.attempts, 0);
    assert!(smtp.received().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn transactional_uncertain_is_retried_exactly_once(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let smtp = FakeSmtp::start(Mode::DropAfterData).await;
    let id = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    let step = notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    assert!(matches!(step, Step::Retry(_)));
    let r = row(&db, tenant, id).await;
    assert_eq!((r.status.as_str(), r.uncertain_count), ("uncertain", 1));
    assert!(r.has_body, "kept for the one retry");

    // The retry dies the same way: final `uncertain`, no third send.
    let step = notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    assert_eq!(step, Step::Done);
    let r = row(&db, tenant, id).await;
    assert_eq!(
        (r.status.as_str(), r.uncertain_count, r.attempts),
        ("uncertain", 2, 2)
    );
    assert!(!r.has_body);
    smtp.set_mode(Mode::Accept);
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    assert!(smtp.received().is_empty());

    // A second message whose retry succeeds.
    smtp.set_mode(Mode::DropAfterData);
    let id2 = enqueue(&db, tenant, "k2", Stream::Transactional, "b@example.test").await;
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id2)
        .await
        .unwrap();
    smtp.set_mode(Mode::Accept);
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id2)
        .await
        .unwrap();
    assert_eq!(row(&db, tenant, id2).await.status, "accepted");
    assert_eq!(smtp.received().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn marketing_uncertain_is_never_resent(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let smtp = FakeSmtp::start(Mode::DropAfterData).await;
    let id = enqueue(&db, tenant, "news:1", Stream::Marketing, "a@example.test").await;
    let step = notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    assert_eq!(step, Step::Done);
    smtp.set_mode(Mode::Accept);
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    let r = row(&db, tenant, id).await;
    assert_eq!((r.status.as_str(), r.attempts), ("uncertain", 1));
    assert!(smtp.received().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_worker_dying_mid_send_leaves_uncertain_then_one_retry(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let id = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    // As left behind by a crash between `sending` and the SMTP answer.
    let mut tx = tenant_tx(&db, tenant).await.unwrap();
    sqlx::query(
        "UPDATE email_messages SET status = 'sending', attempts = 1, send_token = gen_random_uuid()
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // Recent: another worker may still be sending it; nothing is touched.
    assert!(matches!(
        notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
            .await
            .unwrap(),
        Step::Retry(_)
    ));
    assert_eq!(row(&db, tenant, id).await.status, "sending");
    assert!(smtp.received().is_empty());
    // Stale: that worker is gone; uncertain, then the one retry.
    age(&db, tenant, id, "5 minutes").await;
    notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap();
    let r = row(&db, tenant, id).await;
    assert_eq!(
        (r.status.as_str(), r.uncertain_count, r.attempts),
        ("accepted", 1, 2)
    );
    assert_eq!(smtp.received().len(), 1);
}

async fn age(db: &PgPool, tenant: Uuid, id: Uuid, by: &str) {
    let mut tx = tenant_tx(db, tenant).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE email_messages SET updated_at = now() - interval '{by}' WHERE id = $1"
    )))
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_stale_worker_cannot_overwrite_a_newer_attempt(db: PgPool) {
    use platform::mail::Delivery;
    let (db, tenant) = setup(db).await;
    let id = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    let first = notifications::begin_send(&db, tenant, id)
        .await
        .unwrap()
        .unwrap();
    // Worker 1 hangs past its lease; worker 2 takes the message over.
    age(&db, tenant, id, "5 minutes").await;
    let second = notifications::begin_send(&db, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.token, second.token);
    assert_eq!(second.uncertain_count, 1);
    // Worker 1's late answer is ignored, whatever it is.
    assert_eq!(
        notifications::finish_send(&db, &first, Delivery::Rejected("550".into()))
            .await
            .unwrap(),
        Step::Done
    );
    assert_eq!(row(&db, tenant, id).await.status, "sending");
    notifications::finish_send(&db, &second, Delivery::Accepted)
        .await
        .unwrap();
    let r = row(&db, tenant, id).await;
    assert_eq!((r.status.as_str(), r.uncertain_count), ("accepted", 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn stalled_deliveries_are_reconciled_across_tenants(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let (other, _) = testkit::tenant(&db, "other").await;
    let stuck = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    let fresh = enqueue(&db, tenant, "k2", Stream::Transactional, "b@example.test").await;
    let elsewhere = enqueue(&db, other, "k1", Stream::Marketing, "c@example.test").await;
    age(&db, tenant, stuck, "2 hours").await;
    age(&db, other, elsewhere, "2 hours").await;
    let jobs = notifications::reconcile_jobs(&db).await.unwrap();
    let mut found: Vec<_> = jobs
        .iter()
        .map(|j| {
            (
                j.tenant_id,
                j.payload["message_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    found.sort();
    let mut want = vec![
        (Some(tenant), stuck.to_string()),
        (Some(other), elsewhere.to_string()),
    ];
    want.sort();
    assert_eq!(found, want);
    assert!(!found.iter().any(|(_, id)| *id == fresh.to_string()));
    // Enqueueing them twice for the same stall is one job each.
    let key = jobs[0].idempotency_key.clone().unwrap();
    assert!(key.starts_with("mail:"));
    let again = notifications::reconcile_jobs(&db).await.unwrap();
    assert_eq!(again[0].idempotency_key.as_deref(), Some(key.as_str()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unreachable_or_deferring_server_retries_then_fails(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let down = mailer_for("smtp://127.0.0.1:1");
    let id = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    let step = notifications::deliver(&db, &down, None, tenant, id)
        .await
        .unwrap();
    assert!(matches!(step, Step::Retry(_)));
    assert_eq!(row(&db, tenant, id).await.status, "pending");
    let smtp = FakeSmtp::start(Mode::Defer).await;
    // Deferred (4xx) until the message runs out of SMTP attempts (8), then `failed`.
    let mut steps = 0;
    while let Step::Retry(_) = notifications::deliver(&db, &smtp.mailer(), None, tenant, id)
        .await
        .unwrap()
    {
        steps += 1;
        assert!(steps < 20);
    }
    let r = row(&db, tenant, id).await;
    assert_eq!((r.status.as_str(), r.attempts), ("failed", 8));

    smtp.set_mode(Mode::Reject);
    let id2 = enqueue(
        &db,
        tenant,
        "k2",
        Stream::Transactional,
        "gone@example.test",
    )
    .await;
    assert_eq!(
        notifications::deliver(&db, &smtp.mailer(), None, tenant, id2)
            .await
            .unwrap(),
        Step::Done
    );
    assert_eq!(row(&db, tenant, id2).await.status, "failed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn messages_and_suppressions_are_tenant_isolated(db: PgPool) {
    let (db, tenant) = setup(db).await;
    let (other, _) = testkit::tenant(&db, "other").await;
    let smtp = FakeSmtp::start(Mode::Accept).await;
    let id = enqueue(&db, tenant, "k1", Stream::Transactional, "a@example.test").await;
    // The other tenant's job context cannot see (or send) it.
    assert_eq!(
        notifications::deliver(&db, &smtp.mailer(), None, other, id)
            .await
            .unwrap(),
        Step::Done
    );
    assert!(smtp.received().is_empty());
    let mut tx = tenant_tx(&db, other).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM email_messages")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    let write = sqlx::query(
        "INSERT INTO email_suppressions (tenant_id, email, reason) VALUES ($1, 'x@example.test', 'manual')",
    )
    .bind(tenant)
    .execute(&mut *tx)
    .await;
    assert!(write.is_err(), "RLS refuses writes for another tenant");
}
