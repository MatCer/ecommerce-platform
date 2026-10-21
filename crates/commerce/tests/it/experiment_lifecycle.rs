use chrono::{DateTime, Utc};
use platform::db::tenant_tx;
use sqlx::PgPool;
use uuid::Uuid;

use crate::experiments::{arms, draft, running, set_status, setup, update};
use crate::tenancy::db_code;

const MIGRATION: i64 = 20261029000100;

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
