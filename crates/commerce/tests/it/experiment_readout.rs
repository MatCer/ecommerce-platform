use chrono::{DateTime, Duration, NaiveTime, Utc};
use commerce::experiments::{self, ArmReadout, Need, Readout, ReadoutState, Side};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

use crate::experiments::draft;

pub(crate) fn tomorrow() -> DateTime<Utc> {
    (Utc::now() + Duration::days(1))
        .date_naive()
        .and_time(NaiveTime::MIN)
        .and_utc()
}

pub(crate) fn day(d: f64) -> Duration {
    Duration::seconds((d * 86_400.0) as i64)
}

#[derive(Clone, Copy)]
pub(crate) struct Stop<'a> {
    pub at: DateTime<Utc>,
    pub reason: &'a str,
}

pub(crate) async fn experiment(
    runtime: &PgPool,
    tenant: Uuid,
    name: &str,
    started_at: DateTime<Utc>,
    stopped: Option<Stop<'_>>,
) -> Uuid {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO experiments (tenant_id, name, version, arms, status, stop_reason, started_at,
                                  stopped_at)
         VALUES ($1, $2, 1, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tenant)
    .bind(name)
    .bind(json!([
        {"key": "control", "weight": 50, "config": {}},
        {"key": "variant", "weight": 50, "config": {}},
    ]))
    .bind(if stopped.is_some() {
        "stopped"
    } else {
        "running"
    })
    .bind(stopped.map(|s| s.reason))
    .bind(started_at)
    .bind(stopped.map(|s| s.at))
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

pub(crate) async fn assign(
    runtime: &PgPool,
    tenant: Uuid,
    experiment: Uuid,
    arm: &str,
    n: usize,
    at: DateTime<Utc>,
) -> Vec<String> {
    let anons: Vec<String> = (0..n)
        .map(|_| Uuid::now_v7().simple().to_string())
        .collect();
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO experiment_assignments (tenant_id, experiment_id, anon_id, arm, assigned_at)
         SELECT $1, $2, unnest($3::text[]), $4, $5",
    )
    .bind(tenant)
    .bind(experiment)
    .bind(&anons)
    .bind(arm)
    .bind(at)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    anons
}

pub(crate) async fn buy(
    runtime: &PgPool,
    tenant: Uuid,
    anons: &[String],
    at: DateTime<Utc>,
    order: Uuid,
) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO events (id, tenant_id, at, type, anon_id, props)
         SELECT gen_random_uuid(), $1, $2, 'purchase', unnest($3::text[]),
                jsonb_build_object('order_id', $4::uuid)",
    )
    .bind(tenant)
    .bind(at)
    .bind(anons)
    .bind(order)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

pub(crate) async fn cohort(
    runtime: &PgPool,
    shop: &Shop,
    experiment: Uuid,
    arm: &str,
    (n, buyers): (usize, usize),
    at: DateTime<Utc>,
    order: Uuid,
) {
    let anons = assign(runtime, shop.tenant, experiment, arm, n, at).await;
    buy(runtime, shop.tenant, &anons[..buyers], at + day(1.0), order).await;
}

pub(crate) async fn order(runtime: &PgPool, shop: &Shop, status: &str, payment: &str) -> Uuid {
    let id = testkit::storefront::raw_order(runtime, shop, shop.cz, "CZK", 12_900, 1, status).await;
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1")
        .bind(id)
        .bind(payment)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

pub(crate) async fn readout(
    runtime: &PgPool,
    tenant: Uuid,
    id: Uuid,
    now: DateTime<Utc>,
) -> Option<Readout> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    experiments::get(&mut tx, id, now).await.unwrap().readout
}

fn counts(arm: &ArmReadout) -> (u64, u64, u64) {
    (arm.assigned, arm.subjects, arm.converters)
}

#[track_caller]
fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "{actual:e} is not {expected:e}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_readout_counts_purchases_in_the_week_after_a_mature_assignment(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-window").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = tomorrow();
    let other = experiment(
        &runtime,
        shop.tenant,
        "older",
        t,
        Some(Stop {
            at: t + day(1.0),
            reason: "manual",
        }),
    )
    .await;
    let id = experiment(&runtime, shop.tenant, "homepage", t, None).await;

    let six = assign(&runtime, shop.tenant, id, "control", 1, t + day(1.0)).await;
    buy(&runtime, shop.tenant, &six, t + day(7.0), paid).await;
    let eight = assign(&runtime, shop.tenant, id, "control", 1, t + day(1.0)).await;
    buy(&runtime, shop.tenant, &eight, t + day(9.0), paid).await;
    let early = assign(&runtime, shop.tenant, id, "control", 1, t + day(2.0)).await;
    buy(&runtime, shop.tenant, &early, t + day(1.5), paid).await;
    let immature = assign(&runtime, shop.tenant, id, "control", 1, t + day(25.0)).await;
    buy(&runtime, shop.tenant, &immature, t + day(26.0), paid).await;
    let seven = assign(&runtime, shop.tenant, id, "variant", 1, t + day(3.0)).await;
    buy(&runtime, shop.tenant, &seven, t + day(10.0), paid).await;
    let today = assign(&runtime, shop.tenant, id, "variant", 1, t + day(30.0)).await;
    buy(&runtime, shop.tenant, &today, t + day(30.0), paid).await;
    let elsewhere = assign(&runtime, shop.tenant, other, "control", 3, t).await;
    buy(&runtime, shop.tenant, &elsewhere, t + day(0.5), paid).await;

    let r = readout(&runtime, shop.tenant, id, t + day(30.5))
        .await
        .unwrap();
    assert_eq!(
        (r.as_of, r.decides_from, r.final_, r.min_converters),
        (t + day(30.0), Some(t + day(21.0)), false, 100)
    );
    assert_eq!(
        counts(&r.control),
        (4, 3, 1),
        "a purchase 6 days after counts; 8 days after or before the assignment does not; an \
         immature subject is assigned but not counted"
    );
    assert_eq!(
        counts(&r.variant),
        (1, 1, 1),
        "a purchase exactly 7 days after counts; an assignment at the snapshot is not in it"
    );
    close(r.control.conversion_rate.unwrap(), 1.0 / 3.0);
    assert_eq!(r.variant.conversion_rate, Some(1.0));
    assert_eq!(
        r.state,
        ReadoutState::Collecting {
            need: Need::Converters
        }
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn cancelled_and_refunded_orders_do_not_convert(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-orders").await;
    let t = tomorrow();
    let id = experiment(&runtime, shop.tenant, "homepage", t, None).await;
    let subjects = assign(&runtime, shop.tenant, id, "control", 5, t + day(1.0)).await;
    for (subject, status, payment) in [
        (0, "cancelled", "unpaid"),
        (1, "confirmed", "refunded"),
        (2, "confirmed", "partially_refunded"),
        (3, "returned", "paid"),
        (4, "cancelled", "unpaid"),
        (4, "delivered", "paid"),
    ] {
        let o = order(&runtime, &shop, status, payment).await;
        buy(
            &runtime,
            shop.tenant,
            &subjects[subject..=subject],
            t + day(2.0),
            o,
        )
        .await;
    }

    let r = readout(&runtime, shop.tenant, id, t + day(30.0))
        .await
        .unwrap();
    assert_eq!(
        counts(&r.control),
        (5, 5, 3),
        "partially refunded, returned, and cancelled-then-bought subjects convert"
    );
    assert_eq!(counts(&r.variant), (0, 0, 0), "a missing arm is zeros");
    assert_eq!(r.variant.conversion_rate, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_stopped_readout_ends_a_week_after_the_stop_and_is_final_from_the_next_midnight(
    db: PgPool,
) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-stop").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = tomorrow();
    let stop = t + day(20.5);
    let id = experiment(
        &runtime,
        shop.tenant,
        "homepage",
        t,
        Some(Stop {
            at: stop,
            reason: "manual",
        }),
    )
    .await;
    let before = assign(&runtime, shop.tenant, id, "control", 1, t + day(20.25)).await;
    buy(&runtime, shop.tenant, &before, t + day(26.0), paid).await;
    let after = assign(&runtime, shop.tenant, id, "control", 1, t + day(21.0)).await;
    buy(&runtime, shop.tenant, &after, t + day(22.0), paid).await;

    let r = readout(&runtime, shop.tenant, id, t + day(27.4))
        .await
        .unwrap();
    assert_eq!((r.as_of, r.final_), (t + day(27.0), false));
    assert_eq!(
        counts(&r.control),
        (1, 0, 0),
        "not yet mature, and the assignment after the stop is out"
    );

    let r = readout(&runtime, shop.tenant, id, t + day(28.0))
        .await
        .unwrap();
    assert_eq!((r.as_of, r.final_), (stop + day(7.0), true));
    assert_eq!(counts(&r.control), (1, 1, 1));
    assert_eq!(
        readout(&runtime, shop.tenant, id, t + day(90.0)).await,
        Some(r),
        "the snapshot stays at the stop plus 7 days"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_draft_has_no_readout(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "readout-draft").await;
    let id = draft(&runtime, tenant, "homepage", 1).await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let detail = experiments::get(&mut tx, id, Utc::now()).await.unwrap();
    assert_eq!(
        (detail.experiment.id, detail.readout),
        (id, None),
        "a draft has no assignments to read"
    );
    assert!(matches!(
        experiments::get(&mut tx, Uuid::now_v7(), Utc::now()).await,
        Err(platform::Error::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_readout_matches_the_reference_posterior(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-numbers").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = tomorrow();
    let id = experiment(&runtime, shop.tenant, "homepage", t, None).await;
    cohort(&runtime, &shop, id, "control", (100, 10), t, paid).await;
    cohort(&runtime, &shop, id, "variant", (100, 15), t, paid).await;

    let r = readout(&runtime, shop.tenant, id, t + day(30.0))
        .await
        .unwrap();
    assert_eq!(
        (counts(&r.control), counts(&r.variant)),
        ((100, 100, 10), (100, 100, 15))
    );
    close(r.variant.p_best, 0.853_283_789_934);
    close(r.control.p_best, 1.0 - 0.853_283_789_934);
    close(r.control.expected_loss, 0.052_597_695_973_848_3);
    close(r.variant.expected_loss, 0.003_578_088_130_711_048);
    assert_eq!(
        r.state,
        ReadoutState::Collecting {
            need: Need::Converters
        },
        "15 converters are short of 100"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_clear_winner_is_named_unless_the_experiment_stopped_for_srm(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-winner").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = tomorrow();
    let mut states = vec![];
    for (name, reason) in [("manual", "manual"), ("broken", "srm")] {
        let id = experiment(
            &runtime,
            shop.tenant,
            name,
            t,
            Some(Stop {
                at: t + day(20.0),
                reason,
            }),
        )
        .await;
        cohort(&runtime, &shop, id, "control", (1000, 100), t, paid).await;
        cohort(&runtime, &shop, id, "variant", (1000, 160), t, paid).await;
        let r = readout(&runtime, shop.tenant, id, t + day(40.0))
            .await
            .unwrap();
        assert_eq!(
            (counts(&r.control), counts(&r.variant), r.final_),
            ((1000, 1000, 100), (1000, 1000, 160), true)
        );
        states.push(r.state);
    }
    assert_eq!(
        states,
        [
            ReadoutState::Winner { arm: Side::Variant },
            ReadoutState::Collecting {
                need: Need::Evidence
            },
        ]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_stopped_after_two_weeks_still_decides_once_its_subjects_mature(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "readout-stopped-time").await;
    let paid = order(&runtime, &shop, "confirmed", "paid").await;
    let t = tomorrow();
    let stopped_on = |days: f64| {
        Some(Stop {
            at: t + day(days),
            reason: "manual",
        })
    };
    let late = experiment(&runtime, shop.tenant, "late", t, stopped_on(20.0)).await;
    cohort(&runtime, &shop, late, "control", (2000, 100), t, paid).await;
    cohort(&runtime, &shop, late, "variant", (2000, 160), t, paid).await;

    let r = readout(&runtime, shop.tenant, late, t + day(20.0))
        .await
        .unwrap();
    assert_eq!(
        (r.state, r.decides_from, r.final_),
        (
            ReadoutState::Collecting { need: Need::Time },
            Some(t + day(21.0)),
            false
        )
    );
    let r = readout(&runtime, shop.tenant, late, t + day(21.0))
        .await
        .unwrap();
    assert_eq!(
        r.state,
        ReadoutState::Winner { arm: Side::Variant },
        "the subjects assigned before the stop reach 14 mature days"
    );

    let early = experiment(&runtime, shop.tenant, "early", t, stopped_on(10.0)).await;
    let r = readout(&runtime, shop.tenant, early, t + day(10.0))
        .await
        .unwrap();
    assert_eq!(r.decides_from, None);
}
