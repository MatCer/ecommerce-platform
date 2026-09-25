//! Storefront query service (spec §11.1, A23).
//!
//! One Meilisearch multi-search per request:
//! - `hits`: all filters, `distinct = product_id`, the requested page and sort;
//! - `exact`: single-token queries only, the same filters plus `skus = q OR eans = q`, so exact
//!   SKU/EAN matches come first;
//! - `available`: all filters without `distinct` (one document per variant), facet values;
//! - one query per selected facet with that facet's own clause removed (disjunctive facets:
//!   values within a facet are OR-ed, so their availability ignores the facet's selection);
//! - `universe`: only the base filter (market, category, text), for the list of values shown.
//!
//! Filters apply to variant documents, so `color = red AND size = xl` only matches products
//! with one variant that is both (no cross-variant false matches). Facets report availability
//! only, never counts (A23). Hits are rehydrated from Postgres: products that are no longer
//! active, translated or priced in the market are dropped.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Utc;
use platform::Error;
use platform::db::TenantTx;
use platform::storage::Storage;
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::meili::{Meili, quote};
use super::{index_uid, lang, market_key};
use crate::catalog::code_valid;
use crate::markets::invalid;
use crate::media::AssetVariant;
use crate::money::{Currency, Locale, Money, MoneyView};

pub const MAX_PER_PAGE: u32 = 48;
pub const DEFAULT_PER_PAGE: u32 = 24;
/// Meilisearch `pagination.maxTotalHits`.
const MAX_HITS: u32 = 1000;
const MAX_FACETS: usize = 10;
const MAX_VALUES: usize = 20;
const MAX_QUERY_CHARS: usize = 200;
/// Typeahead: at most this many results in total, categories first (up to 3).
pub const SUGGEST_LIMIT: usize = 8;
const SUGGEST_CATEGORIES: usize = 3;
/// Exact SKU/EAN matches placed before the text matches.
const EXACT_LIMIT: u32 = 5;
/// Image variants small enough for result lists.
const THUMB_MAX_WIDTH: u32 = 640;

/// The storefront context of a request: tenant, market and locale (from the edge).
#[derive(Debug, Clone)]
pub struct Scope {
    pub tenant_id: Uuid,
    pub market_code: String,
    pub price_list_id: Option<Uuid>,
    pub currency: Currency,
    pub locale: String,
    /// The market's default locale: its text stands in for missing translations (WP13a).
    pub default_locale: String,
}

/// The storefront scope for a request context (tenant, market, locale from the edge), or
/// `None` unless the tenant is active, the market has a verified domain (so the shop is
/// public) and sells in `locale`. Until WP6's storefront token check exists this is what keeps
/// unpublished catalogs out of reach (fail closed).
pub async fn storefront_scope(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    market_id: Uuid,
    locale: &str,
) -> Result<Option<Scope>, Error> {
    let public = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM platform.domains d JOIN platform.tenants t ON t.id = d.tenant_id
               WHERE d.tenant_id = $1 AND d.market_id = $2 AND d.verified_at IS NOT NULL
                 AND t.status = 'active'
           ) AS "public!""#,
        tenant_id,
        market_id
    )
    .fetch_one(db)
    .await?;
    if !public {
        return Ok(None);
    }
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let scope = scope(&mut tx, market_id, locale).await?;
    tx.commit().await?;
    Ok(scope)
}

/// Loads the market for `market_id` if it sells in `locale`.
pub async fn scope(
    tx: &mut TenantTx,
    market_id: Uuid,
    locale: &str,
) -> Result<Option<Scope>, Error> {
    let Some(r) = sqlx::query!(
        "SELECT code, currency, price_list_id, locales, default_locale FROM markets WHERE id = $1",
        market_id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    if !r.locales.iter().any(|l| l == locale) {
        return Ok(None);
    }
    let currency = Currency::parse(&r.currency)
        .ok_or_else(|| Error::Internal(format!("market currency {}", r.currency)))?;
    Ok(Some(Scope {
        tenant_id: tx.tenant_id(),
        market_code: r.code,
        price_list_id: r.price_list_id,
        currency,
        locale: locale.to_owned(),
        default_locale: r.default_locale,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// Text relevance; for an empty query, popularity then newest.
    #[default]
    Relevance,
    PriceAsc,
    PriceDesc,
    Newest,
}

/// A facet key: `opt.<option code>`, `param.<parameter key>` or `brand`.
fn facet_key_valid(key: &str) -> bool {
    key == "brand"
        || key
            .strip_prefix("opt.")
            .or_else(|| key.strip_prefix("param."))
            .is_some_and(code_valid)
}

#[derive(Debug, Clone, Default)]
pub struct SearchRequest {
    pub q: String,
    /// Facet key → selected values (OR within a facet, AND across facets).
    pub filters: BTreeMap<String, Vec<String>>,
    pub category_id: Option<Uuid>,
    pub in_stock: bool,
    /// Gross price bounds in minor units, inclusive.
    pub price_min: Option<i64>,
    pub price_max: Option<i64>,
    pub sort: Sort,
    /// 1-based.
    pub page: u32,
    pub per_page: u32,
}

impl SearchRequest {
    /// Bounds everything that reaches Meilisearch (spec §14: validate at the boundary).
    pub fn validate(&self) -> Result<(), Error> {
        if self.q.chars().count() > MAX_QUERY_CHARS {
            return Err(invalid(
                "query_too_long",
                "the query is longer than 200 characters",
            ));
        }
        if self.filters.len() > MAX_FACETS {
            return Err(invalid(
                "too_many_filters",
                "at most 10 facets can be filtered",
            ));
        }
        for (key, values) in &self.filters {
            if !facet_key_valid(key) {
                return Err(invalid("invalid_filter", format!("unknown facet {key:?}")));
            }
            if values.is_empty()
                || values.len() > MAX_VALUES
                || values
                    .iter()
                    .any(|v| v.trim().is_empty() || v.chars().count() > 200)
            {
                return Err(invalid(
                    "invalid_filter",
                    format!("{key}: 1-20 values of 1-200 characters"),
                ));
            }
        }
        if !(1..=MAX_PER_PAGE).contains(&self.per_page) || self.page == 0 {
            return Err(invalid("invalid_page", "page >= 1 and per_page 1-48"));
        }
        if self.page.saturating_mul(self.per_page) > MAX_HITS + self.per_page {
            return Err(invalid(
                "invalid_page",
                "results beyond the first 1000 are not available",
            ));
        }
        if self.price_min.is_some_and(|p| p < 0) || self.price_max.is_some_and(|p| p < 0) {
            return Err(invalid(
                "invalid_price",
                "price bounds must not be negative",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SearchHit {
    pub product_id: Uuid,
    /// The variant that matched best (e.g. the red one when filtering by red).
    pub variant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub brand: Option<String>,
    /// Current price of the matched variant in the market (gross).
    pub price: MoneyView,
    /// At least one variant can be bought now.
    pub in_stock: bool,
    /// The first product image, in the list-sized variants (up to 640 px).
    pub image: Vec<AssetVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FacetValue {
    pub value: String,
    pub label: String,
    pub selected: bool,
    /// Selecting it would still match something. Unavailable values are shown disabled (A23).
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Facet {
    /// `opt.<code>`, `param.<key>` or `brand`; the filter key to send back.
    pub key: String,
    pub label: String,
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PriceRange {
    pub min_minor: i64,
    pub max_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SearchResult {
    pub items: Vec<SearchHit>,
    /// Matching products (before rehydration dropped any that just became unavailable).
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
    pub facets: Vec<Facet>,
    /// Price span of the products matching the query and category (without facet filters).
    pub price_range: Option<PriceRange>,
}

// ---------------------------------------------------------------------------------------
// Filter building (pure)

/// Clauses every query shares: sellable in the market, category, text-independent.
fn base_clauses(scope: &Scope, req: &SearchRequest) -> Vec<String> {
    let mut c = vec![format!("active_in_markets = {}", quote(&scope.market_code))];
    if let Some(cat) = req.category_id {
        c.push(format!("category_ids = {}", quote(&cat.to_string())));
    }
    c
}

/// Clauses from the shopper's refinements, keyed so one can be left out.
fn refinement_clauses<'r>(scope: &Scope, req: &'r SearchRequest) -> Vec<(Option<&'r str>, String)> {
    let price = format!("price.{}", market_key(&scope.market_code));
    let mut c = vec![];
    for (key, values) in &req.filters {
        let list: Vec<String> = values.iter().map(|v| quote(v.trim())).collect();
        c.push((
            Some(key.as_str()),
            format!("{key} IN [{}]", list.join(", ")),
        ));
    }
    if req.in_stock {
        c.push((None, "in_stock = true".to_owned()));
    }
    if let Some(min) = req.price_min {
        c.push((None, format!("{price} >= {min}")));
    }
    if let Some(max) = req.price_max {
        c.push((None, format!("{price} <= {max}")));
    }
    c
}

fn and(clauses: impl IntoIterator<Item = String>) -> String {
    clauses
        .into_iter()
        .map(|c| format!("({c})"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Facet key → values as `(value, selected, available)`, before labelling.
type RawFacets = Vec<(String, Vec<(String, bool, bool)>)>;

/// Which query of the multi-search answers what.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    queries: Vec<Value>,
    available: Option<usize>,
    /// facet key → query index
    per_facet: BTreeMap<String, usize>,
    universe: Option<usize>,
}

const FACET_PATTERNS: [&str; 3] = ["opt.*", "param.*", "brand"];

/// `skus = q OR eans = q` for a query that could be a SKU or EAN (one token, ≤ 64 chars).
fn exact_clause(q: &str) -> Option<String> {
    let raw = q.trim();
    (!raw.is_empty() && raw.chars().count() <= 64 && !raw.contains(char::is_whitespace))
        .then(|| format!("skus = {0} OR eans = {0}", quote(raw)))
}

/// The multi-search for `req`. `exclude`: products already placed first (exact SKU/EAN
/// matches), left out of the text hits so the combined list has no duplicates.
fn plan(scope: &Scope, req: &SearchRequest, facets: bool, exclude: &[Uuid]) -> Plan {
    let uid = index_uid(scope.tenant_id, &scope.locale);
    let q = lang::analyze(&req.q, &scope.locale);
    let base = base_clauses(scope, req);
    let refinements = refinement_clauses(scope, req);
    let all = and(base
        .iter()
        .cloned()
        .chain(refinements.iter().map(|(_, c)| c.clone())));
    let price = format!("price.{}", market_key(&scope.market_code));
    let sort: Vec<String> = match req.sort {
        Sort::PriceAsc => vec![format!("{price}:asc")],
        Sort::PriceDesc => vec![format!("{price}:desc")],
        Sort::Newest => vec!["created_at:desc".into()],
        Sort::Relevance if q.is_empty() => {
            vec!["popularity:desc".into(), "created_at:desc".into()]
        }
        Sort::Relevance => vec![],
    };
    let hits_filter = if exclude.is_empty() {
        all.clone()
    } else {
        let ids: Vec<String> = exclude.iter().map(|id| quote(&id.to_string())).collect();
        format!("{all} AND (NOT product_id IN [{}])", ids.join(", "))
    };
    let mut p = Plan::default();
    p.queries.push(json!({
        "indexUid": uid, "q": q, "filter": hits_filter, "sort": sort,
        "page": req.page, "hitsPerPage": req.per_page,
        "attributesToRetrieve": ["id", "product_id"],
    }));
    if !facets {
        return p;
    }
    let facet_query = |filter: String, facets: Vec<String>| {
        json!({
            "indexUid": uid, "q": q, "filter": filter, "limit": 0,
            // One document per variant: availability must see every variant (A23).
            "distinct": "id", "facets": facets,
        })
    };
    let patterns: Vec<String> = FACET_PATTERNS.iter().map(|s| (*s).to_owned()).collect();
    if refinements.is_empty() {
        p.universe = Some(p.queries.len());
        p.available = p.universe;
        let mut f = patterns;
        f.push(price);
        p.queries.push(facet_query(all, f));
        return p;
    }
    p.available = Some(p.queries.len());
    p.queries.push(facet_query(all, patterns.clone()));
    for key in req.filters.keys() {
        let without = and(base.iter().cloned().chain(
            refinements
                .iter()
                .filter(|(k, _)| *k != Some(key.as_str()))
                .map(|(_, c)| c.clone()),
        ));
        p.per_facet.insert(key.clone(), p.queries.len());
        p.queries.push(facet_query(without, vec![key.clone()]));
    }
    p.universe = Some(p.queries.len());
    let mut f = patterns;
    f.push(price);
    p.queries.push(facet_query(and(base), f));
    p
}

/// facet key → values present in a result's `facetDistribution`.
fn distribution(result: Option<&Value>) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    let Some(Value::Object(d)) = result.and_then(|r| r.get("facetDistribution")) else {
        return out;
    };
    for (key, values) in d {
        if !facet_key_valid(key) {
            continue;
        }
        if let Value::Object(values) = values {
            out.insert(key.clone(), values.keys().cloned().collect());
        }
    }
    out
}

/// Facets with availability: values come from the universe; a value is available when the
/// query for its facet (the full filter, or the filter without the facet's own clause if the
/// facet is refined) still has it.
fn facets_from(plan: &Plan, results: &[Value], req: &SearchRequest) -> RawFacets {
    let mut universe = distribution(plan.universe.and_then(|i| results.get(i)));
    // Selected values are always listed, even past the engine's per-facet value cap.
    // ponytail: facets over `MAX_FACET_VALUES` (300) values are cut there; add facet search
    // when a catalog needs more.
    for (key, values) in &req.filters {
        let listed = universe.entry(key.clone()).or_default();
        listed.extend(values.iter().map(|v| v.trim().to_owned()));
    }
    let available = distribution(plan.available.and_then(|i| results.get(i)));
    let mut out = vec![];
    for (key, values) in universe {
        let reachable = match plan.per_facet.get(&key) {
            Some(i) => distribution(results.get(*i))
                .remove(&key)
                .unwrap_or_default(),
            None => available.get(&key).cloned().unwrap_or_default(),
        };
        let selected: BTreeSet<&str> = req
            .filters
            .get(&key)
            .map(|v| v.iter().map(|s| s.trim()).collect())
            .unwrap_or_default();
        let values = values
            .into_iter()
            .map(|v| {
                let sel = selected.contains(v.as_str());
                // A selection missing from a distribution cut at the cap is unknown, not
                // unavailable: never disable what the shopper selected on that guess.
                let capped = reachable.len() >= super::index::MAX_FACET_VALUES;
                let avail = reachable.contains(&v) || (sel && capped);
                (v, sel, avail)
            })
            .collect();
        out.push((key, values));
    }
    out
}

fn price_range(plan: &Plan, results: &[Value], scope: &Scope) -> Option<PriceRange> {
    let key = format!("price.{}", market_key(&scope.market_code));
    let stats = plan
        .universe
        .and_then(|i| results.get(i))?
        .get("facetStats")?
        .get(&key)?;
    Some(PriceRange {
        min_minor: stats.get("min")?.as_f64()? as i64,
        max_minor: stats.get("max")?.as_f64()? as i64,
    })
}

/// Page `page` of the list `exact ++ text` (text hits exclude the exact products), from
/// text page `page - 1` (`previous`, only needed past page 1 when there are exact hits) and
/// text page `page` (`current`). Requires `exact.len() <= per_page`.
fn page_of<T: Clone>(
    exact: &[T],
    previous: Option<Vec<T>>,
    current: Vec<T>,
    page: u32,
    per_page: u32,
) -> Vec<T> {
    let (k, n) = (exact.len(), per_page as usize);
    let head: Vec<T> = match (k, page) {
        (0, _) => vec![],
        (_, 1) => exact.to_vec(),
        _ => previous
            .unwrap_or_default()
            .into_iter()
            .skip(n - k.min(n))
            .collect(),
    };
    head.into_iter().chain(current).take(n).collect()
}

/// `(variant id, product id)` of the hits, in order.
fn hit_ids(result: Option<&Value>) -> Vec<(Uuid, Uuid)> {
    result
        .and_then(|r| r.get("hits"))
        .and_then(Value::as_array)
        .map(|hits| {
            hits.iter()
                .filter_map(|h| {
                    let id = |k: &str| h.get(k)?.as_str().and_then(|s| Uuid::parse_str(s).ok());
                    Some((id("id")?, id("product_id")?))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// Service

/// Runs a search. Meilisearch failures surface as [`Error::Unavailable`] (search is degraded,
/// the shop keeps working).
pub async fn search(
    tx: &mut TenantTx,
    meili: &Meili,
    storage: &Storage,
    scope: &Scope,
    req: &SearchRequest,
) -> Result<SearchResult, Error> {
    req.validate()?;
    // Exact SKU/EAN first (spec §11.1): products with a variant whose SKU or EAN equals the
    // query (and that pass the refinements) lead the list, then the text matches without them.
    let per_page = req.per_page;
    let exact: Vec<(Uuid, Uuid)> = match exact_clause(&req.q) {
        Some(clause) => {
            let filter = and(base_clauses(scope, req)
                .into_iter()
                .chain(refinement_clauses(scope, req).into_iter().map(|(_, c)| c))
                .chain([clause]));
            let probe = json!({
                "indexUid": index_uid(scope.tenant_id, &scope.locale), "q": "", "filter": filter,
                // At most one page, so the text hits of any page come from two adjacent pages.
                "limit": EXACT_LIMIT.min(per_page),
                "attributesToRetrieve": ["id", "product_id"],
            });
            hit_ids(meili.multi_search(&[probe]).await?.first())
        }
        None => vec![],
    };
    let exact_products: Vec<Uuid> = exact.iter().map(|(_, p)| *p).collect();
    let k = u32::try_from(exact.len()).unwrap_or(per_page);
    let mut plan = plan(scope, req, true, &exact_products);
    // Page p of [exact.., text..] holds text items [(p-1)·n - k, p·n - k): the tail of text
    // page p-1 and the head of text page p.
    let previous = (k > 0 && req.page > 1).then(|| {
        let mut q = plan.queries[0].clone();
        q["page"] = json!(req.page - 1);
        plan.queries.push(q);
        plan.queries.len() - 1
    });
    let results = meili.multi_search(&plan.queries).await?;
    let hits = results.first();
    let text_total = hits
        .and_then(|h| h.get("totalHits"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = text_total + u64::from(k);
    let total_pages = u32::try_from(total.div_ceil(u64::from(per_page))).unwrap_or(u32::MAX);
    let ids = page_of(
        &exact,
        previous.map(|i| hit_ids(results.get(i))),
        hit_ids(hits),
        req.page,
        per_page,
    );

    let items = rehydrate(tx, storage, scope, req, &ids).await?;

    let raw_facets = facets_from(&plan, &results, req);
    let facets = label_facets(tx, &scope.locale, raw_facets).await?;
    let no_refinements = req.filters.is_empty()
        && !req.in_stock
        && req.price_min.is_none()
        && req.price_max.is_none();
    if total == 0 && req.page == 1 && no_refinements && !req.q.trim().is_empty() {
        record_zero_result(tx, &scope.locale, &req.q).await?;
    }
    Ok(SearchResult {
        items,
        total,
        page: req.page,
        per_page: req.per_page,
        total_pages,
        facets,
        price_range: price_range(&plan, &results, scope),
    })
}

/// A variant's current sellable state (rehydration).
struct Current {
    id: Uuid,
    price: i64,
    in_stock: bool,
    options: BTreeMap<String, String>,
}

impl Current {
    /// The request's option, stock and price filters still hold for this variant now.
    /// (Parameter and brand filters change only with a product edit, which reindexes it.)
    fn satisfies(&self, req: &SearchRequest) -> bool {
        let options_ok = req.filters.iter().all(|(key, values)| {
            key.strip_prefix("opt.").is_none_or(|code| {
                self.options
                    .get(code)
                    .is_some_and(|v| values.iter().any(|s| s.trim().eq_ignore_ascii_case(v)))
            })
        });
        options_ok
            && (!req.in_stock || self.in_stock)
            && req.price_min.is_none_or(|min| self.price >= min)
            && req.price_max.is_none_or(|max| self.price <= max)
    }
}

/// Current storefront data for `ids` (variant, product), in order. Products that are no
/// longer active, translated or priced in the market, and hits whose matched variant no
/// longer satisfies `req`, are left out.
async fn rehydrate(
    tx: &mut TenantTx,
    storage: &Storage,
    scope: &Scope,
    req: &SearchRequest,
    ids: &[(Uuid, Uuid)],
) -> Result<Vec<SearchHit>, Error> {
    let Some(price_list) = scope.price_list_id else {
        return Ok(vec![]);
    };
    let products: Vec<Uuid> = ids.iter().map(|(_, p)| *p).collect();
    let reps: Vec<Uuid> = ids.iter().map(|(v, _)| *v).collect();
    let rows: HashMap<Uuid, (String, String, Option<String>)> = sqlx::query!(
        r#"SELECT p.id, t.name AS "name!", t.slug AS "slug!", p.brand
           FROM products p
           CROSS JOIN LATERAL (
               SELECT name, slug FROM product_translations pt WHERE pt.product_id = p.id
               ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1
           ) t
           WHERE p.id = ANY($1) AND p.status = 'active'"#,
        &products,
        scope.locale,
        scope.default_locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, (r.name, r.slug, r.brand)))
    .collect();

    // product → variants priced in the market now.
    let mut variants: HashMap<Uuid, Vec<Current>> = HashMap::new();
    for r in sqlx::query!(
        r#"SELECT v.product_id, v.id, v.option_values, pi.amount_minor,
                  (NOT COALESCE(l.track, true) OR COALESCE(l.allow_backorder, false)
                   OR COALESCE(l.on_hand - l.reserved, 0) > 0) AS "in_stock!"
           FROM variants v
           JOIN price_intervals pi ON pi.variant_id = v.id AND pi.price_list_id = $2
                AND pi.valid_from <= $3 AND (pi.valid_to IS NULL OR pi.valid_to > $3)
           LEFT JOIN inventory_levels l ON l.variant_id = v.id
           WHERE v.product_id = ANY($1)
           ORDER BY v.product_id, v.position"#,
        &products,
        price_list,
        Utc::now()
    )
    .fetch_all(&mut **tx)
    .await?
    {
        variants.entry(r.product_id).or_default().push(Current {
            id: r.id,
            price: r.amount_minor,
            in_stock: r.in_stock,
            options: serde_json::from_value(r.option_values).unwrap_or_default(),
        });
    }

    let mut images: HashMap<Uuid, Vec<AssetVariant>> = HashMap::new();
    for r in sqlx::query!(
        "SELECT DISTINCT ON (pm.product_id) pm.product_id, a.variants
         FROM product_media pm JOIN assets a ON a.id = pm.asset_id AND a.status = 'ready'
         WHERE pm.product_id = ANY($1) AND (pm.variant_id IS NULL OR pm.variant_id = ANY($2))
         ORDER BY pm.product_id, (pm.variant_id IS NULL), pm.position",
        &products,
        &reps
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let all: Vec<AssetVariant> = serde_json::from_value(r.variants).unwrap_or_default();
        images.insert(
            r.product_id,
            all.into_iter()
                .filter(|v| v.width <= THUMB_MAX_WIDTH)
                .map(|mut v| {
                    v.url = storage.media_url(&v.key);
                    v
                })
                .collect(),
        );
    }

    let locale = Locale::from_tag(&scope.locale);
    Ok(ids
        .iter()
        .filter_map(|(variant, product)| {
            let (name, slug, brand) = rows.get(product)?.clone();
            let priced = variants.get(product)?;
            // The matched variant, only if it still satisfies the request now; the index
            // catches up with the change shortly (never substitute another variant).
            let matched = priced
                .iter()
                .find(|v| v.id == *variant)
                .filter(|v| v.satisfies(req))?;
            Some(SearchHit {
                product_id: *product,
                variant_id: matched.id,
                name,
                slug,
                brand,
                price: Money::new(matched.price, scope.currency).view(locale),
                in_stock: priced.iter().any(|v| v.in_stock),
                image: images.get(product).cloned().unwrap_or_default(),
            })
        })
        .collect())
}

/// Labels for facet keys and values in `locale` (fallback: the code itself).
/// ponytail: scans the options of every product using the codes; cache per tenant if big
/// catalogs make this slow.
async fn label_facets(
    tx: &mut TenantTx,
    locale: &str,
    raw: RawFacets,
) -> Result<Vec<Facet>, Error> {
    let options: Vec<String> = raw
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("opt.").map(str::to_owned))
        .collect();
    let params: Vec<String> = raw
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("param.").map(str::to_owned))
        .collect();
    let pick = |names: &Value| -> Option<String> {
        names.get(locale).and_then(Value::as_str).map(str::to_owned)
    };
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut value_labels: HashMap<(String, String), String> = HashMap::new();
    if !options.is_empty() {
        for r in sqlx::query!(
            r#"SELECT o.code, o.name_i18n, v.value->>'code' AS "value?", v.value->'name_i18n' AS "value_names?"
               FROM product_options o
               CROSS JOIN LATERAL jsonb_array_elements(o."values") AS v(value)
               WHERE o.code = ANY($1)"#,
            &options
        )
        .fetch_all(&mut **tx)
        .await?
        {
            let key = format!("opt.{}", r.code);
            if let Some(l) = pick(&r.name_i18n) {
                labels.entry(key.clone()).or_insert(l);
            }
            if let (Some(value), Some(l)) = (r.value, r.value_names.as_ref().and_then(pick)) {
                value_labels.entry((key, value)).or_insert(l);
            }
        }
    }
    if !params.is_empty() {
        for r in sqlx::query!(
            "SELECT key, name_i18n FROM parameters WHERE key = ANY($1)",
            &params
        )
        .fetch_all(&mut **tx)
        .await?
        {
            if let Some(l) = pick(&r.name_i18n) {
                labels.insert(format!("param.{}", r.key), l);
            }
        }
    }
    let mut facets: Vec<Facet> = raw
        .into_iter()
        .map(|(key, values)| Facet {
            label: labels.get(&key).cloned().unwrap_or_else(|| key.clone()),
            values: values
                .into_iter()
                .map(|(value, selected, available)| FacetValue {
                    label: value_labels
                        .get(&(key.clone(), value.clone()))
                        .cloned()
                        .unwrap_or_else(|| value.clone()),
                    value,
                    selected,
                    available,
                })
                .collect(),
            key,
        })
        .collect();
    // Options, then parameters, then brand.
    facets.sort_by_key(|f| (!f.key.starts_with("opt."), f.key == "brand", f.key.clone()));
    Ok(facets)
}

/// The form a zero-result query is logged in, or `None` when it must not be stored (A20):
/// only short product-like queries, folded; anything that looks like contact data (an e-mail
/// address, a phone or account number: `@` or 6+ digits) or free text (more than 6 words, over
/// 64 characters) is dropped. Nothing ties it to a shopper; rows expire after 90 days.
fn zero_result_text(q: &str) -> Option<String> {
    let digits = q.chars().filter(char::is_ascii_digit).count();
    let folded = lang::fold(q);
    let ok = !q.contains('@')
        && digits < 6
        && !folded.is_empty()
        && folded.chars().count() <= 64
        && folded.split(' ').count() <= 6;
    ok.then_some(folded)
}

/// Counts a query without results (see [`zero_result_text`]).
pub async fn record_zero_result(tx: &mut TenantTx, locale: &str, q: &str) -> Result<(), Error> {
    let Some(normalized) = zero_result_text(q) else {
        return Ok(());
    };
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "INSERT INTO search_zero_results (tenant_id, day, locale, query)
         VALUES ($1, current_date, $2, $3)
         ON CONFLICT (tenant_id, day, locale, query)
         DO UPDATE SET count = search_zero_results.count + 1",
        tenant_id,
        locale,
        normalized
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Typeahead

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CategorySuggestion {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Suggestions {
    pub categories: Vec<CategorySuggestion>,
    pub products: Vec<SearchHit>,
}

/// Up to [`SUGGEST_LIMIT`] suggestions: matching categories (up to 3), then products.
pub async fn suggest(
    tx: &mut TenantTx,
    meili: &Meili,
    storage: &Storage,
    scope: &Scope,
    q: &str,
) -> Result<Suggestions, Error> {
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(invalid(
            "query_too_long",
            "the query is longer than 200 characters",
        ));
    }
    let words: Vec<String> = lang::fold(q)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    if words.is_empty() {
        return Ok(Suggestions {
            categories: vec![],
            products: vec![],
        });
    }
    // Every query word is a prefix of some word of the category name.
    let categories: Vec<CategorySuggestion> = sqlx::query!(
        "SELECT category_id, name, slug FROM category_translations WHERE locale = $1 ORDER BY name",
        scope.locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter(|r| {
        let name = lang::fold(&r.name);
        words
            .iter()
            .all(|w| name.split(' ').any(|n| n.starts_with(w.as_str())))
    })
    .take(SUGGEST_CATEGORIES)
    .map(|r| CategorySuggestion {
        id: r.category_id,
        name: r.name,
        slug: r.slug,
    })
    .collect();

    let req = SearchRequest {
        q: q.to_owned(),
        page: 1,
        per_page: u32::try_from(SUGGEST_LIMIT - categories.len()).unwrap_or(1),
        ..Default::default()
    };
    // Exact SKU/EAN matches, then stemmed matches, then the word being typed as a prefix of
    // the folded names (unstemmed). One list, no pagination: merging is safe here.
    let mut queries = plan(scope, &req, false, &[]).queries;
    let mut prefix = queries[0].clone();
    prefix["q"] = json!(lang::analyze_prefix(q, &scope.locale));
    queries.push(prefix);
    if let Some(exact) = exact_clause(q) {
        let mut e = queries[0].clone();
        e["q"] = json!("");
        e["filter"] = json!(and(base_clauses(scope, &req).into_iter().chain([exact])));
        queries.insert(0, e);
    }
    let results = meili.multi_search(&queries).await?;
    let mut ids: Vec<(Uuid, Uuid)> = vec![];
    for id in results.iter().flat_map(|r| hit_ids(Some(r))) {
        if !ids.iter().any(|(_, p)| *p == id.1) {
            ids.push(id);
        }
    }
    ids.truncate(req.per_page as usize);
    let products = rehydrate(tx, storage, scope, &req, &ids).await?;
    Ok(Suggestions {
        categories,
        products,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            tenant_id: Uuid::from_u128(7),
            market_code: "cz".into(),
            price_list_id: Some(Uuid::from_u128(8)),
            currency: Currency::Czk,
            locale: "cs".into(),
            default_locale: "cs".into(),
        }
    }

    fn req() -> SearchRequest {
        SearchRequest {
            page: 1,
            per_page: 24,
            ..Default::default()
        }
    }

    fn filters(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.iter().map(|s| (*s).to_owned()).collect()))
            .collect()
    }

    #[test]
    fn validation_bounds_input() {
        assert!(req().validate().is_ok());
        let bad = [
            SearchRequest {
                q: "x".repeat(201),
                ..req()
            },
            SearchRequest {
                filters: filters(&[("price.cz", &["1"])]),
                ..req()
            },
            SearchRequest {
                filters: filters(&[("opt.Color", &["red"])]),
                ..req()
            },
            SearchRequest {
                filters: filters(&[("opt.color", &[])]),
                ..req()
            },
            SearchRequest {
                filters: filters(&[("opt.color", &[" "])]),
                ..req()
            },
            SearchRequest {
                per_page: 49,
                ..req()
            },
            SearchRequest { page: 0, ..req() },
            SearchRequest { page: 43, ..req() },
            SearchRequest {
                price_min: Some(-1),
                ..req()
            },
        ];
        for r in bad {
            assert!(r.validate().is_err(), "{r:?}");
        }
        let ok = SearchRequest {
            filters: filters(&[
                ("opt.color", &["red"]),
                ("param.material", &["len"]),
                ("brand", &["Acme"]),
            ]),
            ..req()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn filters_are_or_within_and_across_facets_on_variant_documents() {
        let r = SearchRequest {
            q: "pánská Trička".into(),
            filters: filters(&[("opt.color", &["red", "blue"]), ("opt.size", &["xl"])]),
            in_stock: true,
            price_min: Some(100),
            ..req()
        };
        let p = plan(&scope(), &r, true, &[]);
        let hits = &p.queries[0];
        assert_eq!(hits["indexUid"], format!("t_{}_cs", Uuid::from_u128(7)));
        assert_eq!(hits["q"], "pansk trick");
        assert_eq!(
            hits["filter"],
            r#"(active_in_markets = "cz") AND (opt.color IN ["red", "blue"]) AND (opt.size IN ["xl"]) AND (in_stock = true) AND (price.cz >= 100)"#
        );
        assert!(
            hits.get("distinct").is_none(),
            "hits use the index distinct (product_id)"
        );
        // Availability for `opt.color` ignores the color selection but keeps the size.
        let color = &p.queries[p.per_facet["opt.color"]];
        assert_eq!(
            color["filter"],
            r#"(active_in_markets = "cz") AND (opt.size IN ["xl"]) AND (in_stock = true) AND (price.cz >= 100)"#
        );
        assert_eq!(color["distinct"], "id");
        assert_eq!(color["facets"], json!(["opt.color"]));
        let universe = &p.queries[p.universe.unwrap_or_default()];
        assert_eq!(universe["filter"], r#"(active_in_markets = "cz")"#);
        assert_eq!(
            universe["facets"],
            json!(["opt.*", "param.*", "brand", "price.cz"])
        );
        // A query with whitespace is no SKU.
        assert!(exact_clause(&r.q).is_none());
    }

    #[test]
    fn exact_matches_are_excluded_from_the_text_hits_only() {
        let r = SearchRequest {
            q: "TS-RED-M".into(),
            ..req()
        };
        assert_eq!(
            exact_clause(&r.q).as_deref(),
            Some(r#"skus = "TS-RED-M" OR eans = "TS-RED-M""#)
        );
        assert!(exact_clause(&"x".repeat(65)).is_none());
        let p = plan(&scope(), &r, true, &[Uuid::from_u128(5)]);
        assert_eq!(
            p.queries[0]["filter"],
            format!(
                r#"(active_in_markets = "cz") AND (NOT product_id IN ["{}"])"#,
                Uuid::from_u128(5)
            )
        );
        for q in &p.queries[1..] {
            assert_eq!(
                q["filter"], r#"(active_in_markets = "cz")"#,
                "facets see everything"
            );
        }
    }

    #[test]
    fn filter_values_cannot_escape_their_quotes() {
        let r = SearchRequest {
            q: r#"x"OR"#.into(),
            filters: filters(&[(
                "brand",
                &[r#"a" OR active_in_markets EXISTS OR brand = "b"#],
            )]),
            ..req()
        };
        let p = plan(&scope(), &r, true, &[]);
        assert_eq!(
            p.queries[0]["filter"],
            r#"(active_in_markets = "cz") AND (brand IN ["a\" OR active_in_markets EXISTS OR brand = \"b"])"#
        );
        assert_eq!(
            exact_clause(&r.q).as_deref(),
            Some(r#"skus = "x\"OR" OR eans = "x\"OR""#)
        );
    }

    #[test]
    fn sort_and_market_price_field() {
        let s = Scope {
            market_code: "sk-eu".into(),
            ..scope()
        };
        let p = plan(
            &s,
            &SearchRequest {
                sort: Sort::PriceDesc,
                ..req()
            },
            false,
            &[],
        );
        assert_eq!(p.queries[0]["sort"], json!(["price.sk_eu:desc"]));
        let p = plan(&s, &req(), false, &[]);
        assert_eq!(
            p.queries[0]["sort"],
            json!(["popularity:desc", "created_at:desc"])
        );
        let p = plan(
            &s,
            &SearchRequest {
                q: "boty".into(),
                ..req()
            },
            false,
            &[],
        );
        assert_eq!(p.queries[0]["sort"], json!([]));
    }

    #[test]
    fn facet_availability_combines_universe_and_disjunctive_queries() {
        let r = SearchRequest {
            filters: filters(&[("opt.color", &["red"])]),
            ..req()
        };
        let p = plan(&scope(), &r, true, &[]);
        let mut results = vec![json!({}); p.queries.len()];
        results[p.universe.unwrap_or_default()] = json!({ "facetDistribution": {
            "opt.color": { "blue": 3, "green": 1, "red": 2 },
            "opt.size": { "m": 1, "xl": 4 },
            "price.cz": { "100": 1 },
        }, "facetStats": { "price.cz": { "min": 100.0, "max": 900.0 } } });
        results[p.available.unwrap_or_default()] = json!({ "facetDistribution": {
            "opt.color": { "red": 2 }, "opt.size": { "xl": 2 },
        }});
        results[p.per_facet["opt.color"]] = json!({ "facetDistribution": {
            "opt.color": { "blue": 3, "red": 2 },
        }});
        let facets = facets_from(&p, &results, &r);
        assert_eq!(
            facets,
            vec![
                (
                    "opt.color".to_owned(),
                    vec![
                        ("blue".to_owned(), false, true),
                        ("green".to_owned(), false, false),
                        ("red".to_owned(), true, true),
                    ]
                ),
                (
                    "opt.size".to_owned(),
                    vec![
                        ("m".to_owned(), false, false),
                        ("xl".to_owned(), false, true)
                    ]
                ),
            ]
        );
        assert_eq!(
            price_range(&p, &results, &scope()),
            Some(PriceRange {
                min_minor: 100,
                max_minor: 900
            })
        );
    }

    #[test]
    fn selected_values_stay_listed_beyond_the_value_cap() {
        let r = SearchRequest {
            filters: filters(&[("brand", &["Zeta"])]),
            ..req()
        };
        let p = plan(&scope(), &r, true, &[]);
        let mut results = vec![json!({}); p.queries.len()];
        // The universe was cut before "Zeta"; the facet's own query still reaches it.
        results[p.universe.unwrap_or_default()] =
            json!({ "facetDistribution": { "brand": { "Acme": 1 } } });
        results[p.per_facet["brand"]] =
            json!({ "facetDistribution": { "brand": { "Acme": 1, "Zeta": 1 } } });
        let facets = facets_from(&p, &results, &r);
        assert_eq!(
            facets,
            vec![(
                "brand".to_owned(),
                vec![
                    ("Acme".to_owned(), false, true),
                    ("Zeta".to_owned(), true, true)
                ]
            )]
        );
    }

    #[test]
    fn exact_matches_lead_and_pages_neither_skip_nor_repeat() {
        // Exact [X, Y]; text (without X, Y) [a..g]; 3 per page.
        let text: Vec<char> = "abcdefg".chars().collect();
        let page = |p: u32| {
            let at = |q: u32| {
                text.iter()
                    .copied()
                    .skip(((q - 1) * 3) as usize)
                    .take(3)
                    .collect()
            };
            page_of(&['X', 'Y'], (p > 1).then(|| at(p - 1)), at(p), p, 3)
        };
        let all: Vec<char> = (1..=4).flat_map(page).collect();
        assert_eq!(all.iter().collect::<String>(), "XYabcdefg");
        assert_eq!(page(1), ['X', 'Y', 'a']);
        assert_eq!(page(2), ['b', 'c', 'd']);
        // Without exact matches it is just the text page.
        assert_eq!(page_of::<char>(&[], None, vec!['a', 'b'], 2, 3), ['a', 'b']);
    }

    #[test]
    fn zero_result_log_keeps_only_product_like_queries() {
        assert_eq!(
            zero_result_text("Modré  TRIČKO"),
            Some("modre tricko".to_owned())
        );
        for personal in [
            "jan.novak@example.cz",
            "+420 777 123 456",
            "777123456",
            "účet 1234567890/0100",
            "please call me back tomorrow morning about my order",
            "",
        ] {
            assert_eq!(zero_result_text(personal), None, "{personal}");
        }
        assert!(zero_result_text("iphone 15").is_some());
    }

    #[test]
    fn rehydration_rechecks_the_matched_variant() {
        let v = Current {
            id: Uuid::from_u128(1),
            price: 500,
            in_stock: false,
            options: [("color".to_owned(), "red".to_owned())].into(),
        };
        assert!(v.satisfies(&req()));
        assert!(v.satisfies(&SearchRequest {
            filters: filters(&[("opt.color", &["blue", "RED"]), ("brand", &["x"])]),
            ..req()
        }));
        for r in [
            SearchRequest {
                filters: filters(&[("opt.color", &["blue"])]),
                ..req()
            },
            SearchRequest {
                filters: filters(&[("opt.size", &["xl"])]),
                ..req()
            },
            SearchRequest {
                in_stock: true,
                ..req()
            },
            SearchRequest {
                price_max: Some(499),
                ..req()
            },
            SearchRequest {
                price_min: Some(501),
                ..req()
            },
        ] {
            assert!(!v.satisfies(&r), "{r:?}");
        }
    }
}
