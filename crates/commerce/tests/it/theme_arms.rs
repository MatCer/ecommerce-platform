use commerce::experiments::{ExperimentRef, ThemeArm};
use commerce::{tenancy, themes};
use platform::db::tenant_tx;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn arms(keys: &[&str]) -> Value {
    keys.iter()
        .map(|k| json!({"key": k, "weight": 50, "config": {}}))
        .collect()
}

async fn artifact(db: &PgPool, id: &str) {
    sqlx::query("INSERT INTO platform.theme_artifacts (id, kind) VALUES ($1, 'theme')")
        .bind(id)
        .execute(db)
        .await
        .unwrap();
}

async fn revision(
    runtime: &PgPool,
    tenant: Uuid,
    number: i32,
    status: &str,
    artifact: Option<&str>,
) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO theme_revisions (tenant_id, number, artifact_id, origin, change, source_key,
                                      status, created_by)
         VALUES ($1, $2, $3, 'custom', 'fork', 'theme-sources/x.tar.gz', $4, 'o') RETURNING id",
    )
    .bind(tenant)
    .bind(number)
    .bind(artifact)
    .bind(status)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn insert(
    runtime: &PgPool,
    tenant: Uuid,
    name: &str,
    keys: &[&str],
    arm: Option<&str>,
    rev: Option<Uuid>,
    status: &str,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO experiments (tenant_id, name, version, arms, theme_arm, theme_revision_id,
                                  status, started_at)
         VALUES ($1, $2, 1, $3, $4, $5, $6, CASE WHEN $6 = 'running' THEN now() END)
         RETURNING id",
    )
    .bind(tenant)
    .bind(name)
    .bind(arms(keys))
    .bind(arm)
    .bind(rev)
    .bind(status)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn exec(
    runtime: &PgPool,
    tenant: Uuid,
    sql: &'static str,
    id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    tx.commit().await
}

async fn resolved(runtime: &PgPool, host: &str) -> Option<ExperimentRef> {
    tenancy::resolve_host(runtime, host)
        .await
        .unwrap()
        .unwrap()
        .experiment
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolve_names_the_running_experiment_and_its_theme_arm(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "arm-resolve").await;
    let host = "arm-resolve.localhost";
    let new_theme = "b0000000000000000000000000000001";
    artifact(&db, new_theme).await;
    let rev = revision(&runtime, shop.tenant, 7, "ready", Some(new_theme)).await;

    let copy = insert(
        &runtime,
        shop.tenant,
        "hero-copy",
        &["control", "b"],
        None,
        None,
        "running",
    )
    .await
    .unwrap();
    assert_eq!(
        resolved(&runtime, host).await,
        Some(ExperimentRef {
            id: copy,
            version: 1,
            name: "hero-copy".into(),
            theme_arm: None,
        })
    );
    exec(
        &runtime,
        shop.tenant,
        "UPDATE experiments SET status = 'stopped' WHERE id = $1",
        copy,
    )
    .await
    .unwrap();

    let id = insert(
        &runtime,
        shop.tenant,
        "new-theme",
        &["control", "next"],
        Some("next"),
        Some(rev),
        "draft",
    )
    .await
    .unwrap();
    assert_eq!(resolved(&runtime, host).await, None, "draft");
    exec(
        &runtime,
        shop.tenant,
        "UPDATE experiments SET status = 'running' WHERE id = $1",
        id,
    )
    .await
    .unwrap();
    assert_eq!(
        resolved(&runtime, host).await,
        Some(ExperimentRef {
            id,
            version: 1,
            name: "new-theme".into(),
            theme_arm: Some(ThemeArm {
                arm: "next".into(),
                artifact: new_theme.into(),
            }),
        })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_theme_arm_is_the_one_other_arm_and_a_gated_revision_of_the_tenant(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "arm-cap").await;
    let (other, _) = testkit::tenant(&runtime, "arm-cap-other").await;
    for id in [
        "c0000000000000000000000000000001",
        "c0000000000000000000000000000002",
        "c0000000000000000000000000000003",
    ] {
        artifact(&db, id).await;
    }
    let ready = revision(
        &runtime,
        tenant,
        1,
        "ready",
        Some("c0000000000000000000000000000001"),
    )
    .await;
    let failed = revision(
        &runtime,
        tenant,
        2,
        "failed",
        Some("c0000000000000000000000000000002"),
    )
    .await;
    let building = revision(&runtime, tenant, 3, "building", None).await;
    let foreign = revision(
        &runtime,
        other,
        1,
        "ready",
        Some("c0000000000000000000000000000003"),
    )
    .await;
    let two = &["control", "b"][..];

    for (why, keys, arm, rev) in [
        ("an arm without a revision", two, Some("b"), None),
        ("a revision without an arm", two, None, Some(ready)),
        (
            "an arm the experiment does not have",
            two,
            Some("c"),
            Some(ready),
        ),
        (
            "a third arm: only control plus one",
            &["control", "b", "c"][..],
            Some("b"),
            Some(ready),
        ),
        ("another tenant's revision", two, Some("b"), Some(foreign)),
    ] {
        assert!(
            insert(&runtime, tenant, "t", keys, arm, rev, "draft")
                .await
                .is_err(),
            "{why}"
        );
    }

    for (name, why, rev) in [
        ("ungated", "an ungated revision", failed),
        ("unbuilt", "a revision with no artifact", building),
    ] {
        assert!(
            insert(&runtime, tenant, name, two, Some("b"), Some(rev), "running")
                .await
                .is_err(),
            "started as {why}"
        );
        let draft = insert(&runtime, tenant, name, two, Some("b"), Some(rev), "draft")
            .await
            .unwrap();
        let start = exec(
            &runtime,
            tenant,
            "UPDATE experiments SET status = 'running' WHERE id = $1",
            draft,
        )
        .await;
        assert!(start.is_err(), "start with {why}");
    }

    let id = insert(
        &runtime,
        tenant,
        "whole-theme",
        two,
        Some("b"),
        Some(ready),
        "draft",
    )
    .await
    .unwrap();
    for sql in [
        "UPDATE experiments SET theme_arm = 'control' WHERE id = $1",
        "UPDATE experiments SET theme_revision_id = NULL, theme_arm = NULL WHERE id = $1",
    ] {
        assert!(
            exec(&runtime, tenant, sql, id).await.is_err(),
            "frozen: {sql}"
        );
    }
    exec(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'running' WHERE id = $1",
        id,
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_running_theme_arm_keeps_its_artifact_through_gc(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let storage = testkit::memory_storage();
    let (tenant, _) = testkit::tenant(&runtime, "arm-gc").await;
    let arm = "d0000000000000000000000000000001";
    artifact(&db, arm).await;
    let rev = revision(&runtime, tenant, 1, "ready", Some(arm)).await;
    for n in 2..=6 {
        let id = format!("d00000000000000000000000000000{n:02}");
        artifact(&db, &id).await;
        revision(&runtime, tenant, n, "ready", Some(&id)).await;
    }
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    sqlx::query("UPDATE theme_revisions SET status_changed_at = now() - interval '30 days'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE platform.theme_artifacts SET created_at = now() - interval '30 days'")
        .execute(&db)
        .await
        .unwrap();
    let id = insert(
        &runtime,
        tenant,
        "whole-theme",
        &["control", "b"],
        Some("b"),
        Some(rev),
        "running",
    )
    .await
    .unwrap();
    let artifact_of = || async {
        let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT artifact_id FROM theme_revisions WHERE id = $1",
        )
        .bind(rev)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
    };

    let report = themes::maintenance(&runtime, &storage).await.unwrap();
    assert_eq!(
        report.released_revisions, 0,
        "the running arm's surplus ready revision stays"
    );
    assert_eq!(artifact_of().await.as_deref(), Some(arm));
    assert!(themes::artifact_exists(&runtime, arm).await.unwrap());

    exec(
        &runtime,
        tenant,
        "UPDATE experiments SET status = 'stopped' WHERE id = $1",
        id,
    )
    .await
    .unwrap();
    let report = themes::maintenance(&runtime, &storage).await.unwrap();
    assert_eq!(report.released_revisions, 1);
    assert_eq!(report.deleted_artifacts, vec![arm.to_owned()]);
    assert_eq!(artifact_of().await, None);
}
