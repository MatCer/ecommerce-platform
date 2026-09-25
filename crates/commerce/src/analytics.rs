//! Analytics (spec §7.6, §11.3, A20).
//!
//! Two sources, two levels of trust:
//! - **Counters** (before or without consent): the edge counts page requests per market,
//!   route template and UTC day, with no identifiers and no query text ([`record_counters`]).
//! - **Consented events**, stored only when the server-side consent records grant `analytics`
//!   to the request's anonymous subject: the browser events (`page_view`, `view_item`,
//!   `add_to_cart`, `begin_checkout`, `search`, `web_vitals`) from the beacon ([`ingest`];
//!   claimed purposes are ignored, props are allowlisted per type) and the `purchase` the API
//!   records from the placed order ([`link_purchase`]). The stored `anon_id` is derived from
//!   the subject (a keyed hash), never the subject itself. Orders of visitors without consent
//!   leave no analytics trace; sales figures come from `orders` directly.
//!
//! Hourly rollups ([`rollup`]) aggregate events into `daily_metrics`; the dashboard
//! ([`dashboard`]) reads money from `orders` (authoritative) and traffic from counters and
//! rollups. Days are UTC.

use std::collections::BTreeMap;

use chrono::{DateTime, Days, NaiveDate, NaiveTime, Utc};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::consent::{self, ConsentPurpose, Subject};

/// Route templates the edge counts and events carry.
pub const TEMPLATES: [&str; 8] = [
    "home", "category", "product", "search", "page", "blog", "checkout", "other",
];
/// Events per beacon batch; the rest is dropped.
pub const MAX_BATCH: usize = 50;
/// A visitor's events closer than this belong to one session.
pub const SESSION_GAP_MINUTES: i64 = 30;
/// Hourly rollups (today and yesterday; the last two weeks once a day).
pub const ROLLUP_JOB: &str = "analytics.rollup";
/// Nightly partition maintenance and retention (13 months, §14).
pub const PARTITIONS_JOB: &str = "analytics.partitions";

const MAX_COUNTERS: usize = 10_000;

fn invalid(detail: impl Into<String>) -> Error {
    Error::Validation {
        code: "invalid_counters",
        detail: detail.into(),
    }
}

// --- counters (edge) ---------------------------------------------------------------------------

/// What the edge flushes every few seconds (`POST /internal/v1/analytics/counters`). The edge
/// resends a failed batch unchanged with the same `batch_id`; tenants that already counted it
/// skip it, so a retry never counts twice.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CounterBatch {
    pub batch_id: Uuid,
    #[serde(default)]
    pub counters: Vec<PageCounter>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PageCounter {
    pub tenant_id: Uuid,
    pub market_id: Uuid,
    pub day: NaiveDate,
    pub template: String,
    pub requests: i64,
}

#[derive(Debug, Default, Serialize, ToSchema)]
pub struct CountersRecorded {
    /// Rows added now.
    pub counters: usize,
    /// Tenants that had counted this batch already (a retry).
    pub replayed_tenants: usize,
}

fn plausible_day(day: NaiveDate, today: NaiveDate) -> bool {
    day <= today + Days::new(1) && day + Days::new(2) >= today
}

/// Adds the edge's counts. Rows for unknown markets or implausible days are skipped (the edge
/// may flush after a market was removed); each tenant is written in its own transaction
/// together with the batch id, so a resent batch is skipped by the tenants that have it.
pub async fn record_counters(db: &PgPool, batch: &CounterBatch) -> Result<CountersRecorded, Error> {
    if batch.counters.len() > MAX_COUNTERS {
        return Err(invalid(format!("at most {MAX_COUNTERS} rows per batch")));
    }
    let today = Utc::now().date_naive();
    let mut tenants: BTreeMap<Uuid, Vec<&PageCounter>> = BTreeMap::new();
    for c in &batch.counters {
        if !TEMPLATES.contains(&c.template.as_str()) || !(1..=100_000_000).contains(&c.requests) {
            return Err(invalid("unknown template or count out of range"));
        }
        if plausible_day(c.day, today) {
            tenants.entry(c.tenant_id).or_default().push(c);
        }
    }
    let mut out = CountersRecorded::default();
    for (tenant, counters) in tenants {
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM platform.tenants WHERE id = $1) AS "ok!""#,
            tenant
        )
        .fetch_one(db)
        .await?;
        if !exists {
            continue;
        }
        let mut tx = tenant_tx(db, tenant).await?;
        let first = sqlx::query!(
            "INSERT INTO analytics_counter_batches (tenant_id, batch_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
            tenant,
            batch.batch_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !first {
            out.replayed_tenants += 1;
            continue;
        }
        sqlx::query!(
            "DELETE FROM analytics_counter_batches WHERE received_at < now() - interval '5 days'"
        )
        .execute(&mut *tx)
        .await?;
        let markets: Vec<Uuid> = sqlx::query_scalar!("SELECT id FROM markets")
            .fetch_all(&mut *tx)
            .await?;
        for c in counters.iter().filter(|c| markets.contains(&c.market_id)) {
            sqlx::query!(
                "INSERT INTO analytics_counters (tenant_id, market_id, day, template, requests)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (tenant_id, market_id, day, template)
                 DO UPDATE SET requests = analytics_counters.requests + EXCLUDED.requests",
                tenant,
                c.market_id,
                c.day,
                c.template,
                c.requests
            )
            .execute(&mut *tx)
            .await?;
            out.counters += 1;
        }
        tx.commit().await?;
    }
    Ok(out)
}

// --- consented browser events ------------------------------------------------------------------

/// A validated beacon event: type + allowlisted props.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanEvent {
    pub kind: &'static str,
    pub props: Value,
}

fn template_of(raw: &Map<String, Value>) -> String {
    raw.get("template")
        .and_then(Value::as_str)
        .filter(|t| TEMPLATES.contains(t))
        .unwrap_or("other")
        .to_owned()
}

fn uuid_of(raw: &Map<String, Value>, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .map(|u| u.to_string())
}

/// Validates one beacon event; `None` drops it. Only known types and props survive, so the
/// beacon cannot smuggle identifiers or free text into the store.
pub fn clean_event(raw: &Value) -> Option<CleanEvent> {
    let raw = raw.as_object()?;
    let template = template_of(raw);
    let kind = raw.get("type")?.as_str()?;
    let (kind, props) = match kind {
        "page_view" => ("page_view", json!({ "template": template })),
        "begin_checkout" => ("begin_checkout", json!({ "template": template })),
        // Consented visitors only (never counted before consent), minimized like the
        // zero-result log: short product-like text, folded; contact-like text is dropped.
        "search" => {
            let query = crate::search::query::loggable_query(raw.get("query")?.as_str()?)?;
            ("search", json!({ "template": template, "query": query }))
        }
        "view_item" => (
            "view_item",
            json!({ "template": template, "product_id": uuid_of(raw, "product_id")? }),
        ),
        "add_to_cart" => {
            let product = uuid_of(raw, "product_id");
            let variant = uuid_of(raw, "variant_id");
            if product.is_none() && variant.is_none() {
                return None;
            }
            let quantity = raw
                .get("quantity")
                .and_then(Value::as_i64)
                .filter(|q| (1..=999).contains(q))
                .unwrap_or(1);
            (
                "add_to_cart",
                json!({ "template": template, "product_id": product, "variant_id": variant,
                        "quantity": quantity }),
            )
        }
        // The SDK reports `web_vital` per metric; stored as `web_vitals`.
        "web_vital" | "web_vitals" => {
            let name = raw.get("name")?.as_str()?;
            let max = match name {
                "LCP" | "INP" => 60_000.0,
                "CLS" => 100.0,
                _ => return None,
            };
            let value = raw
                .get("value")?
                .as_f64()
                .filter(|v| v.is_finite() && (0.0..=max).contains(v))?;
            (
                "web_vitals",
                json!({ "template": template, "name": name, "value": value }),
            )
        }
        _ => return None,
    };
    Some(CleanEvent { kind, props })
}

/// The pseudonymous id events are stored under: a hash of the consent subject, scoped to
/// the tenant, so the stored events never contain the cookie value itself.
pub fn anon_id(tenant_id: Uuid, subject: &str) -> String {
    let digest = Sha256::digest(format!("analytics-anon:{tenant_id}:{subject}"));
    hex::encode(&digest[..16])
}

/// The visitor's current session (last event under 30 minutes ago), or a new one. A
/// transaction-scoped lock per visitor keeps two concurrent first beacons from opening two
/// sessions (the second waits and sees the first one's events).
async fn session_for(tx: &mut TenantTx, anon: &str, now: DateTime<Utc>) -> Result<Uuid, Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        format!("analytics-session:{}:{anon}", tx.tenant_id())
    )
    .execute(&mut **tx)
    .await?;
    // No upper bound on `at`: a concurrent request with a later timestamp that took the lock
    // first opened the session this one belongs to. A session lasts at most a day (the
    // rollups attribute it to its start day and read a bounded range).
    let since = now - chrono::Duration::minutes(SESSION_GAP_MINUTES);
    let last = sqlx::query_scalar!(
        "SELECT e.session_id FROM events e
         WHERE e.anon_id = $1 AND e.at > $2 AND e.session_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM events f
                           WHERE f.anon_id = $1 AND f.session_id = e.session_id
                             AND f.at < $3::timestamptz - interval '1 day')
         ORDER BY e.at DESC LIMIT 1",
        anon,
        since,
        now
    )
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    Ok(last.unwrap_or_else(crate::id::new_id))
}

/// The analytics grant of the anonymous subject, resolved from `consent_records` now (A20).
async fn granted(tx: &mut TenantTx, subject: &str) -> Result<Option<Vec<String>>, Error> {
    if !consent::well_formed_anon(subject) {
        return Ok(None);
    }
    let state = consent::state(tx, &Subject::Anon(subject.to_owned())).await?;
    if state.purposes.get(ConsentPurpose::Analytics) != Some(true) {
        return Ok(None);
    }
    Ok(Some(
        ConsentPurpose::ALL
            .into_iter()
            .filter(|p| state.purposes.get(*p) == Some(true))
            .map(|p| p.as_str().to_owned())
            .collect(),
    ))
}

/// The valid events of a beacon body (`{"events": [...]}`), at most [`MAX_BATCH`].
pub fn parse_batch(body: &[u8]) -> Vec<CleanEvent> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("events").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .take(MAX_BATCH)
        .filter_map(clean_event)
        .collect()
}

/// Stores a beacon batch when the subject granted `analytics`; returns how many events were
/// stored (0 without consent: nothing is kept, not even a count).
pub async fn ingest(
    tx: &mut TenantTx,
    market_id: Uuid,
    subject: Option<&str>,
    body: &[u8],
    now: DateTime<Utc>,
) -> Result<usize, Error> {
    let Some(subject) = subject else {
        return Ok(0);
    };
    let Some(purposes) = granted(tx, subject).await? else {
        return Ok(0);
    };
    let mut events = parse_batch(body);
    if events.is_empty() {
        return Ok(0);
    }
    let anon = anon_id(tx.tenant_id(), subject);
    let session = session_for(tx, &anon, now).await?;
    for e in &mut events {
        // The cart knows only the variant: the product comes from the catalog (this tenant's
        // only, via RLS), not from the client.
        if e.kind == "add_to_cart"
            && e.props["product_id"].is_null()
            && let Some(variant) = e.props["variant_id"]
                .as_str()
                .and_then(|v| Uuid::parse_str(v).ok())
        {
            let product =
                sqlx::query_scalar!("SELECT product_id FROM variants WHERE id = $1", variant)
                    .fetch_optional(&mut **tx)
                    .await?;
            e.props["product_id"] = json!(product);
        }
    }
    for e in &events {
        sqlx::query!(
            "INSERT INTO events (id, tenant_id, at, type, anon_id, session_id, market_id, props,
                                 consent_purposes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            crate::id::new_id(),
            tx.tenant_id(),
            now,
            e.kind,
            anon,
            session,
            market_id,
            e.props,
            &purposes
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(events.len())
}

// --- purchases ---------------------------------------------------------------------------------

/// One purchase event per order (a fixed id and the order's placement time): a retried
/// placement links nothing twice.
fn purchase_id(order_id: Uuid) -> Uuid {
    let digest = Sha256::digest(format!("purchase:{order_id}"));
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// Records the `purchase` of a placed order for the visitor's analytics session, only when
/// the checkout request carried a consent subject whose records grant `analytics` (A20). The
/// amounts come from the order, never from the client. Without the grant nothing is stored:
/// sales figures come from `orders` directly.
pub async fn link_purchase(
    tx: &mut TenantTx,
    order_id: Uuid,
    subject: &str,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    let Some(purposes) = granted(tx, subject).await? else {
        return Ok(false);
    };
    let anon = anon_id(tx.tenant_id(), subject);
    let session = session_for(tx, &anon, now).await?;
    let done = sqlx::query!(
        "INSERT INTO events (id, tenant_id, at, type, anon_id, session_id, market_id, props,
                             consent_purposes)
         SELECT $1, tenant_id, placed_at, 'purchase', $3, $4, market_id,
                jsonb_build_object('order_id', id, 'total_minor', total_minor, 'currency', currency),
                $5
         FROM orders WHERE id = $2
         ON CONFLICT (tenant_id, at, id) DO NOTHING",
        purchase_id(order_id),
        order_id,
        anon,
        session,
        &purposes
    )
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected() == 1)
}

// --- rollups -----------------------------------------------------------------------------------

fn day_bounds(day: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = day.and_time(NaiveTime::MIN).and_utc();
    (start, start + chrono::Duration::days(1))
}

/// Recomputes one UTC day of `daily_metrics` from the events (idempotent). Sessions and the
/// funnel count each session once, on the day it started (its steps may continue after
/// midnight), so summing days never double-counts a session; page views and product
/// views/adds count by event time.
pub async fn rollup(tx: &mut TenantTx, day: NaiveDate) -> Result<(), Error> {
    let (start, end) = day_bounds(day);
    sqlx::query!("DELETE FROM daily_metrics WHERE date = $1", day)
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        r#"INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
           SELECT $1, $2, s.market_id, m.metric, '{}'::jsonb, m.value
           FROM (
               SELECT market_id,
                      count(*) AS sessions,
                      count(*) FILTER (WHERE view_item) AS view_item,
                      count(*) FILTER (WHERE add_to_cart) AS add_to_cart,
                      count(*) FILTER (WHERE begin_checkout) AS begin_checkout,
                      count(*) FILTER (WHERE purchase) AS purchase
               FROM (
                   SELECT session_id,
                          (array_agg(market_id ORDER BY at))[1] AS market_id,
                          bool_or(type = 'view_item') AS view_item,
                          bool_or(type = 'add_to_cart') AS add_to_cart,
                          bool_or(type = 'begin_checkout') AS begin_checkout,
                          bool_or(type = 'purchase') AS purchase
                   FROM events
                   WHERE session_id IS NOT NULL AND market_id IS NOT NULL
                     AND at >= $3::timestamptz - interval '2 days'
                     AND at < $4::timestamptz + interval '2 days'
                   GROUP BY session_id
                   HAVING min(at) >= $3 AND min(at) < $4
               ) started
               GROUP BY market_id
           ) s
           CROSS JOIN LATERAL (VALUES
               ('sessions', s.sessions::float8),
               ('funnel.view_item', s.view_item::float8),
               ('funnel.add_to_cart', s.add_to_cart::float8),
               ('funnel.begin_checkout', s.begin_checkout::float8),
               ('funnel.purchase', s.purchase::float8)
           ) AS m(metric, value)"#,
        tx.tenant_id(),
        day,
        start,
        end
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
           SELECT $1, $2, market_id, 'page_views', '{}'::jsonb, count(*)::float8
           FROM events
           WHERE at >= $3 AND at < $4 AND type = 'page_view' AND market_id IS NOT NULL
           GROUP BY market_id"#,
        tx.tenant_id(),
        day,
        start,
        end
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        r#"INSERT INTO daily_metrics (tenant_id, date, market_id, metric, dims, value)
           SELECT $1, $2, market_id,
                  CASE type WHEN 'view_item' THEN 'product.views' ELSE 'product.add_to_carts' END,
                  jsonb_build_object('product_id', props->>'product_id'), count(*)::float8
           FROM events
           WHERE at >= $3 AND at < $4 AND type IN ('view_item', 'add_to_cart')
             AND market_id IS NOT NULL AND props->>'product_id' IS NOT NULL
           GROUP BY market_id, type, props->>'product_id'"#,
        tx.tenant_id(),
        day,
        start,
        end
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// --- dashboard ---------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DashboardQuery {
    /// First day (UTC, inclusive).
    pub from: NaiveDate,
    /// Last day (UTC, inclusive); at most 366 days after `from`.
    pub to: NaiveDate,
    /// Only this market; all markets when omitted.
    pub market_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SalesTotals {
    pub currency: String,
    /// Sum of order totals (placed, not cancelled), minor units.
    pub revenue_minor: i64,
    pub orders: i64,
    /// Average order value, minor units (0 without orders).
    pub aov_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DailySales {
    pub date: NaiveDate,
    pub currency: String,
    pub revenue_minor: i64,
    pub orders: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DailyTraffic {
    pub date: NaiveDate,
    /// All page requests counted by the edge (no consent needed, no identifiers).
    pub page_requests: i64,
    /// Sessions of visitors who consented to analytics.
    pub consented_sessions: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FunnelStep {
    /// `sessions`, `view_item`, `add_to_cart`, `begin_checkout`, `purchase`.
    pub step: String,
    /// Consented sessions that reached the step.
    pub sessions: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateRequests {
    pub template: String,
    pub requests: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Traffic {
    pub page_requests: i64,
    pub consented_sessions: i64,
    pub consented_page_views: i64,
    /// Consented sessions with a purchase / consented sessions (by session start day);
    /// `None` without sessions.
    pub conversion_rate: Option<f64>,
    /// Labelled "consented sessions" (A20): visitors without consent are not in it.
    pub funnel: Vec<FunnelStep>,
    pub by_template: Vec<TemplateRequests>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TopProduct {
    pub product_id: Option<Uuid>,
    pub name: String,
    pub currency: String,
    pub units: i64,
    pub revenue_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueryCount {
    pub query: String,
    pub count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VitalP75 {
    pub template: String,
    /// `LCP`, `INP` (ms) or `CLS` (unitless).
    pub metric: String,
    pub p75: f64,
    pub samples: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Dashboard {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub market_id: Option<Uuid>,
    pub sales: Vec<SalesTotals>,
    pub daily_sales: Vec<DailySales>,
    pub daily_traffic: Vec<DailyTraffic>,
    pub traffic: Traffic,
    pub top_products: Vec<TopProduct>,
    /// Searches of consented visitors (minimized text).
    pub top_searches: Vec<QueryCount>,
    /// Searches without results (the search log, minimized; filtered by the market's locales).
    pub zero_result_searches: Vec<QueryCount>,
    pub web_vitals: Vec<VitalP75>,
}

const TOP: i64 = 10;

/// The admin dashboard for a date range (and optionally one market).
pub async fn dashboard(tx: &mut TenantTx, q: &DashboardQuery) -> Result<Dashboard, Error> {
    if q.to < q.from || q.from + Days::new(366) < q.to {
        return Err(Error::Validation {
            code: "invalid_range",
            detail: "`to` must be on or after `from`, at most 366 days apart".into(),
        });
    }
    let (start, _) = day_bounds(q.from);
    let (_, end) = day_bounds(q.to);
    let m = q.market_id;
    if let Some(id) = m {
        sqlx::query_scalar!("SELECT id FROM markets WHERE id = $1", id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(Error::NotFound)?;
    }

    let sales = sqlx::query_as!(
        SalesTotals,
        r#"SELECT currency AS "currency!", sum(total_minor)::bigint AS "revenue_minor!",
                  count(*) AS "orders!", (sum(total_minor) / count(*))::bigint AS "aov_minor!"
           FROM orders
           WHERE placed_at >= $1 AND placed_at < $2 AND status <> 'cancelled'
             AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY currency ORDER BY currency"#,
        start,
        end,
        m
    )
    .fetch_all(&mut **tx)
    .await?;
    let daily_sales = sqlx::query_as!(
        DailySales,
        r#"SELECT (placed_at AT TIME ZONE 'UTC')::date AS "date!", currency AS "currency!",
                  sum(total_minor)::bigint AS "revenue_minor!", count(*) AS "orders!"
           FROM orders
           WHERE placed_at >= $1 AND placed_at < $2 AND status <> 'cancelled'
             AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY 1, 2 ORDER BY 1, 2"#,
        start,
        end,
        m
    )
    .fetch_all(&mut **tx)
    .await?;

    let daily_traffic = sqlx::query_as!(
        DailyTraffic,
        r#"SELECT d::date AS "date!",
                  coalesce((SELECT sum(requests) FROM analytics_counters c
                            WHERE c.day = d::date AND ($3::uuid IS NULL OR c.market_id = $3)), 0)::bigint
                      AS "page_requests!",
                  coalesce((SELECT sum(value) FROM daily_metrics x
                            WHERE x.date = d::date AND x.metric = 'sessions'
                              AND ($3::uuid IS NULL OR x.market_id = $3)), 0)::bigint
                      AS "consented_sessions!"
           FROM generate_series($1::date, $2::date, interval '1 day') AS d
           ORDER BY 1"#,
        q.from,
        q.to,
        m
    )
    .fetch_all(&mut **tx)
    .await?;

    let metrics: BTreeMap<String, i64> = sqlx::query!(
        r#"SELECT metric AS "metric!", sum(value)::bigint AS "value!"
           FROM daily_metrics
           WHERE date BETWEEN $1 AND $2 AND dims = '{}'::jsonb
             AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY metric"#,
        q.from,
        q.to,
        m
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.metric, r.value))
    .collect();
    let metric = |k: &str| metrics.get(k).copied().unwrap_or(0);
    let by_template = sqlx::query_as!(
        TemplateRequests,
        r#"SELECT template AS "template!", sum(requests)::bigint AS "requests!"
           FROM analytics_counters
           WHERE day BETWEEN $1 AND $2 AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY template ORDER BY 2 DESC, 1"#,
        q.from,
        q.to,
        m
    )
    .fetch_all(&mut **tx)
    .await?;
    let sessions = metric("sessions");
    let traffic = Traffic {
        page_requests: by_template.iter().map(|t| t.requests).sum(),
        consented_sessions: sessions,
        consented_page_views: metric("page_views"),
        #[allow(clippy::cast_precision_loss)]
        conversion_rate: (sessions > 0).then(|| metric("funnel.purchase") as f64 / sessions as f64),
        funnel: [
            ("sessions", sessions),
            ("view_item", metric("funnel.view_item")),
            ("add_to_cart", metric("funnel.add_to_cart")),
            ("begin_checkout", metric("funnel.begin_checkout")),
            ("purchase", metric("funnel.purchase")),
        ]
        .into_iter()
        .map(|(step, sessions)| FunnelStep {
            step: step.into(),
            sessions,
        })
        .collect(),
        by_template,
    };

    let top_products = sqlx::query_as!(
        TopProduct,
        r#"SELECT l.product_id, min(l.name) AS "name!", o.currency AS "currency!",
                  sum(l.quantity)::bigint AS "units!", sum(l.total_minor)::bigint AS "revenue_minor!"
           FROM order_lines l JOIN orders o ON o.id = l.order_id
           WHERE o.placed_at >= $1 AND o.placed_at < $2 AND o.status <> 'cancelled'
             AND ($3::uuid IS NULL OR o.market_id = $3)
           GROUP BY l.product_id, (CASE WHEN l.product_id IS NULL THEN l.name END), o.currency
           ORDER BY 4 DESC, 5 DESC LIMIT $4"#,
        start,
        end,
        m,
        TOP
    )
    .fetch_all(&mut **tx)
    .await?;

    // Consented visitors' searches (never collected before consent, A20).
    let top_searches = sqlx::query_as!(
        QueryCount,
        r#"SELECT props->>'query' AS "query!", count(*) AS "count!"
           FROM events
           WHERE type = 'search' AND at >= $1 AND at < $2 AND props ? 'query'
             AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT $4"#,
        start,
        end,
        m,
        TOP
    )
    .fetch_all(&mut **tx)
    .await?;
    let zero_result_searches = sqlx::query_as!(
        QueryCount,
        r#"SELECT query AS "query!", sum(count)::bigint AS "count!"
           FROM search_zero_results
           WHERE day BETWEEN $1 AND $2
             AND ($3::uuid IS NULL OR locale = ANY (SELECT unnest(locales) FROM markets WHERE id = $3))
           GROUP BY query ORDER BY 2 DESC, 1 LIMIT $4"#,
        q.from,
        q.to,
        m,
        TOP
    )
    .fetch_all(&mut **tx)
    .await?;

    // Exact p75 over the range (sampled RUM keeps this small).
    let web_vitals = sqlx::query_as!(
        VitalP75,
        r#"SELECT props->>'template' AS "template!", props->>'name' AS "metric!",
                  percentile_cont(0.75) WITHIN GROUP (ORDER BY (props->>'value')::float8) AS "p75!",
                  count(*) AS "samples!"
           FROM events
           WHERE type = 'web_vitals' AND at >= $1 AND at < $2
             AND ($3::uuid IS NULL OR market_id = $3)
           GROUP BY 1, 2 ORDER BY 1, 2"#,
        start,
        end,
        m
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(Dashboard {
        from: q.from,
        to: q.to,
        market_id: m,
        sales,
        daily_sales,
        daily_traffic,
        traffic,
        top_products,
        top_searches,
        zero_result_searches,
        web_vitals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_event_keeps_only_allowlisted_props() {
        let p = "0197a8a0-0000-7000-8000-000000000001";
        let e = clean_event(&json!({
            "type": "view_item", "template": "product", "product_id": p,
            "email": "a@b.cz", "purposes": ["ads"]
        }))
        .unwrap();
        assert_eq!(e.kind, "view_item");
        assert_eq!(e.props, json!({ "template": "product", "product_id": p }));
        assert_eq!(
            clean_event(&json!({ "type": "page_view", "template": "<script>" }))
                .unwrap()
                .props,
            json!({ "template": "other" })
        );
        assert!(clean_event(&json!({ "type": "view_item" })).is_none());
        assert!(clean_event(&json!({ "type": "add_to_cart", "quantity": 2 })).is_none());
        assert!(clean_event(&json!({ "type": "login", "template": "home" })).is_none());
        assert!(clean_event(&json!("page_view")).is_none());
    }

    #[test]
    fn web_vitals_are_bounded() {
        let v = clean_event(
            &json!({ "type": "web_vital", "template": "home", "name": "LCP",
                                     "value": 1234.5, "rating": "good" }),
        )
        .unwrap();
        assert_eq!(v.kind, "web_vitals");
        assert_eq!(
            v.props,
            json!({ "template": "home", "name": "LCP", "value": 1234.5 })
        );
        for bad in [
            json!({ "type": "web_vital", "name": "FID", "value": 1 }),
            json!({ "type": "web_vital", "name": "LCP", "value": -1 }),
            json!({ "type": "web_vital", "name": "CLS", "value": 1000 }),
            json!({ "type": "web_vital", "name": "INP", "value": "fast" }),
        ] {
            assert!(clean_event(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn anon_ids_are_per_tenant_and_do_not_contain_the_subject() {
        let subject = "0123456789abcdef0123456789abcdef";
        let a = anon_id(Uuid::nil(), subject);
        assert_eq!(a.len(), 32);
        assert_ne!(a, subject);
        assert_ne!(a, anon_id(Uuid::from_u128(1), subject));
    }

    #[test]
    fn counters_accept_only_recent_days() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        assert!(plausible_day(today, today));
        assert!(plausible_day(today - Days::new(2), today));
        assert!(!plausible_day(today - Days::new(3), today));
        assert!(plausible_day(today + Days::new(1), today));
        assert!(!plausible_day(today + Days::new(2), today));
    }
}
