use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;

use super::Experiment;
use super::stats::{Counts, Pair};

pub(super) async fn counts(
    tx: &mut TenantTx,
    e: &Experiment,
    started_at: DateTime<Utc>,
    as_of: DateTime<Utc>,
) -> Result<Pair<Counts>, Error> {
    // MATERIALIZED: when the planner underestimates `a` (new assignments past the last ANALYZE)
    // it would rerun `converted` once per assignment, which is quadratic.
    let rows = sqlx::query!(
        r#"WITH a AS (
               SELECT anon_id, arm, assigned_at,
                      assigned_at <= $2::timestamptz - interval '7 days' AS mature
               FROM experiment_assignments
               WHERE experiment_id = $1 AND assigned_at >= $3::timestamptz
                 AND assigned_at < $2::timestamptz
                 AND ($4::timestamptz IS NULL OR assigned_at <= $4::timestamptz)
           ), converted AS MATERIALIZED (
               SELECT DISTINCT a.anon_id FROM a
               JOIN events e ON e.anon_id = a.anon_id AND e.type = 'purchase'
                AND e.at > a.assigned_at AND e.at <= a.assigned_at + interval '7 days'
               JOIN orders o ON o.id = (e.props ->> 'order_id')::uuid
               WHERE a.mature AND e.at >= $3::timestamptz AND e.at <= $2::timestamptz
                 AND o.status <> 'cancelled' AND o.payment_status <> 'refunded'
           )
           SELECT a.arm AS "arm!", count(*) AS "assigned!",
                  count(*) FILTER (WHERE a.mature) AS "subjects!",
                  count(c.anon_id) AS "converters!"
           FROM a LEFT JOIN converted c USING (anon_id) GROUP BY a.arm"#,
        e.id,
        as_of,
        started_at,
        e.stopped_at
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Pair::<Counts>::default();
    for r in rows {
        let arm = if r.arm == e.arms.control.key {
            &mut out.control
        } else if r.arm == e.arms.variant.key {
            &mut out.variant
        } else {
            continue;
        };
        *arm = Counts {
            assigned: r.assigned as u64,
            subjects: r.subjects as u64,
            converters: r.converters as u64,
        };
    }
    Ok(out)
}
