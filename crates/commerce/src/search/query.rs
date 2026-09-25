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
}

/// Loads the market for `market_id` if it sells in `locale`.
pub async fn scope(
    tx: &mut TenantTx,
    market_id: Uuid,
    locale: &str,
) -> Result<Option<Scope>, Error> {
    let Some(r) = sqlx::query!(
        "SELECT code, currency, price_list_id, locales FROM markets WHERE id = $1",
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

/// Which query of the multi-search answers what.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    queries: Vec<Value>,
    exact: Option<usize>,
    available: Option<usize>,
    /// facet key → query index
    per_facet: BTreeMap<String, usize>,
    universe: Option<usize>,
}

const FACET_PATTERNS: [&str; 3] = ["opt.*", "param.*", "brand"];

fn plan(scope: &Scope, req: &SearchRequest, facets: bool) -> Plan {
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
    let mut p = Plan::default();
    p.queries.push(json!({
        "indexUid": uid, "q": q, "filter": all, "sort": sort,
        "page": req.page, "hitsPerPage": req.per_page,
        "attributesToRetrieve": ["id", "product_id"],
    }));
    let raw = req.q.trim();
    if req.page == 1 && !raw.is_empty() && raw.len() <= 64 && !raw.contains(char::is_whitespace) {
        let exact = format!("skus = {0} OR eans = {0}", quote(raw));
        p.exact = Some(p.queries.len());
        p.queries.push(json!({
            "indexUid": uid, "q": "", "filter": format!("{all} AND ({exact})"), "limit": 5,
            "attributesToRetrieve": ["id", "product_id"],
        }));
    }
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
fn facets_from(
    plan: &Plan,
    results: &[Value],
    req: &SearchRequest,
) -> Vec<(String, Vec<(String, bool, bool)>)> {
    let universe = distribution(plan.universe.and_then(|i| results.get(i)));
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
                let avail = reachable.contains(&v);
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
    let plan = plan(scope, req, true);
    let results = meili.multi_search(&plan.queries).await?;
    let hits = results.first();
    let total = hits
        .and_then(|h| h.get("totalHits"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_pages = hits
        .and_then(|h| h.get("totalPages"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);

    let mut ids = plan
        .exact
        .map(|i| hit_ids(results.get(i)))
        .unwrap_or_default();
    for id in hit_ids(hits) {
        if !ids.iter().any(|(_, p)| *p == id.1) {
            ids.push(id);
        }
    }
    ids.truncate(req.per_page as usize);
    let items = rehydrate(tx, storage, scope, &ids).await?;

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

/// Current storefront data for `ids` (variant, product), in order; products that are no
/// longer active, translated or priced in the market are left out.
async fn rehydrate(
    tx: &mut TenantTx,
    storage: &Storage,
    scope: &Scope,
    ids: &[(Uuid, Uuid)],
) -> Result<Vec<SearchHit>, Error> {
    let Some(price_list) = scope.price_list_id else {
        return Ok(vec![]);
    };
    let products: Vec<Uuid> = ids.iter().map(|(_, p)| *p).collect();
    let reps: Vec<Uuid> = ids.iter().map(|(v, _)| *v).collect();
    let rows: HashMap<Uuid, (String, String, Option<String>)> = sqlx::query!(
        "SELECT p.id, t.name, t.slug, p.brand
         FROM products p JOIN product_translations t ON t.product_id = p.id AND t.locale = $2
         WHERE p.id = ANY($1) AND p.status = 'active'",
        &products,
        scope.locale
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.id, (r.name, r.slug, r.brand)))
    .collect();

    // product → [(variant, price, in stock)] for variants priced in the market now.
    let mut variants: HashMap<Uuid, Vec<(Uuid, i64, bool)>> = HashMap::new();
    for r in sqlx::query!(
        r#"SELECT v.product_id, v.id, pi.amount_minor,
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
        variants
            .entry(r.product_id)
            .or_default()
            .push((r.id, r.amount_minor, r.in_stock));
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
            // The matched variant if it is still priced, else the cheapest one.
            let (variant_id, price, _) = priced
                .iter()
                .find(|(v, _, _)| v == variant)
                .or_else(|| priced.iter().min_by_key(|(_, p, _)| *p))
                .copied()?;
            Some(SearchHit {
                product_id: *product,
                variant_id,
                name,
                slug,
                brand,
                price: Money::new(price, scope.currency).view(locale),
                in_stock: priced.iter().any(|(_, _, s)| *s),
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
    raw: Vec<(String, Vec<(String, bool, bool)>)>,
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

/// Counts a query without results (normalized text only, A20).
pub async fn record_zero_result(tx: &mut TenantTx, locale: &str, q: &str) -> Result<(), Error> {
    let normalized: String = lang::fold(q).chars().take(MAX_QUERY_CHARS).collect();
    if normalized.is_empty() {
        return Ok(());
    }
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
    let plan = plan(scope, &req, false);
    let results = meili.multi_search(&plan.queries).await?;
    let mut ids = plan
        .exact
        .map(|i| hit_ids(results.get(i)))
        .unwrap_or_default();
    for id in hit_ids(results.first()) {
        if !ids.iter().any(|(_, p)| *p == id.1) {
            ids.push(id);
        }
    }
    ids.truncate(req.per_page as usize);
    let products = rehydrate(tx, storage, scope, &ids).await?;
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
        let p = plan(&scope(), &r, true);
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
        // A token with whitespace is no SKU: no exact query.
        assert!(p.exact.is_none());
    }

    #[test]
    fn single_token_queries_also_look_up_exact_skus() {
        let r = SearchRequest {
            q: "TS-RED-M".into(),
            category_id: Some(Uuid::from_u128(9)),
            ..req()
        };
        let p = plan(&scope(), &r, false);
        let exact = &p.queries[p.exact.unwrap_or_default()];
        assert_eq!(
            exact["filter"],
            format!(
                r#"(active_in_markets = "cz") AND (category_ids = "{}") AND (skus = "TS-RED-M" OR eans = "TS-RED-M")"#,
                Uuid::from_u128(9)
            )
        );
        assert_eq!(p.queries.len(), 2);
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
        let p = plan(&scope(), &r, true);
        assert_eq!(
            p.queries[0]["filter"],
            r#"(active_in_markets = "cz") AND (brand IN ["a\" OR active_in_markets EXISTS OR brand = \"b"])"#
        );
        assert_eq!(
            p.queries[1]["filter"]
                .as_str()
                .map(|f| f.ends_with(r#"(skus = "x\"OR" OR eans = "x\"OR")"#)),
            Some(true)
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
        );
        assert_eq!(p.queries[0]["sort"], json!(["price.sk_eu:desc"]));
        let p = plan(&s, &req(), false);
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
        );
        assert_eq!(p.queries[0]["sort"], json!([]));
    }

    #[test]
    fn facet_availability_combines_universe_and_disjunctive_queries() {
        let r = SearchRequest {
            filters: filters(&[("opt.color", &["red"])]),
            ..req()
        };
        let p = plan(&scope(), &r, true);
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
}
