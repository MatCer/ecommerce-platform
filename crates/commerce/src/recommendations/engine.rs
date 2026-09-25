//! The recommendation engine (spec §11.2): a chain of strategies per context, falling back to
//! bestsellers (and finally the newest products, so a shop without sales is never empty).
//!
//! Every candidate goes through the same filter: not excluded by the merchant, not the current
//! product or already in the cart, not a duplicate, sold in the market (active with a price in
//! the market's price list) and purchasable. The run is recorded as an [`Explained`] result,
//! which the storefront reduces to product cards and staff see in full ("why recommended").

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::settings::RecommendationSettings;
use super::{HALF_LIFE_DAYS, Strategy, WINDOW_DAYS};
use crate::markets::invalid;
use crate::storefront::Context;
use crate::storefront::cards::{self, ProductCard};

pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 24;
/// Recently viewed ids a device may send.
pub const MAX_RECENT: usize = 12;
/// Consented events an affinity is computed from (newest first).
const AFFINITY_EVENTS: i64 = 200;
/// Products a personalized ranking considers.
const PERSONAL_POOL: i64 = 200;
/// Candidate pages per strategy before moving on to the next one.
const MAX_ROUNDS: usize = 5;

/// What the recommendations are for (`context` query parameter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Product(Uuid),
    Category(Uuid),
    Collection(Uuid),
    Home,
    /// The visitor's cart (private variant only).
    Cart,
    /// Recently viewed product ids from the device. The history exists only while the
    /// visitor grants `personalization` (A20, kept on the device) and is sent without any
    /// identity, so the server never links it to anyone.
    Recent(Vec<Uuid>),
}

impl Target {
    /// `product:<id>`, `category:<id>`, `collection:<id>`, `home`, `cart` or `recent` (with
    /// `ids`, comma-separated, at most 12; invalid ids are dropped).
    pub fn parse(context: &str, ids: Option<&str>) -> Result<Self, Error> {
        let bad = || {
            invalid(
                "invalid_context",
                "context is product:<id>, category:<id>, collection:<id>, home, cart or recent",
            )
        };
        let id = |s: &str| Uuid::parse_str(s).map_err(|_| bad());
        Ok(match context.split_once(':') {
            Some(("product", rest)) => Self::Product(id(rest)?),
            Some(("category", rest)) => Self::Category(id(rest)?),
            Some(("collection", rest)) => Self::Collection(id(rest)?),
            None if context == "home" || context.is_empty() => Self::Home,
            None if context == "cart" => Self::Cart,
            None if context == "recent" => {
                let mut out: Vec<Uuid> = Vec::new();
                for s in ids.unwrap_or_default().split(',').take(MAX_RECENT * 2) {
                    if let Ok(u) = Uuid::parse_str(s.trim())
                        && !out.contains(&u)
                    {
                        out.push(u);
                    }
                }
                out.truncate(MAX_RECENT);
                Self::Recent(out)
            }
            _ => return Err(bad()),
        })
    }
}

/// Clamps a requested `limit` (default 8, 1-24).
pub fn limit(requested: Option<u32>) -> usize {
    requested
        .and_then(|l| usize::try_from(l).ok())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AffinityDim {
    Category,
    Brand,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AffinityScore {
    pub dim: AffinityDim,
    /// Category id or brand name.
    pub key: String,
    pub score: f64,
}

/// A visitor's interests (only ever loaded with `personalization` consent, A20).
#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
pub struct Affinity {
    pub scores: Vec<AffinityScore>,
    /// Products the visitor already looked at (ranked lower).
    pub seen: Vec<Uuid>,
}

impl Affinity {
    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

/// What the request knows about the visitor. The public variant is the default: no cart, no
/// consent.
#[derive(Debug, Clone, Default)]
pub struct Visitor {
    /// Products in the visitor's cart.
    pub cart: Vec<Uuid>,
    /// `personalization` granted, resolved on the server now.
    pub personalization: bool,
    pub affinity: Option<Affinity>,
}

/// Category/brand affinity from the anonymous visitor's own consented events: views count 1,
/// adds 3, decayed; only events recorded while `personalization` was granted are used.
pub async fn anon_affinity(
    tx: &mut TenantTx,
    anon_id: &str,
    now: DateTime<Utc>,
) -> Result<Affinity, Error> {
    let rows = sqlx::query!(
        r#"WITH ev AS (
               SELECT (props->>'product_id')::uuid AS pid,
                      CASE type WHEN 'add_to_cart' THEN 3.0 ELSE 1.0 END
                      * power(0.5, extract(epoch FROM $2 - at)::float8 / 86400.0 / $3) AS w
               FROM events
               WHERE anon_id = $1 AND type IN ('view_item', 'add_to_cart')
                 AND at > $2 - make_interval(days => $4::int) AND at <= $2
                 AND 'personalization' = ANY (consent_purposes)
                 AND props->>'product_id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
               ORDER BY at DESC LIMIT $5
           )
           SELECT 'category' AS "dim!", pc.category_id::text AS "key!", sum(ev.w)::float8 AS "score!"
           FROM ev JOIN product_categories pc ON pc.product_id = ev.pid
           GROUP BY pc.category_id
           UNION ALL
           SELECT 'brand', p.brand, sum(ev.w)::float8
           FROM ev JOIN products p ON p.id = ev.pid WHERE p.brand IS NOT NULL
           GROUP BY p.brand
           UNION ALL
           SELECT DISTINCT 'seen', ev.pid::text, 0::float8 FROM ev"#,
        anon_id,
        now,
        HALF_LIFE_DAYS,
        i32::try_from(WINDOW_DAYS).unwrap_or(90),
        AFFINITY_EVENTS
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Affinity::default();
    for r in rows {
        match r.dim.as_str() {
            "seen" => out.seen.extend(Uuid::parse_str(&r.key).ok()),
            dim => out.scores.push(AffinityScore {
                dim: if dim == "brand" {
                    AffinityDim::Brand
                } else {
                    AffinityDim::Category
                },
                key: r.key,
                score: r.score,
            }),
        }
    }
    Ok(out)
}

/// A customer's affinity from the rollup (`customer_affinity`, consent-filtered there).
pub async fn customer_affinity(tx: &mut TenantTx, customer_id: Uuid) -> Result<Affinity, Error> {
    let scores = sqlx::query!(
        "SELECT dim, key, score FROM customer_affinity WHERE customer_id = $1
         ORDER BY score DESC, key LIMIT 50",
        customer_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| AffinityScore {
        dim: if r.dim == "brand" {
            AffinityDim::Brand
        } else {
            AffinityDim::Category
        },
        key: r.key,
        score: r.score,
    })
    .collect();
    Ok(Affinity {
        scores,
        seen: Vec::new(),
    })
}

// ---------------------------------------------------------------------------------------
// Explained results

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The product the page is about.
    Current,
    InCart,
    /// On the merchant's exclusion list.
    Excluded,
    /// Already recommended by an earlier strategy.
    Duplicate,
    /// Not active, or no price in the market's price list.
    NotSold,
    OutOfStock,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ExplainedItem {
    pub product: ProductCard,
    pub strategy: Strategy,
    /// Strategy-specific: orders together, decayed units, affinity, collection position.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Skipped {
    pub product_id: Uuid,
    pub strategy: Strategy,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
pub struct Explained {
    /// The strategies tried, in order.
    pub chain: Vec<Strategy>,
    /// The strategy of the first product (what a theme titles the slot by).
    pub strategy: Option<Strategy>,
    /// A collection's heading when the first product comes from one.
    pub title: Option<String>,
    pub items: Vec<ExplainedItem>,
    pub skipped: Vec<Skipped>,
}

/// Display names of products (any status), preferring `locale`, for staff explanations of
/// skipped candidates.
pub async fn product_names(
    tx: &mut TenantTx,
    ids: &[Uuid],
    locale: &str,
) -> Result<std::collections::BTreeMap<Uuid, String>, Error> {
    Ok(sqlx::query!(
        "SELECT DISTINCT ON (product_id) product_id, name FROM product_translations
         WHERE product_id = ANY($1) ORDER BY product_id, (locale = $2) DESC, locale",
        ids,
        locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.product_id, r.name))
    .collect())
}

// ---------------------------------------------------------------------------------------
// Chain

/// Which products a bestsellers step ranks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scope {
    All,
    /// The categories the product is in.
    CategoriesOf(Uuid),
    /// The category and its subcategories.
    Subtree(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    BoughtTogether(Vec<Uuid>),
    Bestsellers(Scope),
    /// Open seasonal collections.
    Seasonal,
    /// This month's best sellers a year ago (strategy `seasonal`).
    LastYear,
    Collection(Uuid),
    Personalized,
    Recent(Vec<Uuid>),
    Newest,
}

impl Step {
    /// Read page by page (the others are complete lists read at once).
    fn paged(&self) -> bool {
        matches!(
            self,
            Self::BoughtTogether(_) | Self::Bestsellers(_) | Self::LastYear | Self::Newest
        )
    }

    fn strategy(&self) -> Strategy {
        match self {
            Self::BoughtTogether(_) => Strategy::BoughtTogether,
            Self::Bestsellers(_) => Strategy::Bestsellers,
            Self::Seasonal | Self::LastYear => Strategy::Seasonal,
            Self::Collection(_) => Strategy::Collection,
            Self::Personalized => Strategy::Personalized,
            Self::Recent(_) => Strategy::RecentlyViewed,
            Self::Newest => Strategy::Newest,
        }
    }
}

/// The strategies for a target, most specific first, ending in the bestsellers fallback.
fn chain(target: &Target, settings: &RecommendationSettings, visitor: &Visitor) -> Vec<Step> {
    let personal =
        visitor.personalization && visitor.affinity.as_ref().is_some_and(|a| !a.is_empty());
    let mut steps = match target {
        Target::Product(p) => vec![
            Step::BoughtTogether(vec![*p]),
            Step::Bestsellers(Scope::CategoriesOf(*p)),
        ],
        Target::Category(c) => vec![Step::Bestsellers(Scope::Subtree(*c))],
        Target::Collection(c) => vec![Step::Collection(*c)],
        Target::Home => {
            let mut s = Vec::new();
            if personal {
                s.push(Step::Personalized);
            }
            s.push(Step::Seasonal);
            s
        }
        Target::Cart => {
            let mut s = Vec::new();
            if !visitor.cart.is_empty() {
                s.push(Step::BoughtTogether(visitor.cart.clone()));
            }
            if personal {
                s.push(Step::Personalized);
            }
            s
        }
        // Not a recommendation: the device's own history (kept only with `personalization`,
        // A20), rehydrated without any identity, never padded with other products.
        Target::Recent(ids) => {
            return if settings.recently_viewed {
                vec![Step::Recent(ids.clone())]
            } else {
                Vec::new()
            };
        }
    };
    steps.push(Step::Bestsellers(Scope::All));
    // Last year's bestsellers of this month fill in where current sales cannot (a shop back
    // from a pause, a new season): they never push this year's best sellers aside.
    if *target == Target::Home {
        steps.push(Step::LastYear);
    }
    steps.push(Step::Newest);
    steps.retain(|s| settings.enabled(s.strategy()));
    steps
}

// ---------------------------------------------------------------------------------------
// Candidates

#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    id: Uuid,
    score: f64,
}

/// Ranks a personalized pool: the visitor's affinity (normalized per dimension) for the
/// product's categories and brand, a small bestseller bonus, and seen products halved.
fn rank_personalized(affinity: &Affinity, pool: Vec<PoolRow>) -> Vec<Candidate> {
    let max = |dim| {
        affinity
            .scores
            .iter()
            .filter(|s| s.dim == dim)
            .map(|s| s.score)
            .fold(0.0_f64, f64::max)
    };
    let (max_cat, max_brand) = (max(AffinityDim::Category), max(AffinityDim::Brand));
    let weight: HashMap<(AffinityDim, &str), f64> = affinity
        .scores
        .iter()
        .map(|s| {
            let m = if s.dim == AffinityDim::Brand {
                max_brand
            } else {
                max_cat
            };
            (
                (s.dim, s.key.as_str()),
                if m > 0.0 { s.score / m } else { 0.0 },
            )
        })
        .collect();
    let seen: HashSet<Uuid> = affinity.seen.iter().copied().collect();
    let mut out: Vec<Candidate> = pool
        .into_iter()
        .map(|p| {
            let cats: f64 = p
                .categories
                .iter()
                .map(|c| {
                    weight
                        .get(&(AffinityDim::Category, c.to_string().as_str()))
                        .copied()
                        .unwrap_or(0.0)
                })
                .fold(0.0, f64::max);
            let brand = p
                .brand
                .as_deref()
                .and_then(|b| weight.get(&(AffinityDim::Brand, b)).copied())
                .unwrap_or(0.0);
            let mut score = cats + brand + 0.05 * p.sales.ln_1p();
            if seen.contains(&p.id) {
                score *= 0.5;
            }
            Candidate { id: p.id, score }
        })
        .filter(|c| c.score > 0.0)
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    out
}

#[derive(Debug, Clone)]
struct PoolRow {
    id: Uuid,
    brand: Option<String>,
    categories: Vec<Uuid>,
    sales: f64,
}

async fn category_ids(tx: &mut TenantTx, scope: &Scope) -> Result<Option<Vec<Uuid>>, Error> {
    Ok(match scope {
        Scope::All => None,
        Scope::CategoriesOf(p) => Some(
            sqlx::query_scalar!(
                "SELECT category_id FROM product_categories WHERE product_id = $1",
                p
            )
            .fetch_all(&mut **tx)
            .await?,
        ),
        Scope::Subtree(c) => Some(
            sqlx::query_scalar!(
                r#"WITH RECURSIVE subtree AS (
                       SELECT id FROM categories WHERE id = $1
                       UNION ALL
                       SELECT c.id FROM categories c JOIN subtree s ON c.parent_id = s.id
                   )
                   SELECT id AS "id!" FROM subtree"#,
                c
            )
            .fetch_all(&mut **tx)
            .await?,
        ),
    })
}

/// First day of this month last year, and of the month after.
fn last_year_month(now: DateTime<Utc>) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::from_ymd_opt(now.year() - 1, now.month(), 1)?;
    Some((first, first.checked_add_months(Months::new(1))?))
}

/// Candidates of a step (ordered, best first), `n` from `offset` on, and the heading a
/// collection gives them. Lists that are complete in one read (collections, personalized,
/// recently viewed) answer only the first page.
async fn candidates(
    tx: &mut TenantTx,
    ctx: &Context,
    step: &Step,
    visitor: &Visitor,
    n: i64,
    offset: i64,
) -> Result<(Vec<Candidate>, Option<String>), Error> {
    if offset > 0 && !step.paged() {
        return Ok((Vec::new(), None));
    }
    let scored = |rows: Vec<(Uuid, f64)>| {
        rows.into_iter()
            .map(|(id, score)| Candidate { id, score })
            .collect::<Vec<_>>()
    };
    let positional = |ids: &[Uuid]| {
        ids.iter()
            .enumerate()
            .map(|(i, id)| Candidate {
                id: *id,
                score: (i + 1) as f64,
            })
            .collect::<Vec<_>>()
    };
    Ok(match step {
        Step::BoughtTogether(sources) => {
            let rows = sqlx::query!(
                r#"SELECT product_b AS "id!", sum(count_90d)::float8 AS "score!"
                   FROM co_purchases WHERE product_a = ANY($1)
                   GROUP BY product_b ORDER BY 2 DESC, 1 LIMIT $2 OFFSET $3"#,
                sources,
                n,
                offset
            )
            .fetch_all(&mut **tx)
            .await?;
            (
                scored(rows.into_iter().map(|r| (r.id, r.score)).collect()),
                None,
            )
        }
        Step::Bestsellers(scope) => {
            let cats = category_ids(tx, scope).await?;
            if cats.as_ref().is_some_and(Vec::is_empty) {
                return Ok((Vec::new(), None));
            }
            let rows = sqlx::query!(
                r#"SELECT s.product_id AS "id!", s.sales_score AS "score!"
                   FROM product_scores s JOIN products p ON p.id = s.product_id
                   WHERE s.market_id = $1 AND s.sales_score > 0 AND p.status = 'active'
                     AND ($2::uuid[] IS NULL OR EXISTS (
                         SELECT 1 FROM product_categories pc
                         WHERE pc.product_id = s.product_id AND pc.category_id = ANY($2)))
                   ORDER BY s.sales_score DESC, s.product_id LIMIT $3 OFFSET $4"#,
                ctx.market.id,
                cats.as_deref(),
                n,
                offset
            )
            .fetch_all(&mut **tx)
            .await?;
            (
                scored(rows.into_iter().map(|r| (r.id, r.score)).collect()),
                None,
            )
        }
        Step::Newest => {
            let ids = sqlx::query_scalar!(
                "SELECT id FROM products WHERE status = 'active'
                 ORDER BY created_at DESC, id LIMIT $1 OFFSET $2",
                n,
                offset
            )
            .fetch_all(&mut **tx)
            .await?;
            (
                ids.into_iter()
                    .map(|id| Candidate { id, score: 0.0 })
                    .collect(),
                None,
            )
        }
        Step::Collection(id) => {
            let row = sqlx::query!(
                "SELECT name, title_i18n, product_ids FROM collections
                 WHERE id = $1 AND (starts_at IS NULL OR starts_at <= $2)
                   AND (ends_at IS NULL OR ends_at > $2)",
                id,
                ctx.now
            )
            .fetch_optional(&mut **tx)
            .await?;
            match row {
                Some(r) => (
                    positional(&r.product_ids),
                    Some(ctx.text(&r.title_i18n).unwrap_or(r.name)),
                ),
                None => (Vec::new(), None),
            }
        }
        Step::Seasonal => {
            let open = sqlx::query!(
                "SELECT name, title_i18n, product_ids FROM collections
                 WHERE kind = 'seasonal' AND starts_at <= $1 AND ends_at > $1
                 ORDER BY starts_at DESC, id",
                ctx.now
            )
            .fetch_all(&mut **tx)
            .await?;
            let Some(first) = open.first() else {
                return Ok((Vec::new(), None));
            };
            let title = ctx.text(&first.title_i18n).unwrap_or(first.name.clone());
            let mut ids: Vec<Uuid> = Vec::new();
            for id in open.iter().flat_map(|c| c.product_ids.iter()) {
                if !ids.contains(id) {
                    ids.push(*id);
                }
            }
            (positional(&ids), Some(title))
        }
        Step::LastYear => {
            let Some((from, to)) = last_year_month(ctx.now) else {
                return Ok((Vec::new(), None));
            };
            let rows = sqlx::query!(
                r#"SELECT d.product_id AS "id!", sum(d.purchases)::float8 AS "score!"
                   FROM product_stats_daily d JOIN products p ON p.id = d.product_id
                   WHERE d.market_id = $1 AND d.date >= $2 AND d.date < $3
                     AND p.status = 'active'
                   GROUP BY d.product_id HAVING sum(d.purchases) > 0
                   ORDER BY 2 DESC, 1 LIMIT $4 OFFSET $5"#,
                ctx.market.id,
                from,
                to,
                n,
                offset
            )
            .fetch_all(&mut **tx)
            .await?;
            (
                scored(rows.into_iter().map(|r| (r.id, r.score)).collect()),
                None,
            )
        }
        Step::Personalized => {
            let Some(affinity) = visitor.affinity.as_ref().filter(|a| !a.is_empty()) else {
                return Ok((Vec::new(), None));
            };
            let mut brands = Vec::new();
            let mut cats = Vec::new();
            for s in &affinity.scores {
                match s.dim {
                    AffinityDim::Brand => brands.push(s.key.clone()),
                    AffinityDim::Category => cats.extend(Uuid::parse_str(&s.key).ok()),
                }
            }
            let pool = sqlx::query!(
                r#"SELECT p.id, p.brand, coalesce(s.sales_score, 0) AS "sales!",
                          array(SELECT pc.category_id FROM product_categories pc
                                WHERE pc.product_id = p.id) AS "categories!"
                   FROM products p
                   LEFT JOIN product_scores s ON s.product_id = p.id AND s.market_id = $1
                   WHERE p.status = 'active'
                     AND (p.brand = ANY($2) OR EXISTS (
                         SELECT 1 FROM product_categories pc
                         WHERE pc.product_id = p.id AND pc.category_id = ANY($3)))
                   ORDER BY 3 DESC, p.created_at DESC, p.id LIMIT $4"#,
                ctx.market.id,
                &brands,
                &cats,
                PERSONAL_POOL
            )
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| PoolRow {
                id: r.id,
                brand: r.brand,
                categories: r.categories,
                sales: r.sales,
            })
            .collect();
            (rank_personalized(affinity, pool), None)
        }
        Step::Recent(ids) => (positional(ids), None),
    })
}

// ---------------------------------------------------------------------------------------
// Pipeline

/// Runs the chain for `target` and explains every decision.
pub async fn recommend(
    tx: &mut TenantTx,
    ctx: &Context,
    settings: &RecommendationSettings,
    target: &Target,
    visitor: &Visitor,
    limit: usize,
) -> Result<Explained, Error> {
    let steps = chain(target, settings, visitor);
    let mut out = Explained {
        chain: steps.iter().map(Step::strategy).collect(),
        ..Explained::default()
    };
    let current = match target {
        Target::Product(p) => Some(*p),
        _ => None,
    };
    let cart: HashSet<Uuid> = visitor.cart.iter().copied().collect();
    let excluded: HashSet<Uuid> = settings.excluded_product_ids.iter().copied().collect();
    let mut taken: HashSet<Uuid> = HashSet::new();

    for step in &steps {
        let strategy = step.strategy();
        let mut offset = 0_i64;
        // Pages until the slot is full or the step runs dry; bounded, so a catalog full of
        // unsellable products cannot turn one request into a scan.
        for _ in 0..MAX_ROUNDS {
            if out.items.len() >= limit {
                break;
            }
            // Over-fetch: some candidates are filtered out below.
            let want =
                i64::try_from((limit - out.items.len()) * 3 + excluded.len() + cart.len() + 4)
                    .unwrap_or(i64::MAX);
            let (found, title) = candidates(tx, ctx, step, visitor, want, offset).await?;
            let exhausted = !step.paged() || i64::try_from(found.len()).unwrap_or(0) < want;
            offset = offset.saturating_add(want);
            let mut fresh: Vec<Candidate> = Vec::new();
            for c in found {
                let reason = if Some(c.id) == current {
                    Some(SkipReason::Current)
                } else if cart.contains(&c.id) {
                    Some(SkipReason::InCart)
                } else if excluded.contains(&c.id) {
                    Some(SkipReason::Excluded)
                } else if taken.contains(&c.id) || fresh.iter().any(|f| f.id == c.id) {
                    Some(SkipReason::Duplicate)
                } else {
                    None
                };
                match reason {
                    Some(reason) => out.skipped.push(Skipped {
                        product_id: c.id,
                        strategy,
                        reason,
                    }),
                    None => fresh.push(c),
                }
            }
            let ids: Vec<Uuid> = fresh.iter().map(|c| c.id).collect();
            let mut cards: HashMap<Uuid, ProductCard> = cards::cards(tx, ctx, &ids)
                .await?
                .into_iter()
                .map(|c| (c.id, c))
                .collect();
            for c in fresh {
                if out.items.len() >= limit {
                    break;
                }
                let reason = match cards.remove(&c.id) {
                    None => Some(SkipReason::NotSold),
                    Some(card) if !card.stock.purchasable() => Some(SkipReason::OutOfStock),
                    Some(card) => {
                        if out.items.is_empty() {
                            out.strategy = Some(strategy);
                            out.title.clone_from(&title);
                        }
                        taken.insert(c.id);
                        out.items.push(ExplainedItem {
                            product: card,
                            strategy,
                            score: c.score,
                        });
                        None
                    }
                };
                if let Some(reason) = reason {
                    out.skipped.push(Skipped {
                        product_id: c.id,
                        strategy,
                        reason,
                    });
                }
            }
            if exhausted {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn contexts_parse() {
        assert_eq!(
            Target::parse(&format!("product:{}", u(1)), None).unwrap(),
            Target::Product(u(1))
        );
        assert_eq!(
            Target::parse(&format!("category:{}", u(2)), None).unwrap(),
            Target::Category(u(2))
        );
        assert_eq!(Target::parse("", None).unwrap(), Target::Home);
        assert_eq!(Target::parse("cart", None).unwrap(), Target::Cart);
        for bad in ["product:nope", "foo", "product", "home:1", "cart:x"] {
            assert!(Target::parse(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn recent_ids_are_validated_and_bounded() {
        let many: Vec<String> = (1..=40).map(|i| u(i).to_string()).collect();
        let raw = format!("{},garbage,{}, ,{}", u(1), u(1), many.join(","));
        let Target::Recent(ids) = Target::parse("recent", Some(&raw)).unwrap() else {
            panic!("not recent");
        };
        assert_eq!(ids.len(), MAX_RECENT);
        assert_eq!(ids[0], u(1));
        assert_eq!(ids[1], u(2), "duplicates and garbage dropped");
        assert_eq!(
            Target::parse("recent", None).unwrap(),
            Target::Recent(vec![])
        );
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(limit(None), DEFAULT_LIMIT);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(4)), 4);
        assert_eq!(limit(Some(1000)), MAX_LIMIT);
    }

    fn strategies(steps: &[Step]) -> Vec<Strategy> {
        steps.iter().map(Step::strategy).collect()
    }

    #[test]
    fn chains_fall_back_to_bestsellers() {
        use Strategy::*;
        let s = RecommendationSettings::default();
        let public = Visitor::default();
        assert_eq!(
            strategies(&chain(&Target::Product(u(1)), &s, &public)),
            [BoughtTogether, Bestsellers, Bestsellers, Newest]
        );
        assert_eq!(
            strategies(&chain(&Target::Home, &s, &public)),
            [Seasonal, Bestsellers, Seasonal, Newest]
        );
        // The cart without products has nothing to be bought together with.
        assert_eq!(
            strategies(&chain(&Target::Cart, &s, &public)),
            [Bestsellers, Newest]
        );
        let consented = Visitor {
            cart: vec![u(9)],
            personalization: true,
            affinity: Some(Affinity {
                scores: vec![AffinityScore {
                    dim: AffinityDim::Brand,
                    key: "Lnen".into(),
                    score: 1.0,
                }],
                seen: vec![],
            }),
        };
        assert_eq!(
            strategies(&chain(&Target::Home, &s, &consented)),
            [Personalized, Seasonal, Bestsellers, Seasonal, Newest]
        );
        assert_eq!(
            strategies(&chain(&Target::Cart, &s, &consented)),
            [BoughtTogether, Personalized, Bestsellers, Newest]
        );
    }

    #[test]
    fn personalization_needs_consent_and_signals() {
        use Strategy::*;
        let s = RecommendationSettings::default();
        // An affinity without the consent flag is never used.
        let not_granted = Visitor {
            personalization: false,
            affinity: Some(Affinity {
                scores: vec![AffinityScore {
                    dim: AffinityDim::Brand,
                    key: "x".into(),
                    score: 1.0,
                }],
                seen: vec![],
            }),
            ..Visitor::default()
        };
        assert_eq!(
            strategies(&chain(&Target::Home, &s, &not_granted)),
            [Seasonal, Bestsellers, Seasonal, Newest]
        );
        // Recently viewed is the device's own list: no identity, no padding.
        let recent = Target::Recent(vec![u(1)]);
        assert_eq!(
            strategies(&chain(&recent, &s, &Visitor::default())),
            [RecentlyViewed]
        );
        let off = RecommendationSettings {
            recently_viewed: false,
            ..s.clone()
        };
        assert!(chain(&recent, &off, &Visitor::default()).is_empty());
    }

    #[test]
    fn disabled_strategies_are_skipped() {
        use Strategy::*;
        let s = RecommendationSettings {
            bought_together: false,
            bestsellers: false,
            ..RecommendationSettings::default()
        };
        assert_eq!(
            strategies(&chain(&Target::Product(u(1)), &s, &Visitor::default())),
            [Newest]
        );
    }

    #[test]
    fn personalized_ranking_prefers_affinity_and_demotes_seen() {
        let cat = u(100);
        let affinity = Affinity {
            scores: vec![
                AffinityScore {
                    dim: AffinityDim::Category,
                    key: cat.to_string(),
                    score: 4.0,
                },
                AffinityScore {
                    dim: AffinityDim::Brand,
                    key: "Lnen".into(),
                    score: 2.0,
                },
            ],
            seen: vec![u(3)],
        };
        let row = |id, brand: Option<&str>, cats: Vec<Uuid>, sales| PoolRow {
            id: u(id),
            brand: brand.map(str::to_owned),
            categories: cats,
            sales,
        };
        let ranked = rank_personalized(
            &affinity,
            vec![
                row(1, None, vec![cat], 0.0),
                row(2, Some("Lnen"), vec![cat], 0.0),
                row(3, Some("Lnen"), vec![cat], 50.0),
                row(4, Some("Other"), vec![u(7)], 100.0),
            ],
        );
        let ids: Vec<Uuid> = ranked.iter().map(|c| c.id).collect();
        // Category + brand beats category only; the seen product is halved; unrelated
        // products with only a bestseller bonus still rank, last.
        assert_eq!(ids, [u(2), u(3), u(1), u(4)]);
    }

    #[test]
    fn last_year_month_window() {
        let now = DateTime::parse_from_rfc3339("2026-12-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            last_year_month(now),
            Some((
                NaiveDate::from_ymd_opt(2025, 12, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
            ))
        );
    }
}
