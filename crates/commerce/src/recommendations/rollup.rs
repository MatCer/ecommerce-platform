//! Rollups (spec §11.2): recomputed from their sources, so every step is idempotent and a
//! rerun converges.
//!
//! - `product_stats_daily`: consented `view_item`/`add_to_cart` events (stored only with
//!   `analytics`, A20) and placed, non-cancelled orders (authoritative purchases and revenue).
//! - `co_purchases`: distinct orders of the last 90 days containing both products, support
//!   ≥ [`MIN_SUPPORT`], both directions.
//! - `product_scores`: decayed sums over the last 90 days (half-life [`HALF_LIFE_DAYS`]).
//! - `product_popularity`: the scores summed over markets, for the search documents; written
//!   only when a value moves by at least 10 % (or from/to zero), and the changed products are
//!   marked for reindexing ([`take_reindex`]).
//! - `customer_affinity`: categories/brands from the orders of customers whose current
//!   `personalization` consent is granted.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use uuid::Uuid;

use super::{HALF_LIFE_DAYS, MIN_SUPPORT, WINDOW_DAYS};

/// A first run (or an explicit backfill) reads this many days: enough for last year's month
/// (seasonal) and the 90-day window.
pub const BACKFILL_DAYS: u32 = 400;
/// The nightly run recomputes everything retained (scores and last year's month), so a late
/// change to an older order (a cancellation weeks or months later) leaves every result.
/// ponytail: a full recompute per tenant and night; recompute only the days of changed orders
/// if it gets heavy for large tenants.
pub const NIGHTLY_DAYS: u32 = BACKFILL_DAYS;
/// Stats older than this are pruned.
pub const KEEP_DAYS: i64 = 400;
/// Customer affinity looks back one year, with a slower decay than the scores.
const AFFINITY_DAYS: i64 = 365;
const AFFINITY_HALF_LIFE_DAYS: f64 = 90.0;
/// Popularity blend: a purchased unit counts like 10 views, an add like 3.
const PURCHASE_WEIGHT: i32 = 10;
const ADD_WEIGHT: i32 = 3;
/// Relative change of the stored popularity that triggers a reindex.
const POPULARITY_STEP: f64 = 0.1;

fn day_start(day: NaiveDate) -> DateTime<Utc> {
    day.and_time(NaiveTime::MIN).and_utc()
}

/// Whether the tenant has any stats yet (a first run backfills).
pub async fn has_stats(tx: &mut TenantTx) -> Result<bool, Error> {
    Ok(
        sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM product_stats_daily) AS "e!""#)
            .fetch_one(&mut **tx)
            .await?,
    )
}

/// Recomputes the stats of the UTC days `from..=to`.
pub async fn stats(tx: &mut TenantTx, from: NaiveDate, to: NaiveDate) -> Result<(), Error> {
    let (start, end) = (day_start(from), day_start(to) + chrono::Duration::days(1));
    sqlx::query!(
        "DELETE FROM product_stats_daily WHERE date >= $1 AND date <= $2",
        from,
        to
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO product_stats_daily (tenant_id, date, market_id, product_id, views,
                                            add_to_carts, purchases, revenue_minor)
           SELECT $1, x.day, x.market_id, x.product_id, sum(x.views)::int, sum(x.adds)::int,
                  sum(x.units)::int, sum(x.revenue)::bigint
           FROM (
               SELECT (e.at AT TIME ZONE 'UTC')::date AS day, e.market_id,
                      (e.props->>'product_id')::uuid AS product_id,
                      count(*) FILTER (WHERE e.type = 'view_item') AS views,
                      count(*) FILTER (WHERE e.type = 'add_to_cart') AS adds,
                      0::bigint AS units, 0::numeric AS revenue
               FROM events e
               WHERE e.at >= $2 AND e.at < $3 AND e.type IN ('view_item', 'add_to_cart')
                 AND e.market_id IS NOT NULL
                 AND e.props->>'product_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
               GROUP BY 1, 2, 3
               UNION ALL
               SELECT (o.placed_at AT TIME ZONE 'UTC')::date, o.market_id, l.product_id,
                      0, 0, sum(l.quantity), sum(l.total_minor)
               FROM orders o JOIN order_lines l ON l.order_id = o.id
               WHERE o.placed_at >= $2 AND o.placed_at < $3 AND o.status <> 'cancelled'
                 AND l.product_id IS NOT NULL
               GROUP BY 1, 2, 3
           ) x
           JOIN products p ON p.id = x.product_id
           JOIN markets m ON m.id = x.market_id
           GROUP BY x.day, x.market_id, x.product_id"#,
        tx.tenant_id(),
        start,
        end
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Rebuilds `co_purchases` from the orders of the 90 days before `now`.
pub async fn co_purchases(tx: &mut TenantTx, now: DateTime<Utc>) -> Result<(), Error> {
    sqlx::query!("DELETE FROM co_purchases")
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        "INSERT INTO co_purchases (tenant_id, product_a, product_b, count_90d)
         SELECT $1, a.product_id, b.product_id, count(DISTINCT o.id)::int
         FROM orders o
         JOIN order_lines a ON a.order_id = o.id AND a.product_id IS NOT NULL
         JOIN order_lines b ON b.order_id = o.id AND b.product_id IS NOT NULL
                           AND b.product_id <> a.product_id
         WHERE o.placed_at > $2 AND o.placed_at <= $3 AND o.status <> 'cancelled'
         GROUP BY a.product_id, b.product_id
         HAVING count(DISTINCT o.id) >= $4",
        tx.tenant_id(),
        now - chrono::Duration::days(WINDOW_DAYS),
        now,
        MIN_SUPPORT
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Rebuilds `product_scores` as of `today` (inclusive) from the stats of the window.
pub async fn scores(tx: &mut TenantTx, today: NaiveDate) -> Result<(), Error> {
    sqlx::query!("DELETE FROM product_scores")
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        r#"INSERT INTO product_scores (tenant_id, market_id, product_id, sales_score, popularity)
           SELECT $1, market_id, product_id,
                  sum(purchases * power(0.5, ($2::date - date)::float8 / $3)),
                  sum((purchases * $5 + add_to_carts * $6 + views)
                      * power(0.5, ($2::date - date)::float8 / $3))
           FROM product_stats_daily
           WHERE date > $2::date - $4::int AND date <= $2
           GROUP BY market_id, product_id
           HAVING sum(purchases + add_to_carts + views) > 0"#,
        tx.tenant_id(),
        today,
        HALF_LIFE_DAYS,
        i32::try_from(WINDOW_DAYS).unwrap_or(90),
        PURCHASE_WEIGHT,
        ADD_WEIGHT
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Updates the search popularity from the scores (debounced) and marks the changed products
/// for reindexing ([`take_reindex`]); returns how many changed.
pub async fn popularity(tx: &mut TenantTx) -> Result<u64, Error> {
    Ok(sqlx::query!(
        r#"WITH fresh AS (
               SELECT product_id, round(sum(popularity))::int AS p
               FROM product_scores GROUP BY product_id
           ), merged AS (
               SELECT coalesce(f.product_id, o.product_id) AS product_id,
                      coalesce(f.p, 0) AS p, o.popularity AS old
               FROM fresh f FULL JOIN product_popularity o ON o.product_id = f.product_id
           )
           INSERT INTO product_popularity (tenant_id, product_id, popularity, reindex)
           SELECT $1, product_id, p, true FROM merged
           WHERE (old IS NULL AND p > 0)
              OR (old IS NOT NULL AND abs(p - old) >= greatest(1, ceil(old * $2::float8)))
           ON CONFLICT (tenant_id, product_id)
           DO UPDATE SET popularity = EXCLUDED.popularity, reindex = true"#,
        tx.tenant_id(),
        POPULARITY_STEP
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Takes the products marked for reindexing (clears the marks). Run it in the transaction
/// that enqueues their jobs: either both happen or the marks stay for the next run.
pub async fn take_reindex(tx: &mut TenantTx) -> Result<Vec<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "UPDATE product_popularity SET reindex = false WHERE reindex RETURNING product_id"
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// Rebuilds `customer_affinity` for the customers whose current `personalization` consent is
/// granted (A20); everyone else has no rows.
pub async fn customer_affinity(tx: &mut TenantTx, now: DateTime<Utc>) -> Result<(), Error> {
    sqlx::query!("DELETE FROM customer_affinity")
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        r#"WITH consenting AS (
               SELECT subject_id FROM (
                   SELECT DISTINCT ON (subject_id) subject_id, granted FROM consent_records
                   WHERE subject_type = 'customer' AND purpose = 'personalization'
                   ORDER BY subject_id, at DESC, id DESC
               ) latest WHERE granted
           ), bought AS (
               SELECT o.customer_id, l.product_id,
                      sum(l.quantity * power(0.5, extract(epoch FROM $2 - o.placed_at)::float8
                                                  / 86400.0 / $3)) AS w
               FROM orders o JOIN order_lines l ON l.order_id = o.id
               WHERE o.customer_id IS NOT NULL AND o.status <> 'cancelled'
                 AND o.placed_at > $2 - make_interval(days => $4::int) AND o.placed_at <= $2
                 AND l.product_id IS NOT NULL
                 AND o.customer_id::text IN (SELECT subject_id FROM consenting)
               GROUP BY o.customer_id, l.product_id
           )
           INSERT INTO customer_affinity (tenant_id, customer_id, dim, key, score)
           SELECT $1::uuid, b.customer_id, 'category', pc.category_id::text, sum(b.w)
           FROM bought b JOIN product_categories pc ON pc.product_id = b.product_id
           GROUP BY b.customer_id, pc.category_id
           UNION ALL
           SELECT $1::uuid, b.customer_id, 'brand', p.brand, sum(b.w)
           FROM bought b JOIN products p ON p.id = b.product_id
           WHERE p.brand IS NOT NULL
           GROUP BY b.customer_id, p.brand"#,
        tx.tenant_id(),
        now,
        AFFINITY_HALF_LIFE_DAYS,
        i32::try_from(AFFINITY_DAYS).unwrap_or(365)
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// One rollup run for the tenant: the stats of the last `days` days (today included), then
/// everything derived from them. Returns how many products' search popularity changed (they
/// are marked for [`take_reindex`]).
pub async fn run(tx: &mut TenantTx, now: DateTime<Utc>, days: u32) -> Result<u64, Error> {
    // One rollup per tenant at a time (the hourly job and a seed backfill may overlap): the
    // second waits instead of failing on the first one's rows.
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        format!("recommendations-rollup:{}", tx.tenant_id())
    )
    .execute(&mut **tx)
    .await?;
    let today = now.date_naive();
    let from = today - chrono::Days::new(u64::from(days.max(1) - 1));
    stats(tx, from, today).await?;
    sqlx::query!(
        "DELETE FROM product_stats_daily WHERE date < $1",
        today - chrono::Days::new(u64::try_from(KEEP_DAYS).unwrap_or(400))
    )
    .execute(&mut **tx)
    .await?;
    co_purchases(tx, now).await?;
    scores(tx, today).await?;
    customer_affinity(tx, now).await?;
    popularity(tx).await
}
