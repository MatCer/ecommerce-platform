use chrono::{DateTime, Days, NaiveDate, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::detect::{self, Detection};
use crate::analytics::{day_bounds, web_vitals_p75};

const WINDOW_DAYS: u64 = 14;

/// The latest theme revision published from an AI run started on this insight's draft.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = InsightThemeChange)]
pub struct ThemeChange {
    pub revision_id: Uuid,
    pub revision_number: i32,
    pub published_at: DateTime<Utc>,
    /// The 14 full UTC days before the publish day.
    pub before: Window,
    /// The 14 full UTC days after the publish day.
    pub after: Window,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = InsightWindow)]
pub struct Window {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// `null` ("collecting") until the window's last day is over.
    pub measure: Option<Measure>,
}

/// The detector's own metric over one window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, ToSchema)]
#[schema(as = InsightMeasure)]
pub struct Measure {
    /// Funnel drop: the step's rate; low add-to-cart: the product's add-to-cart rate (0-1);
    /// slow page: the vital's p75 (ms; CLS unitless); zero-result search: the term's searches
    /// without results. `null` without data.
    pub value: Option<f64>,
    /// What the value rests on: sessions at the from-step, product views, measurements, or
    /// searches.
    pub samples: i64,
}

pub(super) struct Published {
    pub revision_id: Uuid,
    pub revision_number: i32,
    pub published_at: DateTime<Utc>,
}

pub(super) async fn theme_change(
    tx: &mut TenantTx,
    d: &Detection,
    p: Published,
    today: NaiveDate,
) -> Result<ThemeChange, Error> {
    let day = p.published_at.date_naive();
    let before = (day - Days::new(WINDOW_DAYS), day - Days::new(1));
    let after = (day + Days::new(1), day + Days::new(WINDOW_DAYS));
    Ok(ThemeChange {
        revision_id: p.revision_id,
        revision_number: p.revision_number,
        published_at: p.published_at,
        before: window(tx, d, before, today).await?,
        after: window(tx, d, after, today).await?,
    })
}

async fn window(
    tx: &mut TenantTx,
    d: &Detection,
    (from, to): (NaiveDate, NaiveDate),
    today: NaiveDate,
) -> Result<Window, Error> {
    let measure = if today > to {
        Some(measure(tx, d, from, to).await?)
    } else {
        None
    };
    Ok(Window { from, to, measure })
}

async fn measure(
    tx: &mut TenantTx,
    d: &Detection,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Measure, Error> {
    match d {
        Detection::FunnelDrop { transition, .. } => {
            let (step, next) = transition.steps();
            rate(tx, step, next, json!({}), from, to).await
        }
        Detection::LowAddToCart { product_id, .. } => {
            let dims = json!({ "product_id": product_id });
            rate(tx, "product.views", "product.add_to_carts", dims, from, to).await
        }
        Detection::SlowPage {
            template, metric, ..
        } => {
            let rows = web_vitals_p75(tx, day_bounds(from).0, day_bounds(to).1, None).await?;
            let hit = rows
                .iter()
                .find(|r| r.template == *template && r.metric == metric.as_str());
            Ok(Measure {
                value: hit.map(|r| r.p75),
                samples: hit.map_or(0, |r| r.samples),
            })
        }
        Detection::ZeroResultSearch { query, .. } => {
            let n = sqlx::query_scalar!(
                r#"SELECT coalesce(sum(count), 0)::bigint AS "n!" FROM search_zero_results
                   WHERE query = $1 AND day BETWEEN $2 AND $3"#,
                query,
                from,
                to
            )
            .fetch_one(&mut **tx)
            .await?;
            Ok(Measure {
                value: Some(n as f64),
                samples: n,
            })
        }
    }
}

async fn rate(
    tx: &mut TenantTx,
    whole: &str,
    part: &str,
    dims: Value,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Measure, Error> {
    let r = sqlx::query!(
        r#"SELECT coalesce(round(sum(value) FILTER (WHERE metric = $1)), 0)::bigint AS "whole!",
                  coalesce(round(sum(value) FILTER (WHERE metric = $2)), 0)::bigint AS "part!"
           FROM daily_metrics
           WHERE metric IN ($1, $2) AND dims = $3 AND date BETWEEN $4 AND $5"#,
        whole,
        part,
        dims,
        from,
        to
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(Measure {
        value: (r.whole > 0).then(|| detect::rate(r.part, r.whole)),
        samples: r.whole,
    })
}
