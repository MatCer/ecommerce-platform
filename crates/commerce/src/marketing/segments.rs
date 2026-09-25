//! Segments (spec §11.5): an allowlisted rule builder compiled to parameterized SQL.
//!
//! Rules are a closed, typed set ([`Condition`], `deny_unknown_fields`): nothing from the
//! input ever becomes SQL text. [`push_conditions`] appends fixed fragments and binds every
//! value (`sqlx::QueryBuilder::push_bind`), so a value can only ever be compared, never run.
//! Members are always `subscribed` subscribers; purchase conditions look at the subscriber's
//! placed orders (same address, or the linked customer's), and affinity conditions only match
//! customers whose `personalization` consent is granted now (A20).

use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::markets::invalid;

pub const MAX_CONDITIONS: usize = 20;
pub const MAX_VALUES: usize = 50;
pub const SAMPLE: i64 = 10;
const CODE: &str = "invalid_rules";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    /// Every condition holds.
    #[default]
    All,
    /// At least one condition holds.
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AffinityDim {
    Category,
    Brand,
}

/// One allowlisted condition; `field` selects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "field", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    /// The subscriber's language is one of `locales` (`cs`, `sk`, `en`).
    Locale { locales: Vec<String> },
    /// Signed up on one of these markets.
    Market { market_ids: Vec<Uuid> },
    /// Subscribed (confirmed) within the window; at least one bound.
    Subscribed {
        #[serde(default)]
        after: Option<DateTime<Utc>>,
        #[serde(default)]
        before: Option<DateTime<Utc>>,
    },
    /// Bought a product of one of these categories.
    PurchasedCategory { category_ids: Vec<Uuid> },
    /// Bought a product of one of these brands.
    PurchasedBrand { brands: Vec<String> },
    /// Number of placed orders; at least one bound.
    OrderCount {
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
    },
    /// Sum of order totals in `currency` (minor units); at least one bound.
    TotalSpent {
        currency: String,
        #[serde(default)]
        min_minor: Option<i64>,
        #[serde(default)]
        max_minor: Option<i64>,
    },
    /// The newest order was placed within the window; at least one bound.
    LastOrder {
        #[serde(default)]
        after: Option<DateTime<Utc>>,
        #[serde(default)]
        before: Option<DateTime<Utc>>,
    },
    /// Clicked a campaign link in the last `days` (1-365).
    Engaged { days: i32 },
    /// Interest in categories (ids) or brands from the recommendation rollup (WP17); only for
    /// customers who grant `personalization`.
    Affinity { dim: AffinityDim, keys: Vec<String> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default, rename = "match")]
    pub match_: Match,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

fn values<T>(what: &str, v: &[T]) -> Result<(), Error> {
    if v.is_empty() || v.len() > MAX_VALUES {
        return Err(invalid(CODE, format!("{what}: 1-{MAX_VALUES} values")));
    }
    Ok(())
}

fn window<T: PartialOrd>(what: &str, lo: Option<&T>, hi: Option<&T>) -> Result<(), Error> {
    match (lo, hi) {
        (None, None) => Err(invalid(CODE, format!("{what}: set at least one bound"))),
        (Some(a), Some(b)) if a > b => Err(invalid(CODE, format!("{what}: bounds are reversed"))),
        _ => Ok(()),
    }
}

impl Rules {
    /// Validates the rules and returns them normalized (trimmed text, lower-case locales).
    pub fn normalized(&self) -> Result<Self, Error> {
        if self.conditions.len() > MAX_CONDITIONS {
            return Err(invalid(
                CODE,
                format!("at most {MAX_CONDITIONS} conditions"),
            ));
        }
        let mut out = Vec::with_capacity(self.conditions.len());
        for c in &self.conditions {
            out.push(match c {
                Condition::Locale { locales } => {
                    values("locale", locales)?;
                    let locales: Vec<String> =
                        locales.iter().map(|l| l.trim().to_lowercase()).collect();
                    if locales
                        .iter()
                        .any(|l| l.len() != 2 || !l.bytes().all(|b| b.is_ascii_lowercase()))
                    {
                        return Err(invalid(CODE, "locale: two-letter language codes"));
                    }
                    Condition::Locale { locales }
                }
                Condition::Market { market_ids } => {
                    values("market", market_ids)?;
                    c.clone()
                }
                Condition::Subscribed { after, before } => {
                    window("subscribed", after.as_ref(), before.as_ref())?;
                    c.clone()
                }
                Condition::PurchasedCategory { category_ids } => {
                    values("purchased_category", category_ids)?;
                    c.clone()
                }
                Condition::PurchasedBrand { brands } => {
                    values("purchased_brand", brands)?;
                    let brands: Vec<String> = brands.iter().map(|b| b.trim().to_owned()).collect();
                    if brands
                        .iter()
                        .any(|b| b.is_empty() || b.chars().count() > 200)
                    {
                        return Err(invalid(CODE, "purchased_brand: 1-200 characters each"));
                    }
                    Condition::PurchasedBrand { brands }
                }
                Condition::OrderCount { min, max } => {
                    window("order_count", min.as_ref(), max.as_ref())?;
                    if min.unwrap_or(0) < 0 || max.unwrap_or(0) < 0 {
                        return Err(invalid(CODE, "order_count: not negative"));
                    }
                    c.clone()
                }
                Condition::TotalSpent {
                    currency,
                    min_minor,
                    max_minor,
                } => {
                    window("total_spent", min_minor.as_ref(), max_minor.as_ref())?;
                    let currency = currency.trim().to_uppercase();
                    if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
                        return Err(invalid(CODE, "total_spent: an ISO currency code"));
                    }
                    Condition::TotalSpent {
                        currency,
                        min_minor: *min_minor,
                        max_minor: *max_minor,
                    }
                }
                Condition::LastOrder { after, before } => {
                    window("last_order", after.as_ref(), before.as_ref())?;
                    c.clone()
                }
                Condition::Engaged { days } => {
                    if !(1..=365).contains(days) {
                        return Err(invalid(CODE, "engaged: 1-365 days"));
                    }
                    c.clone()
                }
                Condition::Affinity { dim, keys } => {
                    values("affinity", keys)?;
                    let keys: Vec<String> = keys.iter().map(|k| k.trim().to_owned()).collect();
                    if keys.iter().any(|k| k.is_empty() || k.chars().count() > 200) {
                        return Err(invalid(CODE, "affinity: 1-200 characters each"));
                    }
                    Condition::Affinity { dim: *dim, keys }
                }
            });
        }
        Ok(Self {
            match_: self.match_,
            conditions: out,
        })
    }
}

/// The subscriber's own placed orders (`o`), for purchase conditions: same address, or the
/// linked customer's; unpaid (`pending`) and cancelled orders do not count.
const OWN_ORDERS: &str = "o.status NOT IN ('pending', 'cancelled') \
     AND (o.email = s.email OR (s.customer_id IS NOT NULL AND o.customer_id = s.customer_id))";

/// Appends ` AND (<conditions>)` for subscribers aliased `s` (nothing for no conditions).
/// Only fixed SQL is pushed; every value goes through `push_bind`.
pub fn push_conditions(qb: &mut QueryBuilder<Postgres>, rules: &Rules, now: DateTime<Utc>) {
    if rules.conditions.is_empty() {
        return;
    }
    qb.push(" AND (");
    let joiner = match rules.match_ {
        Match::All => " AND ",
        Match::Any => " OR ",
    };
    for (i, c) in rules.conditions.iter().enumerate() {
        if i > 0 {
            qb.push(joiner);
        }
        qb.push("(");
        push_condition(qb, c, now);
        qb.push(")");
    }
    qb.push(")");
}

fn push_bounds<T>(qb: &mut QueryBuilder<Postgres>, expr: &str, lo: Option<T>, hi: Option<T>)
where
    T: for<'t> sqlx::Encode<'t, Postgres> + sqlx::Type<Postgres>,
{
    let mut first = true;
    if let Some(lo) = lo {
        qb.push(expr).push(" >= ").push_bind(lo);
        first = false;
    }
    if let Some(hi) = hi {
        if !first {
            qb.push(" AND ");
        }
        qb.push(expr).push(" <= ").push_bind(hi);
    }
}

fn push_condition(qb: &mut QueryBuilder<Postgres>, c: &Condition, now: DateTime<Utc>) {
    match c {
        Condition::Locale { locales } => {
            qb.push("s.locale = ANY(")
                .push_bind(locales.clone())
                .push(")");
        }
        Condition::Market { market_ids } => {
            qb.push("s.market_id = ANY(")
                .push_bind(market_ids.clone())
                .push(")");
        }
        Condition::Subscribed { after, before } => {
            push_bounds(
                qb,
                "coalesce(s.confirmed_at, s.created_at)",
                *after,
                *before,
            );
        }
        Condition::PurchasedCategory { category_ids } => {
            qb.push(
                "EXISTS (SELECT 1 FROM orders o JOIN order_lines l ON l.order_id = o.id \
                 JOIN product_categories pc ON pc.product_id = l.product_id WHERE ",
            )
            .push(OWN_ORDERS)
            .push(" AND pc.category_id = ANY(")
            .push_bind(category_ids.clone())
            .push("))");
        }
        Condition::PurchasedBrand { brands } => {
            qb.push(
                "EXISTS (SELECT 1 FROM orders o JOIN order_lines l ON l.order_id = o.id \
                 JOIN products p ON p.id = l.product_id WHERE ",
            )
            .push(OWN_ORDERS)
            .push(" AND p.brand = ANY(")
            .push_bind(brands.clone())
            .push("))");
        }
        Condition::OrderCount { min, max } => {
            let expr = format!("(SELECT count(*) FROM orders o WHERE {OWN_ORDERS})");
            push_bounds(qb, &expr, *min, *max);
        }
        Condition::TotalSpent {
            currency,
            min_minor,
            max_minor,
        } => {
            // The currency is bound once per bound: the expression is repeated per bound.
            let mut first = true;
            for (op, v) in [(">=", min_minor), ("<=", max_minor)] {
                let Some(v) = v else { continue };
                if !first {
                    qb.push(" AND ");
                }
                first = false;
                qb.push("(SELECT coalesce(sum(o.total_minor), 0) FROM orders o WHERE ")
                    .push(OWN_ORDERS)
                    .push(" AND o.currency = ")
                    .push_bind(currency.clone())
                    .push(") ")
                    .push(op)
                    .push(" ")
                    .push_bind(*v);
            }
        }
        Condition::LastOrder { after, before } => {
            let expr = format!("(SELECT max(o.placed_at) FROM orders o WHERE {OWN_ORDERS})");
            push_bounds(qb, &expr, *after, *before);
        }
        Condition::Engaged { days } => {
            qb.push(
                "EXISTS (SELECT 1 FROM campaign_sends cs WHERE cs.subscriber_id = s.id \
                 AND cs.clicked_at >= ",
            )
            .push_bind(now - Duration::days(i64::from(*days)))
            .push(")");
        }
        Condition::Affinity { dim, keys } => {
            let dim = match dim {
                AffinityDim::Category => "category",
                AffinityDim::Brand => "brand",
            };
            qb.push(
                "s.customer_id IS NOT NULL AND EXISTS (SELECT 1 FROM customer_affinity a \
                 WHERE a.customer_id = s.customer_id AND a.dim = ",
            )
            .push_bind(dim)
            .push(" AND a.key = ANY(")
            .push_bind(keys.clone())
            .push(
                ")) AND (SELECT c.granted FROM consent_records c \
                 WHERE c.subject_type = 'customer' AND c.subject_id = s.customer_id::text \
                 AND c.purpose = 'personalization' ORDER BY c.at DESC, c.id DESC LIMIT 1) \
                 IS TRUE",
            );
        }
    }
}

/// `SELECT <columns> FROM subscribers s WHERE s.status = 'subscribed' AND (<rules>)`.
pub fn members(columns: &str, rules: &Rules, now: DateTime<Utc>) -> QueryBuilder<Postgres> {
    let mut qb = QueryBuilder::new(format!(
        "SELECT {columns} FROM subscribers s WHERE s.status = 'subscribed'"
    ));
    push_conditions(&mut qb, rules, now);
    qb
}

// ---------------------------------------------------------------------------------------
// Preview and CRUD

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SampleMember {
    pub id: Uuid,
    pub email: String,
    pub locale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Preview {
    /// Subscribed members right now.
    pub count: i64,
    /// Up to 10 of them.
    pub sample: Vec<SampleMember>,
}

/// Counts the current members of `rules` and returns a sample.
pub async fn preview(
    tx: &mut TenantTx,
    rules: &Rules,
    now: DateTime<Utc>,
) -> Result<Preview, Error> {
    let rules = rules.normalized()?;
    let count: i64 = members("count(*)", &rules, now)
        .build_query_scalar()
        .fetch_one(&mut **tx)
        .await?;
    let mut qb = members("s.id, s.email, s.locale", &rules, now);
    qb.push(" ORDER BY s.id LIMIT ").push_bind(SAMPLE);
    let sample = qb
        .build_query_as::<(Uuid, String, String)>()
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|(id, email, locale)| SampleMember { id, email, locale })
        .collect();
    Ok(Preview { count, sample })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SegmentInput {
    pub name: String,
    pub rules: Rules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Segment {
    pub id: Uuid,
    pub name: String,
    pub rules: Rules,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn validate(input: &SegmentInput) -> Result<(String, Rules), Error> {
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("invalid_name", "name must be 1-200 characters"));
    }
    Ok((name, input.rules.normalized()?))
}

fn from_row(
    id: Uuid,
    name: String,
    rules: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
) -> Result<Segment, Error> {
    Ok(Segment {
        id,
        name,
        rules: serde_json::from_value(rules).map_err(|e| Error::Internal(e.to_string()))?,
        created_at,
        updated_at,
    })
}

fn db_error(e: sqlx::Error) -> Error {
    if crate::unique_violation(&e) {
        return Error::Conflict {
            code: "name_taken",
            detail: "a segment with this name already exists".into(),
        };
    }
    if e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23503"))
    {
        return Error::Conflict {
            code: "segment_in_use",
            detail: "campaigns use this segment".into(),
        };
    }
    e.into()
}

pub async fn list(tx: &mut TenantTx) -> Result<Vec<Segment>, Error> {
    sqlx::query!("SELECT id, name, rules, created_at, updated_at FROM segments ORDER BY name")
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|r| from_row(r.id, r.name, r.rules, r.created_at, r.updated_at))
        .collect()
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Segment, Error> {
    let r = sqlx::query!(
        "SELECT id, name, rules, created_at, updated_at FROM segments WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    from_row(r.id, r.name, r.rules, r.created_at, r.updated_at)
}

fn rules_json(rules: &Rules) -> Result<serde_json::Value, Error> {
    serde_json::to_value(rules).map_err(|e| Error::Internal(e.to_string()))
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &SegmentInput,
) -> Result<Segment, Error> {
    let (name, rules) = validate(input)?;
    let json = rules_json(&rules)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO segments (tenant_id, name, rules) VALUES ($1, $2, $3) RETURNING id",
        tx.tenant_id(),
        name,
        json
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?;
    crate::audit::record(
        tx,
        actor,
        "segment.create",
        "segment",
        Some(&id.to_string()),
        &json!({ "name": name, "rules": json }),
    )
    .await?;
    get(tx, id).await
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &SegmentInput,
) -> Result<Segment, Error> {
    let (name, rules) = validate(input)?;
    let json = rules_json(&rules)?;
    let n = sqlx::query!(
        "UPDATE segments SET name = $2, rules = $3, updated_at = now() WHERE id = $1",
        id,
        name,
        json
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound);
    }
    crate::audit::record(
        tx,
        actor,
        "segment.update",
        "segment",
        Some(&id.to_string()),
        &json!({ "name": name, "rules": json }),
    )
    .await?;
    get(tx, id).await
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let n = sqlx::query!("DELETE FROM segments WHERE id = $1", id)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound);
    }
    crate::audit::record(
        tx,
        actor,
        "segment.delete",
        "segment",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(v: serde_json::Value) -> Result<Rules, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn rules_are_a_closed_set() {
        assert!(rules(json!({"conditions": [{"field": "sql", "value": "1=1"}]})).is_err());
        assert!(
            rules(json!({"conditions": [{"field": "locale", "locales": ["cs"], "raw": "x"}]}))
                .is_err(),
            "unknown keys are refused"
        );
        assert!(rules(json!({"match": "all", "where": "1=1"})).is_err());
        assert!(rules(json!({"match": "some"})).is_err());
        let ok = rules(json!({"match": "any", "conditions": [
            {"field": "locale", "locales": ["CS"]},
            {"field": "order_count", "min": 2}
        ]}))
        .unwrap()
        .normalized()
        .unwrap();
        assert_eq!(ok.match_, Match::Any);
        assert_eq!(
            ok.conditions[0],
            Condition::Locale {
                locales: vec!["cs".into()]
            }
        );
    }

    #[test]
    fn validation_rejects_bad_values() {
        let bad = [
            json!({"conditions": [{"field": "locale", "locales": ["cs'; DROP TABLE subscribers; --"]}]}),
            json!({"conditions": [{"field": "locale", "locales": []}]}),
            json!({"conditions": [{"field": "order_count"}]}),
            json!({"conditions": [{"field": "order_count", "min": 5, "max": 2}]}),
            json!({"conditions": [{"field": "total_spent", "currency": "K$", "min_minor": 1}]}),
            json!({"conditions": [{"field": "engaged", "days": 0}]}),
            json!({"conditions": [{"field": "purchased_brand", "brands": [" "]}]}),
        ];
        for b in bad {
            let r = rules(b.clone()).unwrap();
            assert!(r.normalized().is_err(), "{b}");
        }
    }

    #[test]
    fn values_are_bound_never_inlined() {
        let evil = "x' OR '1'='1'; DROP TABLE subscribers; --";
        let r = Rules {
            match_: Match::All,
            conditions: vec![
                Condition::Locale {
                    locales: vec!["cs".into()],
                },
                Condition::PurchasedBrand {
                    brands: vec![evil.into()],
                },
                Condition::Affinity {
                    dim: AffinityDim::Brand,
                    keys: vec![evil.into()],
                },
                Condition::TotalSpent {
                    currency: "CZK".into(),
                    min_minor: Some(1000),
                    max_minor: Some(9000),
                },
                Condition::OrderCount {
                    min: Some(1),
                    max: None,
                },
                Condition::Engaged { days: 30 },
            ],
        }
        .normalized()
        .unwrap();
        let qb = members("s.id", &r, Utc::now());
        let sql = qb.sql().as_str().to_owned();
        assert!(!sql.contains("DROP") && !sql.contains("'1'='1'") && !sql.contains("CZK"));
        assert!(!sql.contains("cs'") && !sql.contains("1000"));
        // 1 locale + 1 brand + 2 affinity + 2x(currency, bound) + 1 count + 1 engaged.
        assert_eq!(sql.matches('$').count(), 10, "{sql}");
        assert!(
            sql.starts_with("SELECT s.id FROM subscribers s WHERE s.status = 'subscribed' AND ((")
        );
    }

    #[test]
    fn empty_rules_select_every_subscriber() {
        let qb = members("count(*)", &Rules::default(), Utc::now());
        assert_eq!(
            qb.sql().as_str(),
            "SELECT count(*) FROM subscribers s WHERE s.status = 'subscribed'"
        );
    }

    #[test]
    fn any_joins_with_or() {
        let r = Rules {
            match_: Match::Any,
            conditions: vec![
                Condition::Engaged { days: 7 },
                Condition::Market {
                    market_ids: vec![Uuid::nil()],
                },
            ],
        };
        assert!(
            members("s.id", &r, Utc::now())
                .sql()
                .as_str()
                .contains(")) OR (s.market_id")
        );
    }
}
