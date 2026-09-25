//! Effective-price intervals (A18): the materialized price timeline per (price list, variant).
//!
//! The timeline from "now" on is derived from the base price and the live sales by the pure
//! [`timeline`]; [`diff`] turns it into operations against the stored live intervals, so the
//! past is never rewritten and an unchanged timeline costs no writes. [`refresh`] runs both
//! for a scope of variants inside the caller's transaction and publishes `price.changed`
//! (immediately for the current price, via a scheduled `pricing.transition` job for future
//! sale starts/ends).

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, SubsecRound, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::money::{Currency, div_round_half_up, to_minor};
use crate::promotions::sales::{SaleDiscount, SaleTargets};

/// Job kind: publish `price.changed` for intervals starting at `payload.at`.
pub const TRANSITION_JOB: &str = "pricing.transition";

/// Why an interval has its price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The merchant's base price.
    #[default]
    Base,
    /// A sale (automatic discount) reduces the base price.
    Sale,
    /// A base price change made because a VAT rate changed.
    Tax,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Sale => "sale",
            Self::Tax => "tax",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "sale" => Self::Sale,
            "tax" => Self::Tax,
            _ => Self::Base,
        }
    }
}

/// A sale as it applies to one price list: fixed sales are already filtered by currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaleRule {
    pub id: Uuid,
    pub discount: SaleDiscount,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
}

impl SaleRule {
    fn active_at(&self, t: DateTime<Utc>) -> bool {
        self.starts_at <= t && self.ends_at.is_none_or(|e| e > t)
    }
}

/// The sale price of `base`: percent rounded half up to the minor unit, fixed floored at 0.
pub fn apply(discount: &SaleDiscount, base: i64) -> i64 {
    match discount {
        SaleDiscount::Percent { basis_points } => {
            let off = div_round_half_up(i128::from(base) * i128::from(*basis_points), 10_000);
            base - to_minor(off).unwrap_or(base)
        }
        SaleDiscount::Fixed { amount_minor, .. } => (base - amount_minor).max(0),
    }
}

/// One piece of a desired timeline. `to = None` is open-ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub from: DateTime<Utc>,
    pub to: Option<DateTime<Utc>>,
    pub amount_minor: i64,
    pub cause: Cause,
    pub sale_id: Option<Uuid>,
}

/// A stored interval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Interval {
    pub id: Uuid,
    pub amount_minor: i64,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    pub cause: Cause,
    pub sale_id: Option<Uuid>,
    /// The price arrived by import; nothing is known about the price before it.
    pub imported: bool,
}

/// The timeline from `now` on for base price `base`: at every sale boundary the lowest
/// resulting price of the active sales wins (ties: the lower sale id); without a reducing
/// sale the base price applies. `base_cause` labels the base segment starting at `now`
/// (`tax` for a VAT-driven change); later base segments are `base`. Adjacent segments with
/// the same price and sale are merged.
pub fn timeline(
    now: DateTime<Utc>,
    base: i64,
    base_cause: Cause,
    sales: &[SaleRule],
) -> Vec<Segment> {
    let mut points: BTreeSet<DateTime<Utc>> = BTreeSet::from([now]);
    for s in sales {
        points.extend(
            [Some(s.starts_at), s.ends_at]
                .into_iter()
                .flatten()
                .filter(|t| *t > now),
        );
    }
    let points: Vec<_> = points.into_iter().collect();
    let mut sorted: Vec<&SaleRule> = sales.iter().collect();
    sorted.sort_by_key(|s| s.id);
    let mut out: Vec<Segment> = Vec::new();
    for (i, from) in points.iter().enumerate() {
        let to = points.get(i + 1).copied();
        let best = sorted
            .iter()
            .filter(|s| s.active_at(*from))
            .map(|s| (apply(&s.discount, base), s.id))
            .fold(None::<(i64, Uuid)>, |best, cand| match best {
                Some(b) if b.0 <= cand.0 => Some(b),
                _ => Some(cand),
            })
            .filter(|(amount, _)| *amount < base);
        let (amount, cause, sale_id) = match best {
            Some((amount, id)) => (amount, Cause::Sale, Some(id)),
            None if *from == now => (base, base_cause, None),
            None => (base, Cause::Base, None),
        };
        match out.last_mut() {
            Some(last) if last.amount_minor == amount && last.sale_id == sale_id => last.to = to,
            _ => out.push(Segment {
                from: *from,
                to,
                amount_minor: amount,
                cause,
                sale_id,
            }),
        }
    }
    out
}

/// Writes that turn the stored live intervals into the desired timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ops {
    pub delete: Vec<Uuid>,
    /// `(id, new valid_to)`.
    pub set_valid_to: Vec<(Uuid, Option<DateTime<Utc>>)>,
    pub insert: Vec<Segment>,
}

impl Ops {
    pub fn is_empty(&self) -> bool {
        self.delete.is_empty() && self.set_valid_to.is_empty() && self.insert.is_empty()
    }
}

fn same_price(i: &Interval, s: &Segment) -> bool {
    i.amount_minor == s.amount_minor && i.sale_id == s.sale_id
}

/// Plans the writes. `live`: the stored intervals of one pair that end after `now` (or never),
/// sorted by `valid_from`; `desired`: [`timeline`] output (empty = no price any more).
/// The interval containing `now` is extended when it already has the desired price (keeping
/// its cause), otherwise closed at `now`; intervals starting after `now` are replaced.
pub fn diff(live: &[Interval], desired: &[Segment], now: DateTime<Utc>) -> Ops {
    let current = live.iter().find(|i| i.valid_from <= now);
    let future: Vec<&Interval> = live.iter().filter(|i| i.valid_from > now).collect();
    let mut ops = Ops::default();
    let rest = match (current, desired.first()) {
        (Some(c), Some(first)) if same_price(c, first) => {
            if c.valid_to != first.to {
                ops.set_valid_to.push((c.id, first.to));
            }
            &desired[1..]
        }
        (Some(c), _) => {
            if c.valid_from == now {
                ops.delete.push(c.id);
            } else {
                ops.set_valid_to.push((c.id, Some(now)));
            }
            desired
        }
        (None, _) => desired,
    };
    let unchanged_future = future.len() == rest.len()
        && future.iter().zip(rest).all(|(i, s)| {
            same_price(i, s) && i.valid_from == s.from && i.valid_to == s.to && i.cause == s.cause
        });
    if unchanged_future {
        return ops;
    }
    ops.delete.extend(future.iter().map(|i| i.id));
    ops.insert = rest.to_vec();
    ops
}

// ---------------------------------------------------------------------------------------
// Database

/// Which (price list, variant) pairs to recompute.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Every priced variant of the tenant (sale changes).
    All,
    /// These variants, in every price list or only in `price_list_id`.
    Variants {
        variant_ids: Vec<Uuid>,
        price_list_id: Option<Uuid>,
    },
}

/// A change of the current effective price made by [`refresh`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriceChange {
    pub price_list_id: Uuid,
    pub variant_id: Uuid,
    pub product_id: Uuid,
    pub currency: Currency,
    pub before_minor: Option<i64>,
    pub after_minor: Option<i64>,
    /// Cause and sale of the new price (none when the price was removed).
    pub cause: Option<Cause>,
    pub sale_id: Option<Uuid>,
}

/// Serializes pricing writes of one tenant (the timeline is read-modify-write).
/// ponytail: one lock per tenant; per-variant locks if bulk repricing ever contends.
pub(crate) async fn lock(tx: &mut TenantTx) -> Result<(), Error> {
    let key = format!("pricing:{}", tx.tenant_id());
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", key)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

struct PricedPair {
    price_list_id: Uuid,
    variant_id: Uuid,
    product_id: Uuid,
    currency: Currency,
    amount_minor: i64,
}

fn parse_currency(code: &str) -> Result<Currency, Error> {
    Currency::parse(code).ok_or_else(|| Error::Internal(format!("unknown currency {code}")))
}

/// Recomputes the timelines of `scope` from now on and writes the difference. Base segments
/// starting now get `base_cause`; with `imported`, the interval starting now of a pair without
/// a current price is marked `imported` (its earlier history is unknown). Returns the changes of the current price (also
/// published as `price.changed`).
pub async fn refresh(
    tx: &mut TenantTx,
    scope: &Scope,
    base_cause: Cause,
    imported: bool,
) -> Result<Vec<PriceChange>, Error> {
    lock(tx).await?;
    // "Now" is sampled under the lock: every interval another writer committed starts
    // before it, so nothing committed can be mistaken for a replaceable future interval.
    // Postgres keeps microseconds: compare like with like, or every run would look changed.
    let now = Utc::now().trunc_subsecs(6);
    let tenant_id = tx.tenant_id();
    let (all, variant_ids, list_filter) = match scope {
        Scope::All => (true, vec![], None),
        Scope::Variants {
            variant_ids,
            price_list_id,
        } => (false, variant_ids.clone(), *price_list_id),
    };
    let priced = sqlx::query!(
        "SELECT vp.price_list_id, vp.variant_id, v.product_id, pl.currency, vp.amount_minor
         FROM variant_prices vp
         JOIN price_lists pl ON pl.id = vp.price_list_id
         JOIN variants v ON v.id = vp.variant_id
         WHERE ($1 OR vp.variant_id = ANY($2)) AND ($3::uuid IS NULL OR vp.price_list_id = $3)",
        all,
        &variant_ids,
        list_filter
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(PricedPair {
            price_list_id: r.price_list_id,
            variant_id: r.variant_id,
            product_id: r.product_id,
            currency: parse_currency(&r.currency)?,
            amount_minor: r.amount_minor,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;

    let mut live: HashMap<(Uuid, Uuid), Vec<Interval>> = HashMap::new();
    let rows = sqlx::query!(
        "SELECT id, price_list_id, variant_id, amount_minor, valid_from, valid_to, cause, sale_id,
                imported
         FROM price_intervals
         WHERE (valid_to IS NULL OR valid_to > $1)
           AND ($2 OR variant_id = ANY($3)) AND ($4::uuid IS NULL OR price_list_id = $4)
         ORDER BY valid_from",
        now,
        all,
        &variant_ids,
        list_filter
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        live.entry((r.price_list_id, r.variant_id))
            .or_default()
            .push(Interval {
                id: r.id,
                amount_minor: r.amount_minor,
                valid_from: r.valid_from,
                valid_to: r.valid_to,
                cause: Cause::parse(&r.cause),
                sale_id: r.sale_id,
                imported: r.imported,
            });
    }

    // Category closure (each product's categories and their ancestors) for sale targeting.
    let product_ids: Vec<Uuid> = priced
        .iter()
        .map(|p| p.product_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let closure: HashMap<Uuid, Vec<Uuid>> = sqlx::query!(
        r#"WITH RECURSIVE up (product_id, category_id) AS (
               SELECT product_id, category_id FROM product_categories WHERE product_id = ANY($1)
               UNION
               SELECT up.product_id, c.parent_id FROM up
               JOIN categories c ON c.id = up.category_id
               WHERE c.parent_id IS NOT NULL
           )
           SELECT product_id AS "product_id!", array_agg(category_id) AS "category_ids!"
           FROM up GROUP BY product_id"#,
        &product_ids
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.product_id, r.category_ids))
    .collect();

    let sales = crate::promotions::sales::live(tx, now).await?;

    let mut ops_all = Ops::default();
    let mut inserts: Vec<(Uuid, Uuid, Segment, bool)> = Vec::new();
    let mut changes = Vec::new();
    let mut transitions: BTreeSet<DateTime<Utc>> = BTreeSet::new();
    let (no_categories, no_intervals) = (Vec::new(), Vec::new());
    let mut handled: BTreeSet<(Uuid, Uuid)> = BTreeSet::new();
    for p in &priced {
        let key = (p.price_list_id, p.variant_id);
        handled.insert(key);
        let categories = closure.get(&p.product_id).unwrap_or(&no_categories);
        let rules: Vec<SaleRule> = sales
            .iter()
            .filter(|s| targets_match(&s.targets, p.product_id, categories))
            .filter(|s| match &s.discount {
                SaleDiscount::Percent { .. } => true,
                SaleDiscount::Fixed { currency, .. } => *currency == p.currency,
            })
            .map(|s| SaleRule {
                id: s.id,
                discount: s.discount.clone(),
                starts_at: s.starts_at,
                ends_at: s.ends_at,
            })
            .collect();
        let desired = timeline(now, p.amount_minor, base_cause, &rules);
        let existing = live.get(&key).unwrap_or(&no_intervals);
        let ops = diff(existing, &desired, now);
        if ops.is_empty() {
            continue;
        }
        let before = existing
            .iter()
            .find(|i| i.valid_from <= now)
            .map(|i| i.amount_minor);
        let after = desired.first();
        if before != after.map(|s| s.amount_minor) {
            changes.push(PriceChange {
                price_list_id: p.price_list_id,
                variant_id: p.variant_id,
                product_id: p.product_id,
                currency: p.currency,
                before_minor: before,
                after_minor: after.map(|s| s.amount_minor),
                cause: after.map(|s| s.cause),
                sale_id: after.and_then(|s| s.sale_id),
            });
        }
        let mark_imported = imported && before.is_none();
        transitions.extend(ops.insert.iter().map(|s| s.from).filter(|t| *t > now));
        ops_all.delete.extend(ops.delete);
        ops_all.set_valid_to.extend(ops.set_valid_to);
        // Only the interval starting now marks where the known history begins; later
        // (scheduled) intervals are not new imports.
        inserts.extend(ops.insert.into_iter().map(|s| {
            let imported = mark_imported && s.from == now;
            (p.price_list_id, p.variant_id, s, imported)
        }));
    }
    // Pairs whose price was removed: close their timeline now.
    for (key, existing) in &live {
        if handled.contains(key) {
            continue;
        }
        let ops = diff(existing, &[], now);
        if let Some(before) = existing.iter().find(|i| i.valid_from <= now) {
            let meta = sqlx::query!(
                "SELECT v.product_id, pl.currency FROM variants v, price_lists pl
                 WHERE v.id = $1 AND pl.id = $2",
                key.1,
                key.0
            )
            .fetch_one(&mut **tx)
            .await?;
            changes.push(PriceChange {
                price_list_id: key.0,
                variant_id: key.1,
                product_id: meta.product_id,
                currency: parse_currency(&meta.currency)?,
                before_minor: Some(before.amount_minor),
                after_minor: None,
                cause: None,
                sale_id: None,
            });
        }
        ops_all.delete.extend(ops.delete);
        ops_all.set_valid_to.extend(ops.set_valid_to);
    }

    if !ops_all.delete.is_empty() {
        sqlx::query!(
            "DELETE FROM price_intervals WHERE id = ANY($1)",
            &ops_all.delete
        )
        .execute(&mut **tx)
        .await?;
    }
    if !ops_all.set_valid_to.is_empty() {
        let (ids, tos): (Vec<Uuid>, Vec<Option<DateTime<Utc>>>) =
            ops_all.set_valid_to.into_iter().unzip();
        sqlx::query!(
            "UPDATE price_intervals p SET valid_to = u.valid_to
             FROM UNNEST($1::uuid[], $2::timestamptz[]) AS u (id, valid_to)
             WHERE p.id = u.id",
            &ids,
            &tos as &[Option<DateTime<Utc>>]
        )
        .execute(&mut **tx)
        .await?;
    }
    if !inserts.is_empty() {
        let mut cols = InsertCols::default();
        for (list, variant, s, imported) in inserts {
            cols.ids.push(crate::id::new_id());
            cols.lists.push(list);
            cols.variants.push(variant);
            cols.amounts.push(s.amount_minor);
            cols.froms.push(s.from);
            cols.tos.push(s.to);
            cols.causes.push(s.cause.as_str().to_owned());
            cols.sales.push(s.sale_id);
            cols.imported.push(imported);
        }
        sqlx::query!(
            "INSERT INTO price_intervals (id, tenant_id, price_list_id, variant_id, amount_minor,
                                          valid_from, valid_to, cause, sale_id, imported)
             SELECT id, $1, list, variant, amount, valid_from, valid_to, cause, sale_id, imported
             FROM UNNEST($2::uuid[], $3::uuid[], $4::uuid[], $5::bigint[], $6::timestamptz[],
                         $7::timestamptz[], $8::text[], $9::uuid[], $10::bool[])
                  AS u (id, list, variant, amount, valid_from, valid_to, cause, sale_id, imported)",
            tenant_id,
            &cols.ids,
            &cols.lists,
            &cols.variants,
            &cols.amounts,
            &cols.froms,
            &cols.tos as &[Option<DateTime<Utc>>],
            &cols.causes,
            &cols.sales as &[Option<Uuid>],
            &cols.imported
        )
        .execute(&mut **tx)
        .await?;
    }

    for c in &changes {
        publish_change(tx, c, now).await?;
    }
    for at in transitions {
        let mut job = NewJob::new(TRANSITION_JOB, json!({ "at": at }));
        job.tenant_id = Some(tenant_id);
        job.run_at = Some(at);
        job.idempotency_key = Some(format!(
            "{TRANSITION_JOB}:{tenant_id}:{}",
            at.timestamp_micros()
        ));
        queue::enqueue(&mut **tx, &job).await?;
    }
    Ok(changes)
}

#[derive(Default)]
struct InsertCols {
    ids: Vec<Uuid>,
    lists: Vec<Uuid>,
    variants: Vec<Uuid>,
    amounts: Vec<i64>,
    froms: Vec<DateTime<Utc>>,
    tos: Vec<Option<DateTime<Utc>>>,
    causes: Vec<String>,
    sales: Vec<Option<Uuid>>,
    imported: Vec<bool>,
}

pub(crate) fn targets_match(t: &SaleTargets, product_id: Uuid, categories: &[Uuid]) -> bool {
    t.all
        || t.product_ids.contains(&product_id)
        || t.category_ids.iter().any(|c| categories.contains(c))
}

async fn publish_change(
    tx: &mut TenantTx,
    c: &PriceChange,
    effective_at: DateTime<Utc>,
) -> Result<(), Error> {
    platform::queue::publish(
        &mut **tx,
        "price.changed",
        &json!({
            "price_list_id": c.price_list_id,
            "variant_id": c.variant_id,
            "product_id": c.product_id,
            "currency": c.currency,
            "before_minor": c.before_minor,
            "after_minor": c.after_minor,
            "effective_at": effective_at,
            "cause": c.cause,
            "sale_id": c.sale_id,
        }),
    )
    .await?;
    Ok(())
}

/// The `pricing.transition` job: publishes `price.changed` for every interval that starts at
/// `at` (a scheduled sale start or end). A stale job (the sale was moved) finds nothing.
/// Replay-safe: a marker in the idempotency store, written in the same transaction as the
/// events, makes a retried job (crash after commit, before `complete`) publish nothing.
pub async fn publish_transitions(tx: &mut TenantTx, at: DateTime<Utc>) -> Result<usize, Error> {
    let key = at.timestamp_micros().to_string();
    if crate::idempotency::begin(tx, TRANSITION_JOB, &key, &key)
        .await?
        .is_some()
    {
        return Ok(0);
    }
    let rows = sqlx::query!(
        "SELECT n.price_list_id, n.variant_id, v.product_id, pl.currency, n.amount_minor, n.cause,
                n.sale_id, p.amount_minor AS \"before?\"
         FROM price_intervals n
         JOIN variants v ON v.id = n.variant_id
         JOIN price_lists pl ON pl.id = n.price_list_id
         LEFT JOIN price_intervals p ON p.price_list_id = n.price_list_id
              AND p.variant_id = n.variant_id AND p.valid_to = n.valid_from
         WHERE n.valid_from = $1",
        at
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in &rows {
        let change = PriceChange {
            price_list_id: r.price_list_id,
            variant_id: r.variant_id,
            product_id: r.product_id,
            currency: parse_currency(&r.currency)?,
            before_minor: r.before,
            after_minor: Some(r.amount_minor),
            cause: Some(Cause::parse(&r.cause)),
            sale_id: r.sale_id,
        };
        publish_change(tx, &change, at).await?;
    }
    crate::idempotency::finish(
        tx,
        TRANSITION_JOB,
        &key,
        200,
        &json!({ "published": rows.len() }),
    )
    .await?;
    Ok(rows.len())
}

/// The price in force at `at` for each of `variant_ids` in a price list (variants without a
/// price are absent). What the cart and the storefront charge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EffectivePrice {
    pub variant_id: Uuid,
    pub amount_minor: i64,
    pub cause: Cause,
    pub sale_id: Option<Uuid>,
    /// Until when this price holds (`None`: until further notice).
    pub valid_to: Option<DateTime<Utc>>,
}

pub async fn effective_prices(
    tx: &mut TenantTx,
    price_list_id: Uuid,
    variant_ids: &[Uuid],
    at: DateTime<Utc>,
) -> Result<Vec<EffectivePrice>, Error> {
    Ok(sqlx::query!(
        "SELECT variant_id, amount_minor, cause, sale_id, valid_to FROM price_intervals
         WHERE price_list_id = $1 AND variant_id = ANY($2)
           AND valid_from <= $3 AND (valid_to IS NULL OR valid_to > $3)
         ORDER BY variant_id",
        price_list_id,
        variant_ids,
        at
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| EffectivePrice {
        variant_id: r.variant_id,
        amount_minor: r.amount_minor,
        cause: Cause::parse(&r.cause),
        sale_id: r.sale_id,
        valid_to: r.valid_to,
    })
    .collect())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use proptest::prelude::*;

    use super::*;

    fn t(day: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
            .single()
            .unwrap_or_default()
            + Duration::days(day)
    }

    fn pct(n: u128, bp: u32, from: i64, to: Option<i64>) -> SaleRule {
        SaleRule {
            id: Uuid::from_u128(n),
            discount: SaleDiscount::Percent { basis_points: bp },
            starts_at: t(from),
            ends_at: to.map(t),
        }
    }

    fn seg(from: i64, to: Option<i64>, amount: i64, cause: Cause, sale: Option<u128>) -> Segment {
        Segment {
            from: t(from),
            to: to.map(t),
            amount_minor: amount,
            cause,
            sale_id: sale.map(Uuid::from_u128),
        }
    }

    fn stored(segments: &[Segment]) -> Vec<Interval> {
        segments
            .iter()
            .enumerate()
            .map(|(i, s)| Interval {
                id: Uuid::from_u128(1000 + i as u128),
                amount_minor: s.amount_minor,
                valid_from: s.from,
                valid_to: s.to,
                cause: s.cause,
                sale_id: s.sale_id,
                imported: false,
            })
            .collect()
    }

    #[test]
    fn sale_application() {
        assert_eq!(
            apply(&SaleDiscount::Percent { basis_points: 1500 }, 129_000),
            109_650
        );
        assert_eq!(
            apply(&SaleDiscount::Percent { basis_points: 3333 }, 1001),
            667
        );
        assert_eq!(
            apply(
                &SaleDiscount::Percent {
                    basis_points: 10_000
                },
                500
            ),
            0
        );
        let fixed = |a| SaleDiscount::Fixed {
            amount_minor: a,
            currency: Currency::Czk,
        };
        assert_eq!(apply(&fixed(100), 1000), 900);
        assert_eq!(apply(&fixed(5000), 1000), 0);
    }

    #[test]
    fn timeline_with_future_and_overlapping_sales() {
        // Base 1000; 10 % from day 5 to 10; 30 % from day 8 to 12.
        let sales = [pct(1, 1000, 5, Some(10)), pct(2, 3000, 8, Some(12))];
        assert_eq!(
            timeline(t(0), 1000, Cause::Base, &sales),
            vec![
                seg(0, Some(5), 1000, Cause::Base, None),
                seg(5, Some(8), 900, Cause::Sale, Some(1)),
                seg(8, Some(12), 700, Cause::Sale, Some(2)),
                seg(12, None, 1000, Cause::Base, None),
            ]
        );
        // Mid-sale, a tax-caused base change: the running sale still wins.
        assert_eq!(
            timeline(t(6), 2000, Cause::Tax, &sales)[0],
            seg(6, Some(8), 1800, Cause::Sale, Some(1))
        );
        assert_eq!(
            timeline(t(20), 1000, Cause::Tax, &sales),
            vec![seg(20, None, 1000, Cause::Tax, None)]
        );
        // Equal results: the lower id wins; an open-ended sale never ends.
        let tie = [pct(9, 1000, 0, None), pct(3, 1000, 0, None)];
        assert_eq!(
            timeline(t(0), 1000, Cause::Base, &tie),
            vec![seg(0, None, 900, Cause::Sale, Some(3))]
        );
        // A sale that reduces nothing (free item stays free) is not a sale interval.
        assert_eq!(
            timeline(t(0), 0, Cause::Base, &tie),
            vec![seg(0, None, 0, Cause::Base, None)]
        );
    }

    #[test]
    fn diff_extends_closes_and_replaces() {
        let now = t(3);
        // Stored: base since day 0 (open). Desired: same base until 5, then a sale.
        let live = stored(&[seg(0, None, 1000, Cause::Base, None)]);
        let desired = timeline(now, 1000, Cause::Base, &[pct(1, 1000, 5, None)]);
        let ops = diff(&live, &desired, now);
        assert_eq!(ops.set_valid_to, vec![(live[0].id, Some(t(5)))]);
        assert!(ops.delete.is_empty());
        assert_eq!(ops.insert, vec![seg(5, None, 900, Cause::Sale, Some(1))]);

        // Re-running with the stored result is a no-op.
        let after = stored(&[
            seg(0, Some(5), 1000, Cause::Base, None),
            seg(5, None, 900, Cause::Sale, Some(1)),
        ]);
        assert!(diff(&after, &desired, now).is_empty());

        // Sale deleted: the future sale interval goes, the current one is reopened.
        let ops = diff(&after, &timeline(now, 1000, Cause::Base, &[]), now);
        assert_eq!(ops.delete, vec![after[1].id]);
        assert_eq!(ops.set_valid_to, vec![(after[0].id, None)]);
        assert!(ops.insert.is_empty());

        // Base price change: close the current interval now, insert the new one.
        let ops = diff(&live, &timeline(now, 1200, Cause::Base, &[]), now);
        assert_eq!(ops.set_valid_to, vec![(live[0].id, Some(now))]);
        assert_eq!(ops.insert, vec![seg(3, None, 1200, Cause::Base, None)]);

        // An interval that started exactly now is replaced, not closed to zero length.
        let fresh = stored(&[seg(3, None, 1000, Cause::Base, None)]);
        let ops = diff(&fresh, &timeline(now, 1100, Cause::Base, &[]), now);
        assert_eq!(ops.delete, vec![fresh[0].id]);

        // Price removed.
        let ops = diff(&live, &[], now);
        assert_eq!(ops.set_valid_to, vec![(live[0].id, Some(now))]);
        assert!(ops.insert.is_empty());
        // Nothing stored, nothing wanted.
        assert!(diff(&[], &[], now).is_empty());
    }

    proptest! {
        /// Applying the ops yields a contiguous, non-overlapping history whose future equals
        /// the desired timeline, and the past (before now) is untouched.
        #[test]
        fn diff_then_apply_matches_timeline(
            base0 in 0i64..100_000, base1 in 0i64..100_000,
            sales0 in prop::collection::vec((1u32..10_000, 0i64..20, prop::option::of(1i64..20)), 0..4),
            sales1 in prop::collection::vec((1u32..10_000, 0i64..20, prop::option::of(1i64..20)), 0..4),
            now_day in 1i64..15,
        ) {
            let mk = |v: &[(u32, i64, Option<i64>)]| -> Vec<SaleRule> {
                v.iter().enumerate().map(|(i, (bp, s, len))| pct(i as u128 + 1, *bp, *s, len.map(|l| s + l))).collect()
            };
            // History created at day 0 with the first configuration.
            let first = timeline(t(0), base0, Cause::Base, &mk(&sales0));
            let mut rows = stored(&first);
            let now = t(now_day);
            let live: Vec<Interval> = rows.iter().filter(|i| i.valid_to.is_none_or(|e| e > now)).cloned().collect();
            let desired = timeline(now, base1, Cause::Base, &mk(&sales1));
            let ops = diff(&live, &desired, now);
            let past_before: Vec<Interval> = rows.iter().filter(|i| i.valid_to.is_some_and(|e| e <= now)).cloned().collect();
            rows.retain(|i| !ops.delete.contains(&i.id));
            for (id, to) in &ops.set_valid_to {
                if let Some(r) = rows.iter_mut().find(|r| r.id == *id) { r.valid_to = *to; }
            }
            rows.extend(stored(&ops.insert).into_iter().enumerate().map(|(k, mut i)| { i.id = Uuid::from_u128(5000 + k as u128); i }));
            rows.sort_by_key(|i| i.valid_from);
            // Contiguous and non-overlapping.
            for w in rows.windows(2) {
                prop_assert_eq!(w[0].valid_to, Some(w[1].valid_from));
            }
            prop_assert!(rows.last().unwrap().valid_to.is_none());
            // Past intact.
            for p in &past_before {
                prop_assert!(rows.contains(p));
            }
            // Price at every desired boundary equals the desired price.
            for s in &desired {
                let at = s.from;
                let r = rows.iter().find(|i| i.valid_from <= at && i.valid_to.is_none_or(|e| e > at)).unwrap();
                prop_assert_eq!(r.amount_minor, s.amount_minor);
                prop_assert_eq!(r.sale_id, s.sale_id);
            }
            // Idempotent: planning again changes nothing.
            let live2: Vec<Interval> = rows.iter().filter(|i| i.valid_to.is_none_or(|e| e > now)).cloned().collect();
            prop_assert!(diff(&live2, &desired, now).is_empty());
        }
    }
}
