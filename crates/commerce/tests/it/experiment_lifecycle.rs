use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use commerce::experiments::{
    self, Assignment, Change, Experiment, NewChange, NewExperiment, Status, StopReason,
};
use commerce::storefront::pages::{self, Hero, HeroCopy};
use commerce::storefront::{self, PublicUrls};
use commerce::themes;
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

use crate::experiments::{
    arms, draft, published_cookie_page, running, set_status, setup, start, update,
};
use crate::tenancy::db_code;
use crate::theme_arms::{artifact, revision};

const MIGRATION: i64 = 20261029000200;

async fn refused(runtime: &PgPool, tenant: Uuid, sql: &'static str, id: Uuid) -> Option<String> {
    update(runtime, tenant, sql, id)
        .await
        .err()
        .as_ref()
        .and_then(db_code)
}

async fn reason_of(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Option<String> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar("SELECT stop_reason FROM experiments WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn promoted_at(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Option<DateTime<Utc>> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar("SELECT promoted_at FROM experiments WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

fn recent(at: Option<DateTime<Utc>>) -> bool {
    at.is_some_and(|at| (Utc::now() - at).num_minutes().abs() < 5)
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_stop_reason_is_set_once_with_the_stop(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-reason").await;
    let id = running(&runtime, tenant).await;
    let other = draft(&runtime, tenant, "later", 1).await;
    for (sql, target, why) in [
        (
            "UPDATE experiments SET status = 'stopped' WHERE id = $1",
            id,
            "a stop without a reason",
        ),
        (
            "UPDATE experiments SET stop_reason = 'manual' WHERE id = $1",
            id,
            "a reason before the stop",
        ),
        (
            "UPDATE experiments SET stop_reason = 'manual' WHERE id = $1",
            other,
            "a reason on a draft",
        ),
    ] {
        assert_eq!(
            refused(&runtime, tenant, sql, target).await.as_deref(),
            Some("23514"),
            "{why}"
        );
    }

    update(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'stopped', stop_reason = 'manual' WHERE id = $1",
        id,
    )
    .await
    .unwrap();
    for sql in [
        "UPDATE experiments SET stop_reason = 'srm' WHERE id = $1",
        "UPDATE experiments SET stop_reason = NULL WHERE id = $1",
    ] {
        assert_eq!(
            refused(&runtime, tenant, sql, id).await.as_deref(),
            Some("23514"),
            "{sql}"
        );
    }
    assert_eq!(
        reason_of(&runtime, tenant, id).await.as_deref(),
        Some("manual")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_database_stamps_the_promotion_time_once(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-promoted").await;
    let id = running(&runtime, tenant).await;
    for (sql, why) in [
        (
            "UPDATE experiments SET promoted_at = now() WHERE id = $1",
            "promoted while running",
        ),
        (
            "UPDATE experiments SET status = 'stopped', stop_reason = 'promoted' WHERE id = $1",
            "a promoted stop without the time",
        ),
    ] {
        assert_eq!(
            refused(&runtime, tenant, sql, id).await.as_deref(),
            Some("23514"),
            "{why}"
        );
    }

    update(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'stopped', stop_reason = 'promoted',
                promoted_at = '2000-01-01T00:00:00Z' WHERE id = $1",
        id,
    )
    .await
    .unwrap();
    assert!(
        recent(promoted_at(&runtime, tenant, id).await),
        "the database's time, not the caller's"
    );
    for sql in [
        "UPDATE experiments SET promoted_at = now() WHERE id = $1",
        "UPDATE experiments SET promoted_at = NULL WHERE id = $1",
    ] {
        assert_eq!(
            refused(&runtime, tenant, sql, id).await.as_deref(),
            Some("23514"),
            "{sql}"
        );
    }

    let later = draft(&runtime, tenant, "later", 1).await;
    set_status(&runtime, tenant, later, "running").await;
    set_status(&runtime, tenant, later, "stopped").await;
    update(
        &runtime,
        tenant,
        "UPDATE experiments SET promoted_at = '2000-01-01T00:00:00Z' WHERE id = $1",
        later,
    )
    .await
    .unwrap();
    assert!(recent(promoted_at(&runtime, tenant, later).await));
    assert_eq!(
        reason_of(&runtime, tenant, later).await.as_deref(),
        Some("manual"),
        "a promotion after a stop keeps the first reason"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_has_exactly_two_arms(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-arms").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let three = sqlx::query(
        "INSERT INTO experiments (tenant_id, name, version, arms) VALUES ($1, 'three', 1, $2)",
    )
    .bind(tenant)
    .bind(arms(&[("control", 34), ("b", 33), ("c", 33)]))
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(db_code(&three).as_deref(), Some("23514"));
}

#[sqlx::test(migrations = false)]
async fn experiments_stopped_before_the_migration_count_as_stopped_manually(db: PgPool) {
    let migrator = sqlx::migrate!("../../migrations");
    migrator.run_to(MIGRATION - 1, &db).await.unwrap();
    let (runtime, tenant) = setup(&db, "life-backfill").await;
    let stopped = draft(&runtime, tenant, "old", 1).await;
    let live = draft(&runtime, tenant, "live", 1).await;
    for (sql, id) in [
        (
            "UPDATE experiments SET status = 'running' WHERE id = $1",
            stopped,
        ),
        (
            "UPDATE experiments SET status = 'stopped' WHERE id = $1",
            stopped,
        ),
        (
            "UPDATE experiments SET status = 'running' WHERE id = $1",
            live,
        ),
    ] {
        update(&runtime, tenant, sql, id).await.unwrap();
    }

    migrator.run(&db).await.unwrap();
    assert_eq!(
        reason_of(&runtime, tenant, stopped).await.as_deref(),
        Some("manual")
    );
    assert_eq!(reason_of(&runtime, tenant, live).await, None);
}

async fn home_hero(runtime: &PgPool, shop: &Shop, market: Uuid) -> Hero {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap();
    pages::home(&mut tx, &ctx).await.unwrap().hero
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_home_page_shows_the_stored_hero_copy(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "life-home").await;
    let default = home_hero(&runtime, &shop, shop.cz).await;

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("INSERT INTO home_settings (tenant_id, hero) VALUES ($1, $2)")
        .bind(shop.tenant)
        .bind(json!({
            "title": {"cs": "Podzimní výprodej", "sk": "Jesenný výpredaj"},
            "cta": {"cs": "Vybrat si"},
        }))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let cz = home_hero(&runtime, &shop, shop.cz).await;
    assert_eq!(cz.title, "Podzimní výprodej");
    assert_eq!(cz.cta.label, "Vybrat si");
    assert_eq!(cz.subtitle, default.subtitle, "a field the copy lacks");
    assert_eq!(cz.cta.href, default.cta.href);
    assert_eq!(cz.image, default.image);
    let sk = home_hero(&runtime, &shop, shop.sk).await;
    assert_eq!(sk.title, "Jesenný výpredaj");
    assert_eq!(
        sk.cta.label, "Vybrat si",
        "a text without the locale falls back like other i18n texts"
    );
}

const ACTOR: &str = "test";

async fn in_tx<T>(
    runtime: &PgPool,
    tenant: Uuid,
    f: impl AsyncFnOnce(&mut TenantTx) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let out = f(&mut tx).await?;
    tx.commit().await.unwrap();
    Ok(out)
}

fn code<T: std::fmt::Debug>(result: Result<T, Error>) -> &'static str {
    match result.unwrap_err() {
        Error::Validation { code, .. } | Error::Conflict { code, .. } => code,
        e => panic!("{e:?}"),
    }
}

fn texts(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(
        pairs
            .iter()
            .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
            .collect(),
    )
}

fn title(cs: &str) -> HeroCopy {
    HeroCopy {
        title: texts(&[("cs", cs)]),
        subtitle: None,
        cta: None,
    }
}

fn hero_change(name: &str, hero: HeroCopy) -> NewExperiment {
    NewExperiment {
        name: name.into(),
        change: NewChange::Hero { hero },
    }
}

fn theme_change(revision_id: Uuid) -> NewExperiment {
    NewExperiment {
        name: "new-theme".into(),
        change: NewChange::Theme { revision_id },
    }
}

async fn create(runtime: &PgPool, tenant: Uuid, input: NewExperiment) -> Result<Experiment, Error> {
    in_tx(runtime, tenant, async |tx| {
        experiments::create(tx, ACTOR, &input).await
    })
    .await
}

async fn list(runtime: &PgPool, tenant: Uuid) -> Vec<Experiment> {
    in_tx(runtime, tenant, async |tx| experiments::list(tx).await)
        .await
        .unwrap()
}

async fn stop(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Result<(), Error> {
    in_tx(runtime, tenant, async |tx| {
        experiments::stop(tx, ACTOR, id).await
    })
    .await
}

async fn promote(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Result<Experiment, Error> {
    in_tx(runtime, tenant, async |tx| {
        experiments::promote(tx, ACTOR, id).await
    })
    .await
}

async fn started(runtime: &PgPool, tenant: Uuid, input: NewExperiment) -> Experiment {
    let e = create(runtime, tenant, input).await.unwrap();
    start(runtime, tenant, e.id).await.unwrap();
    e
}

async fn status_of(runtime: &PgPool, tenant: Uuid, id: Uuid) -> Experiment {
    list(runtime, tenant)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .unwrap()
}

async fn active_revision(runtime: &PgPool, tenant: Uuid) -> Option<Uuid> {
    in_tx(runtime, tenant, async |tx| themes::list(tx).await)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.active)
        .map(|r| r.id)
}

async fn changed(db: &PgPool, tenant: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = $2")
        .bind(tenant)
        .bind(experiments::CHANGED_EVENT)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn audits(runtime: &PgPool, tenant: Uuid, action: &str) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_numbers_the_versions_of_each_name_and_lists_the_newest_first(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-create").await;
    let first = create(
        &runtime,
        tenant,
        hero_change("homepage", title("Nová kolekce")),
    )
    .await
    .unwrap();
    let second = create(&runtime, tenant, hero_change("homepage", title("Podzim")))
        .await
        .unwrap();
    let other = create(
        &runtime,
        tenant,
        hero_change("checkout", title("Nová kolekce")),
    )
    .await
    .unwrap();

    assert_eq!((first.version, second.version, other.version), (1, 2, 1));
    assert_eq!(second.status, Status::Draft);
    assert_eq!(
        (second.stop_reason, second.started_at, second.promoted_at),
        (None, None, None)
    );
    assert_eq!(
        second.change,
        Change::Hero {
            hero: title("Podzim")
        }
    );
    let listed = list(&runtime, tenant).await;
    assert_eq!(
        listed
            .iter()
            .map(|e| (e.name.as_str(), e.version))
            .collect::<Vec<_>>(),
        [("checkout", 1), ("homepage", 2), ("homepage", 1)]
    );
    assert_eq!(audits(&runtime, tenant, "experiment.created").await, 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_refuses_a_bad_name_or_hero_copy(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-invalid").await;
    let none = HeroCopy {
        title: None,
        subtitle: None,
        cta: None,
    };
    let long = "x".repeat(201);
    for (input, expected, why) in [
        (
            hero_change("Homepage", title("Ahoj")),
            "invalid_experiment_name",
            "an uppercase name",
        ),
        (
            hero_change("-homepage", title("Ahoj")),
            "invalid_experiment_name",
            "a name starting with a dash",
        ),
        (
            hero_change("homepage", none.clone()),
            "invalid_experiment_config",
            "a copy that changes nothing",
        ),
        (
            hero_change(
                "homepage",
                HeroCopy {
                    subtitle: texts(&[]),
                    ..none.clone()
                },
            ),
            "invalid_experiment_config",
            "a text in no locale",
        ),
        (
            hero_change(
                "homepage",
                HeroCopy {
                    cta: texts(&[("CZ", "Koupit")]),
                    ..none.clone()
                },
            ),
            "invalid_experiment_config",
            "a bad locale",
        ),
        (
            hero_change("homepage", title(&long)),
            "invalid_experiment_config",
            "a text over 200 characters",
        ),
    ] {
        assert_eq!(
            code(create(&runtime, tenant, input).await),
            expected,
            "{why}"
        );
    }
    assert!(list(&runtime, tenant).await.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_theme_change_needs_a_ready_revision_of_the_shop(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-theme").await;
    let (other, _) = testkit::tenant(&runtime, "life-theme-other").await;
    for id in [
        "f1000000000000000000000000000001",
        "f1000000000000000000000000000002",
        "f1000000000000000000000000000003",
    ] {
        artifact(&db, id).await;
    }
    let ready = revision(
        &runtime,
        tenant,
        1,
        "ready",
        Some("f1000000000000000000000000000001"),
    )
    .await;
    let failed = revision(
        &runtime,
        tenant,
        2,
        "failed",
        Some("f1000000000000000000000000000002"),
    )
    .await;
    let building = revision(&runtime, tenant, 3, "building", None).await;
    let foreign = revision(
        &runtime,
        other,
        1,
        "ready",
        Some("f1000000000000000000000000000003"),
    )
    .await;
    for (rev, expected, why) in [
        (foreign, "unknown_theme_revision", "another shop's revision"),
        (Uuid::now_v7(), "unknown_theme_revision", "no such revision"),
        (failed, "theme_revision_not_ready", "a revision that failed"),
        (
            building,
            "theme_revision_not_ready",
            "a revision without an artifact",
        ),
    ] {
        assert_eq!(
            code(create(&runtime, tenant, theme_change(rev)).await),
            expected,
            "{why}"
        );
    }

    let e = create(&runtime, tenant, theme_change(ready)).await.unwrap();
    assert_eq!(
        e.change,
        Change::Theme {
            revision_id: ready,
            revision_number: 1
        }
    );
    published_cookie_page(&runtime, tenant).await;
    start(&runtime, tenant, e.id).await.unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let arm = experiments::running(&mut tx)
        .await
        .unwrap()
        .and_then(|r| r.theme_arm)
        .map(|a| a.arm);
    assert_eq!(arm.as_deref(), Some("variant"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_theme_draft_whose_revision_lost_its_artifact_does_not_start(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-gc").await;
    published_cookie_page(&runtime, tenant).await;
    artifact(&db, "f2000000000000000000000000000001").await;
    let rev = revision(
        &runtime,
        tenant,
        1,
        "ready",
        Some("f2000000000000000000000000000001"),
    )
    .await;
    let e = create(&runtime, tenant, theme_change(rev)).await.unwrap();
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query("UPDATE theme_revisions SET artifact_id = NULL, status = 'failed' WHERE id = $1")
        .bind(rev)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        code(start(&runtime, tenant, e.id).await),
        "theme_revision_not_ready"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn stop_ends_a_running_experiment_once(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-stop").await;
    published_cookie_page(&runtime, tenant).await;
    let e = create(
        &runtime,
        tenant,
        hero_change("homepage", title("Nová kolekce")),
    )
    .await
    .unwrap();
    assert_eq!(
        code(stop(&runtime, tenant, e.id).await),
        "experiment_not_running"
    );

    start(&runtime, tenant, e.id).await.unwrap();
    stop(&runtime, tenant, e.id).await.unwrap();
    let stopped = list(&runtime, tenant).await.remove(0);
    assert_eq!(
        (stopped.status, stopped.stop_reason),
        (Status::Stopped, Some(StopReason::Manual))
    );
    assert!(stopped.stopped_at.is_some());
    stop(&runtime, tenant, e.id).await.unwrap();
    assert_eq!(
        list(&runtime, tenant).await.remove(0),
        stopped,
        "a second stop changes nothing"
    );
    assert_eq!(audits(&runtime, tenant, "experiment.stopped").await, 1);
    assert_eq!(changed(&db, tenant).await, 1);
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    assert_eq!(experiments::running(&mut tx).await.unwrap(), None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn promoting_a_theme_experiment_publishes_its_revision_and_stops_it(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-promote-theme").await;
    published_cookie_page(&runtime, tenant).await;
    for id in [
        "f3000000000000000000000000000001",
        "f3000000000000000000000000000002",
    ] {
        artifact(&db, id).await;
    }
    let live = revision(
        &runtime,
        tenant,
        1,
        "ready",
        Some("f3000000000000000000000000000001"),
    )
    .await;
    in_tx(&runtime, tenant, async |tx| {
        themes::publish(tx, ACTOR, live).await
    })
    .await
    .unwrap();
    let arm = revision(
        &runtime,
        tenant,
        2,
        "ready",
        Some("f3000000000000000000000000000002"),
    )
    .await;
    let e = started(&runtime, tenant, theme_change(arm)).await;

    let promoted = promote(&runtime, tenant, e.id).await.unwrap();
    assert_eq!(
        (promoted.status, promoted.stop_reason),
        (Status::Stopped, Some(StopReason::Promoted))
    );
    assert!(promoted.promoted_at.is_some());
    assert_eq!(active_revision(&runtime, tenant).await, Some(arm));
    assert_eq!(
        promote(&runtime, tenant, e.id).await.unwrap(),
        promoted,
        "a second promote is a replay"
    );
    for (action, n) in [
        ("theme.published", 2),
        ("experiment.stopped", 1),
        ("experiment.promoted", 1),
    ] {
        assert_eq!(audits(&runtime, tenant, action).await, n, "{action}");
    }
    assert_eq!(changed(&db, tenant).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_failed_publish_leaves_the_theme_experiment_running(db: PgPool) {
    let (runtime, unpublishable) = setup(&db, "life-unpublishable").await;
    let (unaccepted, _) = testkit::tenant(&runtime, "life-unaccepted").await;
    for id in [
        "f4000000000000000000000000000001",
        "f4000000000000000000000000000002",
        "f4000000000000000000000000000003",
    ] {
        artifact(&db, id).await;
    }

    let stripped = revision(
        &runtime,
        unpublishable,
        1,
        "ready",
        Some("f4000000000000000000000000000001"),
    )
    .await;
    published_cookie_page(&runtime, unpublishable).await;
    let first = started(&runtime, unpublishable, theme_change(stripped)).await;
    let mut tx = tenant_tx(&runtime, unpublishable).await.unwrap();
    sqlx::query("UPDATE theme_revisions SET artifact_id = NULL, status = 'failed' WHERE id = $1")
        .bind(stripped)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let base = revision(
        &runtime,
        unaccepted,
        1,
        "ready",
        Some("f4000000000000000000000000000002"),
    )
    .await;
    let mut tx = tenant_tx(&runtime, unaccepted).await.unwrap();
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO ai_theme_runs (tenant_id, base_revision_id, prompt, status, created_by)
         VALUES ($1, $2, 'Tmavší hlavička', 'succeeded', 'o') RETURNING id",
    )
    .bind(unaccepted)
    .bind(base)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let ai: Uuid = sqlx::query_scalar(
        "INSERT INTO theme_revisions (tenant_id, number, artifact_id, origin, change, source_key,
                                      status, created_by, ai_run_id)
         VALUES ($1, 2, 'f4000000000000000000000000000003', 'custom', 'ai',
                 'theme-sources/ai.tar.gz', 'ready', 'o', $2) RETURNING id",
    )
    .bind(unaccepted)
    .bind(run)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    published_cookie_page(&runtime, unaccepted).await;
    let second = started(&runtime, unaccepted, theme_change(ai)).await;

    for (tenant, id, expected) in [
        (unpublishable, first.id, "not_publishable"),
        (unaccepted, second.id, "ai_run_not_accepted"),
    ] {
        assert_eq!(code(promote(&runtime, tenant, id).await), expected);
        let e = status_of(&runtime, tenant, id).await;
        assert_eq!(
            (e.status, e.stop_reason, e.promoted_at),
            (Status::Running, None, None),
            "{expected}"
        );
        assert_eq!(active_revision(&runtime, tenant).await, None, "{expected}");
        for action in [
            "theme.published",
            "experiment.stopped",
            "experiment.promoted",
        ] {
            assert_eq!(
                audits(&runtime, tenant, action).await,
                0,
                "{expected}: {action}"
            );
        }
        assert_eq!(changed(&db, tenant).await, 0, "{expected}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn promoting_hero_experiments_merges_their_copy_into_the_home_page(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "life-promote-hero").await;
    published_cookie_page(&runtime, shop.tenant).await;
    let default = home_hero(&runtime, &shop, shop.cz).await;
    let first = HeroCopy {
        title: texts(&[("cs", "Podzimní výprodej"), ("sk", "Jesenný výpredaj")]),
        subtitle: None,
        cta: texts(&[("cs", "Vybrat si")]),
    };
    let e = started(&runtime, shop.tenant, hero_change("homepage", first)).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let variant = Assignment {
        experiment_id: e.id,
        version: e.version,
        arm: "variant".into(),
    };
    assert!(
        experiments::arm_config(&mut tx, &variant)
            .await
            .unwrap()
            .is_some()
    );
    drop(tx);

    let promoted = promote(&runtime, shop.tenant, e.id).await.unwrap();
    assert_eq!(
        (promoted.status, promoted.stop_reason),
        (Status::Stopped, Some(StopReason::Promoted))
    );
    let cz = home_hero(&runtime, &shop, shop.cz).await;
    assert_eq!(
        (cz.title.as_str(), cz.cta.label.as_str()),
        ("Podzimní výprodej", "Vybrat si")
    );
    assert_eq!(cz.subtitle, default.subtitle);
    assert_eq!(
        home_hero(&runtime, &shop, shop.sk).await.title,
        "Jesenný výpredaj"
    );

    let second = HeroCopy {
        title: texts(&[("cs", "Zimní kolekce")]),
        subtitle: texts(&[("cs", "Doprava zdarma")]),
        cta: None,
    };
    let e = started(&runtime, shop.tenant, hero_change("homepage", second)).await;
    promote(&runtime, shop.tenant, e.id).await.unwrap();
    let cz = home_hero(&runtime, &shop, shop.cz).await;
    assert_eq!(
        (
            cz.title.as_str(),
            cz.subtitle.as_str(),
            cz.cta.label.as_str()
        ),
        ("Zimní kolekce", "Doprava zdarma", "Vybrat si"),
        "the second copy replaces the fields it has"
    );
    assert_eq!(changed(&db, shop.tenant).await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn only_a_started_hero_or_theme_experiment_is_promoted(db: PgPool) {
    let (runtime, tenant) = setup(&db, "life-not-promotable").await;
    published_cookie_page(&runtime, tenant).await;
    let unstarted = create(&runtime, tenant, hero_change("homepage", title("Ahoj")))
        .await
        .unwrap();
    assert_eq!(
        code(promote(&runtime, tenant, unstarted.id).await),
        "experiment_not_started"
    );

    let component = draft(&runtime, tenant, "components", 1).await;
    start(&runtime, tenant, component).await.unwrap();
    assert_eq!(
        code(promote(&runtime, tenant, component).await),
        "experiment_not_promotable"
    );
    let e = status_of(&runtime, tenant, component).await;
    assert_eq!((e.change, e.status), (Change::Component, Status::Running));
    assert_eq!(audits(&runtime, tenant, "experiment.promoted").await, 0);
    assert_eq!(changed(&db, tenant).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_hero_experiment_stopped_by_hand_can_still_be_promoted(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "life-late").await;
    published_cookie_page(&runtime, shop.tenant).await;
    let e = started(
        &runtime,
        shop.tenant,
        hero_change("homepage", title("Poslední kusy")),
    )
    .await;
    stop(&runtime, shop.tenant, e.id).await.unwrap();

    let promoted = promote(&runtime, shop.tenant, e.id).await.unwrap();
    assert_eq!(
        (promoted.status, promoted.stop_reason),
        (Status::Stopped, Some(StopReason::Manual))
    );
    assert!(promoted.promoted_at.is_some());
    assert_eq!(
        home_hero(&runtime, &shop, shop.cz).await.title,
        "Poslední kusy"
    );
    assert_eq!(
        promote(&runtime, shop.tenant, e.id).await.unwrap(),
        promoted,
        "promoted once"
    );
    assert_eq!(
        audits(&runtime, shop.tenant, "experiment.promoted").await,
        1
    );
    assert_eq!(
        changed(&db, shop.tenant).await,
        2,
        "the stop and the promotion"
    );
}
