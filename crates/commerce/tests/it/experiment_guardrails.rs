use chrono::{DateTime, Utc};
use commerce::experiments::{self, GuardrailReport};
use platform::db::tenant_tx;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::experiment_readout::{assign, cohort, day, experiment, order, t0};

async fn run(runtime: &PgPool, now: DateTime<Utc>) -> GuardrailReport {
    experiments::guardrails(runtime, now).await.unwrap()
}

/// `(stop_reason, [(actor, diff)] of the stop audits, experiment.changed events)`
async fn outcome(
    db: &PgPool,
    runtime: &PgPool,
    tenant: Uuid,
    id: Uuid,
) -> (Option<String>, Vec<(String, Value)>, i64) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let reason = sqlx::query_scalar("SELECT stop_reason FROM experiments WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let audits = sqlx::query_as(
        "SELECT actor, diff FROM audit_log
         WHERE action = 'experiment.stopped' AND entity_id = $1 ORDER BY id",
    )
    .bind(id.to_string())
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    let changed =
        sqlx::query_scalar("SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = $2")
            .bind(tenant)
            .bind(experiments::CHANGED_EVENT)
            .fetch_one(db)
            .await
            .unwrap();
    (reason, audits, changed)
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_sample_ratio_mismatch_stops_the_experiment_once(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "guard-srm").await;
    let t = t0();
    let id = experiment(&runtime, tenant, "homepage", t, None).await;
    assign(&runtime, tenant, id, "control", 110, t + day(0.1)).await;
    assign(&runtime, tenant, id, "variant", 40, t + day(0.1)).await;

    assert_eq!(
        run(&runtime, t + day(2.0)).await,
        GuardrailReport { stopped: 1 }
    );
    let (reason, audits, changed) = outcome(&db, &runtime, tenant, id).await;
    assert_eq!(reason.as_deref(), Some("srm"));
    assert_eq!(changed, 1);
    let [(actor, diff)] = audits.as_slice() else {
        panic!("{audits:?}");
    };
    assert_eq!(actor, experiments::GUARDRAIL_ACTOR);
    assert_eq!(diff["reason"], "srm");
    let chi2 = diff["chi2"].as_f64().unwrap();
    assert!((chi2 - 2.0 * 35.0 * 35.0 / 75.0).abs() < 1e-9, "{chi2}");

    assert_eq!(
        run(&runtime, t + day(3.0)).await,
        GuardrailReport { stopped: 0 },
        "a second run changes nothing"
    );
    let (reason, audits, changed) = outcome(&db, &runtime, tenant, id).await;
    assert_eq!(
        (reason.as_deref(), audits.len(), changed),
        (Some("srm"), 1, 1)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_clearly_worse_variant_stops_after_14_mature_days(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "guard-harm").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = t0();
    let id = experiment(&runtime, shop.tenant, "homepage", t, None).await;
    cohort(
        &runtime,
        &shop,
        id,
        "control",
        (200, 20),
        t + day(0.1),
        paid,
    )
    .await;
    cohort(&runtime, &shop, id, "variant", (200, 2), t + day(0.1), paid).await;

    assert_eq!(
        run(&runtime, t + day(20.5)).await,
        GuardrailReport { stopped: 0 },
        "13 mature days"
    );
    assert_eq!(
        run(&runtime, t + day(21.0)).await,
        GuardrailReport { stopped: 1 }
    );
    let (reason, audits, _) = outcome(&db, &runtime, shop.tenant, id).await;
    assert_eq!(reason.as_deref(), Some("harm"));
    assert!(
        audits[0].1["p_worse"].as_f64().unwrap() > 0.99,
        "{audits:?}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_stops_after_56_days(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "guard-duration").await;
    let t = t0();
    let id = experiment(&runtime, tenant, "homepage", t, None).await;

    assert_eq!(
        run(&runtime, t + day(55.9)).await,
        GuardrailReport { stopped: 0 }
    );
    assert_eq!(
        run(&runtime, t + day(57.0)).await,
        GuardrailReport { stopped: 1 }
    );
    let (reason, audits, changed) = outcome(&db, &runtime, tenant, id).await;
    assert_eq!(
        (reason.as_deref(), audits.len(), changed),
        (Some("max_duration"), 1, 1)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_the_admin_stopped_first_is_left_alone(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "guard-manual").await;
    let t = t0();
    let id = experiment(&runtime, tenant, "homepage", t, None).await;
    assign(&runtime, tenant, id, "control", 110, t + day(0.1)).await;
    assign(&runtime, tenant, id, "variant", 40, t + day(0.1)).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    experiments::stop(&mut tx, "admin", id).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        run(&runtime, t + day(57.0)).await,
        GuardrailReport { stopped: 0 }
    );
    let (reason, audits, changed) = outcome(&db, &runtime, tenant, id).await;
    assert_eq!(
        (reason.as_deref(), audits.len(), changed),
        (Some("manual"), 1, 1)
    );
    assert_eq!(audits[0].0, "admin");
}
