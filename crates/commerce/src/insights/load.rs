use chrono::{Days, NaiveDate};
use platform::Error;
use platform::db::TenantTx;

use crate::analytics::{FUNNEL_METRICS, day_bounds, web_vitals_p75};

use super::detect::rules::{
    FUNNEL_BASELINE_DAYS, FUNNEL_RECENT_DAYS, PRODUCT_DAYS, VITALS_DAYS, ZERO_RESULT_DAYS,
};
use super::detect::{Metrics, Vital, VitalStat};

fn window_start(day: NaiveDate, days: u64) -> NaiveDate {
    day - Days::new(days - 1)
}

pub(super) async fn metrics(tx: &mut TenantTx, day: NaiveDate) -> Result<Metrics, Error> {
    let recent_from = window_start(day, FUNNEL_RECENT_DAYS);
    let baseline_from = window_start(day, FUNNEL_RECENT_DAYS + FUNNEL_BASELINE_DAYS);
    let mut m = Metrics::default();
    let funnel = sqlx::query!(
        r#"SELECT metric,
                  coalesce(round(sum(value) FILTER (WHERE date >= $2)), 0)::bigint AS "recent!",
                  coalesce(round(sum(value) FILTER (WHERE date < $2)), 0)::bigint AS "baseline!"
           FROM daily_metrics
           WHERE dims = '{}' AND metric = ANY ($4) AND date BETWEEN $1 AND $3
           GROUP BY metric"#,
        baseline_from,
        recent_from,
        day,
        &FUNNEL_METRICS.map(String::from)
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in funnel {
        if let Some(i) = FUNNEL_METRICS.iter().position(|f| *f == r.metric) {
            m.funnel_recent[i] = r.recent;
            m.funnel_baseline[i] = r.baseline;
        }
    }

    m.products = sqlx::query!(
        r#"SELECT p.id,
                  coalesce(round(sum(d.value) FILTER (WHERE d.metric = 'product.views')), 0)::bigint
                      AS "views!",
                  coalesce(round(sum(d.value) FILTER (WHERE d.metric = 'product.add_to_carts')), 0)::bigint
                      AS "adds!"
           FROM daily_metrics d
           JOIN products p ON p.id::text = d.dims->>'product_id'
           WHERE d.metric IN ('product.views', 'product.add_to_carts')
             AND d.date BETWEEN $1 AND $2
           GROUP BY p.id ORDER BY p.id"#,
        window_start(day, PRODUCT_DAYS),
        day
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, r.views, r.adds))
    .collect();

    let first = window_start(day, VITALS_DAYS);
    m.vitals = web_vitals_p75(tx, day_bounds(first).0, day_bounds(day).1, None)
        .await?
        .into_iter()
        .filter_map(|r| {
            Some(VitalStat {
                metric: Vital::parse(&r.metric)?,
                template: r.template,
                p75: r.p75,
                samples: r.samples,
            })
        })
        .collect();

    m.requests = sqlx::query!(
        r#"SELECT template, sum(requests)::bigint AS "requests!" FROM analytics_counters
           WHERE day BETWEEN $1 AND $2 GROUP BY template"#,
        first,
        day
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.template, r.requests))
    .collect();

    m.zero_results = sqlx::query!(
        r#"SELECT query, sum(count)::bigint AS "searches!" FROM search_zero_results
           WHERE day BETWEEN $1 AND $2 GROUP BY query"#,
        window_start(day, ZERO_RESULT_DAYS),
        day
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.query, r.searches))
    .collect();
    Ok(m)
}
