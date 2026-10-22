use commerce::consent::{self, ConsentChoice, Purposes, Source, Subject};
use commerce::content::legal::{self, InstallInput};
use commerce::content::{self, Block, LegalType, Page, PageInput, PageStatus};
use commerce::experiments::{self, Assignment, ExperimentKey};
use commerce::tenancy;
use platform::Error;
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const SUBJECT: &str = "0123456789abcdef0123456789abcdef";

fn key() -> ExperimentKey {
    ExperimentKey::new(b"test-experiments-secret-0123456789abcdef")
}

pub(crate) async fn setup(db: &PgPool, slug: &str) -> (PgPool, Uuid) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, slug).await;
    (runtime, tenant)
}

pub(crate) fn arms(weights: &[(&str, u32)]) -> serde_json::Value {
    weights
        .iter()
        .map(|(k, w)| json!({"key": k, "weight": w, "config": {}}))
        .collect()
}

pub(crate) async fn draft(runtime: &PgPool, tenant: Uuid, name: &str, version: i32) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO experiments (tenant_id, name, version, arms) VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(tenant)
    .bind(name)
    .bind(version)
    .bind(arms(&[("control", 50), ("b", 50)]))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

pub(crate) async fn update(
    runtime: &PgPool,
    tenant: Uuid,
    sql: &'static str,
    id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    tx.commit().await
}

pub(crate) async fn set_status(runtime: &PgPool, tenant: Uuid, id: Uuid, status: &'static str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "UPDATE experiments SET status = $2,
                stop_reason = CASE WHEN $2 = 'stopped' THEN 'manual' END
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

const ACTOR: &str = "test";

async fn install_cookie_page(runtime: &PgPool, tenant: Uuid) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let installed = legal::install(
        &mut tx,
        ACTOR,
        &InstallInput {
            locales: vec![],
            types: vec![LegalType::Cookies],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    installed.created[0]
}

async fn edit_page(
    runtime: &PgPool,
    tenant: Uuid,
    id: Uuid,
    still_covers_experiments: bool,
    change: impl FnOnce(&mut PageInput),
) -> Page {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let p = content::get(&mut tx, id).await.unwrap();
    let mut input = PageInput {
        kind: p.kind,
        legal_type: p.legal_type,
        status: p.status,
        published_at: p.published_at,
        image_asset_id: p.image_asset_id,
        translations: p.translations,
        still_covers_experiments,
    };
    change(&mut input);
    let page = content::update(&mut tx, ACTOR, id, &input).await.unwrap();
    tx.commit().await.unwrap();
    page
}

async fn publish(runtime: &PgPool, tenant: Uuid, id: Uuid, status: PageStatus) -> Page {
    edit_page(runtime, tenant, id, false, |p| p.status = status).await
}

async fn add_paragraph(runtime: &PgPool, tenant: Uuid, id: Uuid, confirmed: bool) -> Page {
    edit_page(runtime, tenant, id, confirmed, |p| {
        p.translations[0].blocks.push(Block::RichText {
            html: "<p>Doplněno obchodníkem.</p>".into(),
        });
    })
    .await
}

pub(crate) async fn published_cookie_page(runtime: &PgPool, tenant: Uuid) -> Uuid {
    let id = install_cookie_page(runtime, tenant).await;
    publish(runtime, tenant, id, PageStatus::Published).await;
    id
}

pub(crate) async fn start(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Result<(), Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    experiments::start(&mut tx, ACTOR, id).await?;
    tx.commit().await.unwrap();
    Ok(())
}

async fn sql(runtime: &PgPool, tenant: Uuid, sql: &'static str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(sql).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

fn outdated(result: Result<(), Error>) -> bool {
    matches!(
        result,
        Err(Error::Validation {
            code: "cookie_policy_outdated",
            ..
        })
    )
}

pub(crate) async fn running(runtime: &PgPool, tenant: Uuid) -> Uuid {
    published_cookie_page(runtime, tenant).await;
    let id = draft(runtime, tenant, "homepage", 1).await;
    start(runtime, tenant, id).await.unwrap();
    id
}

async fn choose(runtime: &PgPool, tenant: Uuid, subject: &str, analytics: bool, version: &str) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    consent::record(
        &mut tx,
        &Subject::Anon(subject.into()),
        &ConsentChoice {
            purposes: Purposes {
                analytics: Some(analytics),
                ..Purposes::default()
            },
            text_version: version.into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn rows(runtime: &PgPool, tenant: Uuid) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM experiment_assignments")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn enrol(runtime: &PgPool, tenant: Uuid, subject: &str) -> Option<Assignment> {
    experiments::enrol(runtime, Some(&key()), tenant, subject)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_a_grant_of_the_experiments_text_enrols_and_the_arm_sticks(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-consent").await;
    let id = running(&runtime, tenant).await;

    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "no consent");
    assert_eq!(rows(&runtime, tenant).await, 0);

    choose(&runtime, tenant, SUBJECT, true, "2026-09-25").await;
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "older text");
    assert_eq!(rows(&runtime, tenant).await, 0);

    choose(&runtime, tenant, SUBJECT, true, consent::TEXT_VERSION).await;
    let first = enrol(&runtime, tenant, SUBJECT).await.unwrap();
    assert_eq!((first.experiment_id, first.version), (id, 1));
    assert!(first.arm == "control" || first.arm == "b", "{}", first.arm);
    assert_eq!(rows(&runtime, tenant).await, 1);
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, Some(first.clone()));
    assert_eq!(rows(&runtime, tenant).await, 1);

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT anon_id FROM experiment_assignments")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    drop(tx);
    assert_eq!(stored, commerce::analytics::anon_id(tenant, SUBJECT));
    assert_ne!(stored, SUBJECT, "never the cookie value");

    choose(&runtime, tenant, SUBJECT, false, consent::TEXT_VERSION).await;
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "withdrawn");
    assert_eq!(rows(&runtime, tenant).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_starts_only_once_the_published_cookie_page_names_testing_page_variants(
    db: PgPool,
) {
    let (runtime, tenant) = setup(&db, "exp-gate").await;
    let id = draft(&runtime, tenant, "homepage", 1).await;
    assert!(
        outdated(start(&runtime, tenant, id).await),
        "no cookie page"
    );

    let page = install_cookie_page(&runtime, tenant).await;
    assert!(outdated(start(&runtime, tenant, id).await), "unpublished");

    let published = publish(&runtime, tenant, page, PageStatus::Published).await;
    assert_eq!(
        published.legal_template_version.as_deref(),
        Some(consent::FIRST_EXPERIMENTS_TEXT_VERSION),
        "publishing the installed template keeps its version"
    );
    let confirmed = add_paragraph(&runtime, tenant, page, true).await;
    assert_eq!(
        confirmed.legal_template_version.as_deref(),
        Some(consent::FIRST_EXPERIMENTS_TEXT_VERSION),
        "edited with confirmation"
    );

    sql(
        &runtime,
        tenant,
        "UPDATE pages SET legal_template_version = '2026-09-25'",
    )
    .await;
    assert!(
        outdated(start(&runtime, tenant, id).await),
        "older template"
    );
    sql(
        &runtime,
        tenant,
        "UPDATE pages SET legal_template_version = '2026-10-16'",
    )
    .await;

    sql(&runtime, tenant, "UPDATE markets SET locales = '{cs,sk}'").await;
    assert!(
        outdated(start(&runtime, tenant, id).await),
        "a market locale the page lacks"
    );
    sql(&runtime, tenant, "UPDATE markets SET locales = '{cs}'").await;

    let edited = add_paragraph(&runtime, tenant, page, false).await;
    assert_eq!(edited.legal_template_version, None);
    assert!(
        outdated(start(&runtime, tenant, id).await),
        "edited without confirmation"
    );

    sql(&runtime, tenant, "DELETE FROM pages").await;
    published_cookie_page(&runtime, tenant).await;
    start(&runtime, tenant, id).await.unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    assert_eq!(
        experiments::running(&mut tx).await.unwrap().map(|e| e.id),
        Some(id)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn enrolment_stops_when_the_cookie_page_stops_covering_experiments(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-gate-enrol").await;
    let page = published_cookie_page(&runtime, tenant).await;
    let id = draft(&runtime, tenant, "homepage", 1).await;
    start(&runtime, tenant, id).await.unwrap();
    choose(&runtime, tenant, SUBJECT, true, consent::TEXT_VERSION).await;
    assert!(enrol(&runtime, tenant, SUBJECT).await.is_some());

    publish(&runtime, tenant, page, PageStatus::Draft).await;
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "unpublished");

    publish(&runtime, tenant, page, PageStatus::Published).await;
    assert!(enrol(&runtime, tenant, SUBJECT).await.is_some());
    add_paragraph(&runtime, tenant, page, false).await;
    assert_eq!(
        enrol(&runtime, tenant, SUBJECT).await,
        None,
        "marker cleared"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_stored_arm_survives_a_key_rotation(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-rotation").await;
    running(&runtime, tenant).await;
    let subjects: Vec<String> = (0..16).map(|i| format!("{i:032x}")).collect();
    let mut first = Vec::new();
    for s in &subjects {
        choose(&runtime, tenant, s, true, consent::TEXT_VERSION).await;
        first.push(enrol(&runtime, tenant, s).await.unwrap().arm);
    }
    let other = ExperimentKey::new(b"another-experiments-secret-fedcba9876543210");
    for (s, arm) in subjects.iter().zip(&first) {
        let again = experiments::enrol(&runtime, Some(&other), tenant, s)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&again.arm, arm);
    }
    assert_eq!(rows(&runtime, tenant).await, 16);
}

#[sqlx::test(migrations = "../../migrations")]
async fn nobody_is_enrolled_without_a_running_experiment_or_a_key(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-none").await;
    choose(&runtime, tenant, SUBJECT, true, consent::TEXT_VERSION).await;
    assert_eq!(
        enrol(&runtime, tenant, SUBJECT).await,
        None,
        "no experiment"
    );

    let id = draft(&runtime, tenant, "homepage", 1).await;
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "draft");

    set_status(&runtime, tenant, id, "running").await;
    assert_eq!(
        experiments::enrol(&runtime, None, tenant, SUBJECT)
            .await
            .unwrap(),
        None,
        "no EXPERIMENTS_SECRET"
    );
    assert_eq!(rows(&runtime, tenant).await, 0);

    set_status(&runtime, tenant, id, "stopped").await;
    assert_eq!(enrol(&runtime, tenant, SUBJECT).await, None, "stopped");
    assert_eq!(rows(&runtime, tenant).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_malformed_subject_is_refused(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-subject").await;
    running(&runtime, tenant).await;
    for bad in [
        "",
        "short",
        "0123456789ABCDEF0123456789ABCDEF",
        &format!("{SUBJECT}0"),
    ] {
        let err = experiments::enrol(&runtime, Some(&key()), tenant, bad)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                platform::Error::Validation {
                    code: "invalid_subject",
                    ..
                }
            ),
            "{bad}: {err:?}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn arms_split_visitors_by_weight(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-split").await;
    running(&runtime, tenant).await;
    let mut control = 0;
    for i in 0..400u32 {
        let s = format!("{:032x}", u128::from(i) * 0x9e37_79b9);
        choose(&runtime, tenant, &s, true, consent::TEXT_VERSION).await;
        if enrol(&runtime, tenant, &s).await.unwrap().arm == "control" {
            control += 1;
        }
    }
    assert!(
        (150..=250).contains(&control),
        "control got {control} of 400"
    );
    assert_eq!(rows(&runtime, tenant).await, 400);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_started_experiment_is_frozen(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-frozen").await;
    let id = running(&runtime, tenant).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let (started, stopped): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as("SELECT started_at, stopped_at FROM experiments WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    drop(tx);
    assert!(started.is_some() && stopped.is_none());

    for sql in [
        r#"UPDATE experiments SET arms = '[{"key":"control","weight":10,"config":{}},{"key":"b","weight":90,"config":{}}]' WHERE id = $1"#,
        "UPDATE experiments SET name = 'renamed' WHERE id = $1",
        "UPDATE experiments SET version = 2 WHERE id = $1",
        "UPDATE experiments SET status = 'draft' WHERE id = $1",
    ] {
        assert!(update(&runtime, tenant, sql, id).await.is_err(), "{sql}");
    }

    let other = draft(&runtime, tenant, "checkout", 1).await;
    let second = update(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'running' WHERE id = $1",
        other,
    )
    .await;
    assert!(second.is_err(), "a second running experiment");
    let skip = update(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'stopped' WHERE id = $1",
        other,
    )
    .await;
    assert!(skip.is_err(), "draft -> stopped");

    set_status(&runtime, tenant, id, "stopped").await;
    let restart = update(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'running' WHERE id = $1",
        id,
    )
    .await;
    assert!(restart.is_err(), "stopped -> running");
    set_status(&runtime, tenant, other, "running").await;

    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let one_arm = sqlx::query(
        "INSERT INTO experiments (tenant_id, name, version, arms) VALUES ($1, 'solo', 1, $2)",
    )
    .bind(tenant)
    .bind(arms(&[("control", 100)]))
    .execute(&mut *tx)
    .await;
    assert!(one_arm.is_err(), "one arm");
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenants_cannot_see_each_others_experiments(db: PgPool) {
    let (runtime, a) = setup(&db, "exp-a").await;
    let (b, _) = testkit::tenant(&runtime, "exp-b").await;
    running(&runtime, a).await;
    choose(&runtime, a, SUBJECT, true, consent::TEXT_VERSION).await;
    choose(&runtime, b, SUBJECT, true, consent::TEXT_VERSION).await;
    assert!(enrol(&runtime, a, SUBJECT).await.is_some());

    assert_eq!(enrol(&runtime, b, SUBJECT).await, None);
    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    assert_eq!(
        experiments::running(&mut tx).await.unwrap().map(|e| e.id),
        None
    );
    let seen: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM experiments), (SELECT count(*) FROM experiment_assignments)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(seen, (0, 0));
    let forged = sqlx::query(
        "INSERT INTO experiments (tenant_id, name, version, arms) VALUES ($1, 'forged', 1, $2)",
    )
    .bind(a)
    .bind(arms(&[("control", 50), ("b", 50)]))
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err(), "insert into another tenant");
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_carries_the_running_experiment(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "exp-resolve").await;
    let resolve = || async {
        tenancy::resolve_host(&runtime, "exp-resolve.localhost")
            .await
            .unwrap()
            .unwrap()
            .experiment
            .map(|e| (e.id, e.version))
    };
    assert_eq!(resolve().await, None);
    let id = draft(&runtime, shop.tenant, "homepage", 3).await;
    assert_eq!(resolve().await, None, "draft");
    set_status(&runtime, shop.tenant, id, "running").await;
    assert_eq!(resolve().await, Some((id, 3)));
    set_status(&runtime, shop.tenant, id, "stopped").await;
    assert_eq!(resolve().await, None, "stopped");
}

#[sqlx::test(migrations = "../../migrations")]
async fn assignments_older_than_the_retention_are_purged(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-purge").await;
    let id = running(&runtime, tenant).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO experiment_assignments (tenant_id, experiment_id, anon_id, arm, assigned_at)
         VALUES ($1, $2, repeat('a', 32), 'control', now() - interval '14 months'),
                ($1, $2, repeat('b', 32), 'b', now() - interval '12 months')",
    )
    .bind(tenant)
    .bind(id)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let purged: i64 = sqlx::query_scalar("SELECT platform.purge_experiment_assignments()")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    assert_eq!(rows(&runtime, tenant).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_enrolment_racing_a_withdrawal_writes_nothing_after_it(db: PgPool) {
    let (runtime, tenant) = setup(&db, "exp-race").await;
    running(&runtime, tenant).await;
    choose(&runtime, tenant, SUBJECT, true, consent::TEXT_VERSION).await;

    let mut withdrawal = tenant_tx(&runtime, tenant).await.unwrap();
    consent::record(
        &mut withdrawal,
        &Subject::Anon(SUBJECT.into()),
        &ConsentChoice {
            purposes: Purposes {
                analytics: Some(false),
                ..Purposes::default()
            },
            text_version: consent::TEXT_VERSION.into(),
            source: Source::Banner,
        },
        None,
    )
    .await
    .unwrap();
    let racing = tokio::spawn({
        let runtime = runtime.clone();
        async move { enrol(&runtime, tenant, SUBJECT).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!racing.is_finished(), "enrolment waits for the withdrawal");
    withdrawal.commit().await.unwrap();

    assert_eq!(racing.await.unwrap(), None);
    assert_eq!(rows(&runtime, tenant).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_newsletter_and_a_cart_unsubscribe_of_one_address_do_not_deadlock(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, market) = testkit::tenant(&runtime, "exp-lock").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO subscribers (tenant_id, email, status, locale, market_id, text_version, source)
         VALUES ($1, 'jana@example.cz', 'subscribed', 'cs', $2, '2026-10-16', 'form')",
    )
    .bind(tenant)
    .bind(market)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let email = Subject::Email("jana@example.cz".into());

    let mut newsletter = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query("UPDATE subscribers SET status = 'unsubscribed' WHERE email = 'jana@example.cz'")
        .execute(&mut *newsletter)
        .await
        .unwrap();
    let cart = tokio::spawn({
        let runtime = runtime.clone();
        let email = email.clone();
        async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            consent::record_server(
                &mut tx,
                &email,
                consent::ConsentPurpose::EmailMarketing,
                false,
                consent::TEXT_VERSION,
                "unsubscribe",
                None,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    consent::record_server(
        &mut newsletter,
        &email,
        consent::ConsentPurpose::EmailMarketing,
        false,
        consent::TEXT_VERSION,
        "unsubscribe",
        None,
    )
    .await
    .unwrap();
    newsletter.commit().await.unwrap();
    cart.await.unwrap();
}
