//! Product listings (category pages, the WP6 search stand-in): filter, sort, paginate and
//! compute facets.
//!
//! [`listing`] is the one seam the search work package (WP7) swaps for Meilisearch: it returns
//! ordered product ids, the total and the facets; cards are hydrated separately
//! ([`super::cards::cards`]), which is also how search hits are rehydrated from Postgres (A23).
//!
//! Postgres implementation: one pass loads the candidate products of the category subtree
//! with their priced variants, option values and filterable parameters; a pure function
//! ([`select`]) filters, sorts, pages and builds the facets.
//! ponytail: in-memory filtering, fine up to a few thousand products per category;
//! Meilisearch (WP7) takes over beyond that.
//!
//! Filter semantics: values of one facet are OR-ed, facets are AND-ed. Option filters must be
//! met by one and the same variant (a red shirt in size M, not a red S and a blue M). Facet
//! counts are not shown (A23); a value that would give no result is `disabled`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::Context;
use super::cards::priced_variants;
use super::messages;

pub const PER_PAGE: u32 = 24;
pub const MAX_PAGE: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Recommended,
    PriceAsc,
    PriceDesc,
    Newest,
}

impl Sort {
    pub const ALL: [Self; 4] = [
        Self::Recommended,
        Self::PriceAsc,
        Self::PriceDesc,
        Self::Newest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::PriceAsc => "price_asc",
            Self::PriceDesc => "price_desc",
            Self::Newest => "newest",
        }
    }

    fn search(self) -> crate::search::query::Sort {
        use crate::search::query::Sort as S;
        match self {
            Self::Recommended => S::Relevance,
            Self::PriceAsc => S::PriceAsc,
            Self::PriceDesc => S::PriceDesc,
            Self::Newest => S::Newest,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingQuery {
    /// Products of this category and its descendants; `None` with `search` = whole catalog.
    pub category_id: Option<Uuid>,
    /// Name search (diacritics-insensitive). WP7 replaces it with Meilisearch.
    pub search: Option<String>,
    /// Facet key -> selected values.
    pub filters: BTreeMap<String, BTreeSet<String>>,
    pub sort: Sort,
    /// 1-based.
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacetKind {
    Option,
    Parameter,
    Brand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetValue {
    pub value: String,
    pub label: String,
    pub selected: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facet {
    pub key: String,
    pub label: String,
    pub kind: FacetKind,
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub product_ids: Vec<Uuid>,
    pub total: u32,
    pub page: u32,
    pub pages: u32,
    pub facets: Vec<Facet>,
    /// The filters that were applied (unknown keys and values dropped).
    pub applied: BTreeMap<String, BTreeSet<String>>,
}

// ---------------------------------------------------------------------------------------
// Pure part

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateVariant {
    pub options: BTreeMap<String, String>,
    pub price_minor: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: Uuid,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub name: String,
    pub brand: Option<String>,
    pub variants: Vec<CandidateVariant>,
    /// Filterable parameter key -> values (product and variant level).
    pub params: BTreeMap<String, BTreeSet<String>>,
}

/// Definition of a facet: its label and the labels of its values, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetDef {
    pub key: String,
    pub label: String,
    pub kind: FacetKind,
    pub values: Vec<(String, String)>,
}

/// The kind of a facet key, from its form: `opt.<code>`, `param.<key>` or `brand` (the search
/// engine's keys, WP7).
pub fn facet_kind(key: &str) -> Option<FacetKind> {
    if key == "brand" {
        Some(FacetKind::Brand)
    } else if key.starts_with("param.") {
        Some(FacetKind::Parameter)
    } else if key.starts_with("opt.") {
        Some(FacetKind::Option)
    } else {
        None
    }
}

/// Every filter restricts: a product without the facet, or without a selected value, does not
/// match (a filter is never silently widened).
fn matches(c: &Candidate, filters: &BTreeMap<String, BTreeSet<String>>) -> bool {
    let product_ok = filters.iter().all(|(k, want)| match facet_kind(k) {
        Some(FacetKind::Brand) => c.brand.as_ref().is_some_and(|b| want.contains(b)),
        Some(FacetKind::Parameter) => c.params.get(k).is_some_and(|have| !have.is_disjoint(want)),
        Some(FacetKind::Option) => true,
        None => false,
    });
    product_ok
        && c.variants.iter().any(|v| {
            filters
                .iter()
                .filter(|(k, _)| facet_kind(k) == Some(FacetKind::Option))
                .all(|(k, want)| v.options.get(k).is_some_and(|x| want.contains(x)))
        })
}

fn min_price(c: &Candidate) -> i64 {
    c.variants
        .iter()
        .map(|v| v.price_minor)
        .min()
        .unwrap_or(i64::MAX)
}

/// Filters, sorts, pages and computes facets. `candidates` come in "recommended" order
/// already broken by `position`.
pub fn select(mut candidates: Vec<Candidate>, defs: &[FacetDef], q: &ListingQuery) -> Listing {
    candidates.retain(|c| !c.variants.is_empty());
    // Only known facets and values filter; anything else in the URL is ignored.
    // Every requested restriction applies, known values or not (an unknown value matches
    // nothing, like in the search engine).
    let applied: BTreeMap<String, BTreeSet<String>> = q
        .filters
        .iter()
        .filter(|(k, vs)| facet_kind(k).is_some() && !vs.is_empty())
        .map(|(k, vs)| (k.clone(), vs.clone()))
        .collect();

    let facets = defs
        .iter()
        .map(|d| {
            let mut others = applied.clone();
            let values = d
                .values
                .iter()
                .map(|(value, label)| {
                    others.insert(d.key.clone(), BTreeSet::from([value.clone()]));
                    FacetValue {
                        value: value.clone(),
                        label: label.clone(),
                        selected: applied.get(&d.key).is_some_and(|s| s.contains(value)),
                        disabled: !candidates.iter().any(|c| matches(c, &others)),
                    }
                })
                .collect::<Vec<_>>();
            Facet {
                key: d.key.clone(),
                label: d.label.clone(),
                kind: d.kind,
                values,
            }
        })
        // A facet with a single choice filters nothing, unless it is selected.
        .filter(|f| f.values.len() > 1 || f.values.iter().any(|v| v.selected))
        .collect();

    let mut hits: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| matches(c, &applied))
        .collect();
    match q.sort {
        Sort::Recommended => {
            hits.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then(b.created_at.cmp(&a.created_at))
            });
        }
        Sort::PriceAsc => hits.sort_by_key(min_price),
        Sort::PriceDesc => hits.sort_by_key(|c| std::cmp::Reverse(min_price(c))),
        Sort::Newest => hits.sort_by_key(|c| std::cmp::Reverse(c.created_at)),
    }
    let per_page = q.per_page.clamp(1, 100);
    let total = u32::try_from(hits.len()).unwrap_or(u32::MAX);
    let pages = total.div_ceil(per_page).max(1);
    let page = q.page.clamp(1, MAX_PAGE);
    let start = usize::try_from((page - 1) * per_page).unwrap_or(usize::MAX);
    let product_ids = hits
        .iter()
        .skip(start)
        .take(per_page as usize)
        .map(|c| c.id)
        .collect();
    Listing {
        product_ids,
        total,
        page,
        pages,
        facets,
        applied,
    }
}

/// Lowercase without Czech/Slovak (and common Latin) diacritics, for matching and sorting.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'ä' | 'à' | 'â' | 'ã' | 'å' => 'a',
            'č' | 'ç' | 'ć' => 'c',
            'ď' => 'd',
            'é' | 'ě' | 'ë' | 'è' | 'ê' => 'e',
            'í' | 'ï' | 'ì' | 'î' => 'i',
            'ĺ' | 'ľ' | 'ł' => 'l',
            'ň' | 'ñ' | 'ń' => 'n',
            'ó' | 'ö' | 'ô' | 'ò' | 'õ' | 'ő' => 'o',
            'ř' | 'ŕ' => 'r',
            'š' | 'ś' => 's',
            'ť' => 't',
            'ú' | 'ů' | 'ü' | 'ù' | 'û' | 'ű' => 'u',
            'ý' | 'ÿ' => 'y',
            'ž' | 'ź' | 'ż' => 'z',
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Postgres loading

struct Base {
    id: Uuid,
    position: i32,
    created_at: DateTime<Utc>,
    name: String,
    brand: Option<String>,
}

/// The listing for `q` in the market of `ctx` (see the module docs).
pub async fn listing(tx: &mut TenantTx, ctx: &Context, q: &ListingQuery) -> Result<Listing, Error> {
    let mut base: Vec<Base> = sqlx::query_as!(
        Base,
        r#"WITH RECURSIVE subtree AS (
               SELECT id FROM categories WHERE id = $1
               UNION ALL
               SELECT c.id FROM categories c JOIN subtree s ON c.parent_id = s.id
           )
           SELECT p.id, coalesce(min(pc.position), 0) AS "position!", p.created_at,
                  (SELECT name FROM product_translations pt WHERE pt.product_id = p.id
                   ORDER BY (pt.locale = $2) DESC, (pt.locale = $3) DESC, pt.locale LIMIT 1)
                   AS "name!",
                  p.brand
           FROM products p
           LEFT JOIN product_categories pc
                  ON pc.product_id = p.id AND pc.category_id IN (SELECT id FROM subtree)
           WHERE p.status = 'active'
           GROUP BY p.id
           HAVING $1::uuid IS NULL OR count(pc.category_id) > 0"#,
        q.category_id,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_all(&mut **tx)
    .await?;
    if let Some(term) = q
        .search
        .as_deref()
        .map(fold)
        .filter(|t| !t.trim().is_empty())
    {
        let words: Vec<&str> = term.split_whitespace().collect();
        base.retain(|b| {
            let name = fold(&b.name);
            words.iter().all(|w| name.contains(w))
        });
        // Relevance stand-in: names starting with the term first.
        for b in &mut base {
            b.position = i32::from(!fold(&b.name).starts_with(words[0]));
        }
    } else if q.search.is_some() {
        base.clear();
    }
    let ids: Vec<Uuid> = base.iter().map(|b| b.id).collect();

    let mut variants: HashMap<Uuid, Vec<CandidateVariant>> = HashMap::new();
    for v in priced_variants(tx, ctx, &ids).await? {
        variants
            .entry(v.product_id)
            .or_default()
            .push(CandidateVariant {
                // Facet keys are the search engine's (`opt.<code>`), so URLs work with both.
                options: v
                    .option_values
                    .into_iter()
                    .map(|(k, v)| (format!("opt.{k}"), v))
                    .collect(),
                price_minor: v.price.amount_minor,
            });
    }

    // Facet definitions: options by code (labels from the first product defining them) ...
    let mut defs: Vec<FacetDef> = Vec::new();
    let mut option_order: BTreeMap<String, (i32, usize)> = BTreeMap::new();
    for o in sqlx::query!(
        r#"SELECT code, position, name_i18n, "values" AS "values!" FROM product_options
           WHERE product_id = ANY($1) ORDER BY position, code"#,
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let idx = match option_order.get(&o.code) {
            Some((_, i)) => *i,
            None => {
                defs.push(FacetDef {
                    key: format!("opt.{}", o.code),
                    label: ctx.text(&o.name_i18n).unwrap_or_else(|| o.code.clone()),
                    kind: FacetKind::Option,
                    values: Vec::new(),
                });
                option_order.insert(o.code.clone(), (o.position, defs.len() - 1));
                defs.len() - 1
            }
        };
        if let Value::Array(values) = &o.values {
            for v in values {
                let Some(code) = v.get("code").and_then(Value::as_str) else {
                    continue;
                };
                if !defs[idx].values.iter().any(|(c, _)| c == code) {
                    let label = v
                        .get("name_i18n")
                        .and_then(|n| ctx.text(n))
                        .unwrap_or_else(|| code.to_owned());
                    defs[idx].values.push((code.to_owned(), label));
                }
            }
        }
    }

    // ... then filterable parameters.
    let mut params: HashMap<Uuid, BTreeMap<String, BTreeSet<String>>> = HashMap::new();
    let mut param_defs: BTreeMap<String, FacetDef> = BTreeMap::new();
    for r in sqlx::query!(
        "SELECT ppv.product_id, pa.key, pa.kind, pa.unit, pa.name_i18n, ppv.value
         FROM product_parameter_values ppv JOIN parameters pa ON pa.id = ppv.parameter_id
         WHERE pa.filterable AND ppv.product_id = ANY($1)
         ORDER BY pa.key",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let Some((value, label)) = param_value(ctx, &r.kind, r.unit.as_deref(), &r.value) else {
            continue;
        };
        let key = format!("param.{}", r.key);
        params
            .entry(r.product_id)
            .or_default()
            .entry(key.clone())
            .or_default()
            .insert(value.clone());
        let def = param_defs.entry(key.clone()).or_insert_with(|| FacetDef {
            key,
            label: ctx.text(&r.name_i18n).unwrap_or_else(|| r.key.clone()),
            kind: FacetKind::Parameter,
            values: Vec::new(),
        });
        if !def.values.iter().any(|(v, _)| *v == value) {
            def.values.push((value, label));
        }
    }
    let mut brands: BTreeSet<String> = base.iter().filter_map(|b| b.brand.clone()).collect();
    if !brands.is_empty() {
        defs.push(FacetDef {
            key: "brand".into(),
            label: messages::text(&ctx.locale, "listing.brand").to_owned(),
            kind: FacetKind::Brand,
            values: std::mem::take(&mut brands)
                .into_iter()
                .map(|b| (b.clone(), b))
                .collect(),
        });
    }
    for mut d in param_defs.into_values() {
        // Options keep the merchant's order; parameter values are sorted.
        d.values.sort_by_cached_key(|v| fold(&v.1));
        if !defs.iter().any(|x| x.key == d.key) {
            defs.push(d);
        }
    }

    let candidates = base
        .into_iter()
        .map(|b| Candidate {
            variants: variants.remove(&b.id).unwrap_or_default(),
            params: params.remove(&b.id).unwrap_or_default(),
            id: b.id,
            position: b.position,
            created_at: b.created_at,
            name: b.name,
            brand: b.brand,
        })
        .collect();
    Ok(select(candidates, &defs, q))
}

/// The listing through the search engine (WP7, A23: variant-correct facets, results
/// rehydrated from Postgres). `Err(Unavailable)` when search is degraded: callers fall back to
/// [`listing`].
pub async fn search_listing(
    tx: &mut TenantTx,
    ctx: &Context,
    search: super::Search<'_>,
    q: &ListingQuery,
) -> Result<Listing, Error> {
    use crate::search::query as sq;
    let scope = super::search_scope(tx, ctx).await?;
    let req = sq::SearchRequest {
        q: q.search.clone().unwrap_or_default(),
        filters: q
            .filters
            .iter()
            .map(|(k, vs)| (k.clone(), vs.iter().cloned().collect()))
            .collect(),
        category_id: q.category_id,
        in_stock: false,
        price_min: None,
        price_max: None,
        sort: q.sort.search(),
        // The engine serves the first 1000 hits only; later pages are clamped, never a 422.
        page: q
            .page
            .clamp(1, 1000 / q.per_page.clamp(1, sq::MAX_PER_PAGE) + 1),
        per_page: q.per_page.clamp(1, sq::MAX_PER_PAGE),
    };
    let found = sq::search(tx, search.meili, search.storage, &scope, &req).await?;
    let mut product_ids: Vec<Uuid> = Vec::with_capacity(found.items.len());
    for hit in &found.items {
        if !product_ids.contains(&hit.product_id) {
            product_ids.push(hit.product_id);
        }
    }
    let facets: Vec<Facet> = found
        .facets
        .into_iter()
        .map(|f| Facet {
            kind: if f.key == "brand" {
                FacetKind::Brand
            } else if f.key.starts_with("param.") {
                FacetKind::Parameter
            } else {
                FacetKind::Option
            },
            values: f
                .values
                .into_iter()
                .map(|v| FacetValue {
                    value: v.value,
                    label: v.label,
                    selected: v.selected,
                    disabled: !v.available,
                })
                .collect(),
            // The engine labels option/parameter facets from the catalog; brand is ours.
            label: if f.key == "brand" {
                messages::text(&ctx.locale, "listing.brand").to_owned()
            } else {
                f.label
            },
            key: f.key,
        })
        .collect();
    let applied = facets
        .iter()
        .filter_map(|f| {
            let selected: BTreeSet<String> = f
                .values
                .iter()
                .filter(|v| v.selected)
                .map(|v| v.value.clone())
                .collect();
            (!selected.is_empty()).then(|| (f.key.clone(), selected))
        })
        .collect();
    Ok(Listing {
        product_ids,
        total: u32::try_from(found.total).unwrap_or(u32::MAX),
        page: found.page,
        pages: found.total_pages.max(1),
        facets,
        applied,
    })
}

/// [`search_listing`] when search is available, the Postgres [`listing`] otherwise.
pub async fn find(
    tx: &mut TenantTx,
    ctx: &Context,
    search: Option<super::Search<'_>>,
    q: &ListingQuery,
) -> Result<Listing, Error> {
    if let Some(s) = search {
        match search_listing(tx, ctx, s, q).await {
            Ok(found) => return Ok(found),
            Err(Error::Unavailable(reason)) => {
                tracing::warn!(%reason, "search degraded; listing from Postgres");
            }
            Err(e) => return Err(e),
        }
    }
    listing(tx, ctx, q).await
}

/// A parameter value as a filter value and its label: localized text, `true`/`false`, or a
/// number (with unit in the label).
pub(crate) fn param_value(
    ctx: &Context,
    kind: &str,
    unit: Option<&str>,
    value: &Value,
) -> Option<(String, String)> {
    match (kind, value) {
        ("text", v @ Value::Object(_)) => ctx.text(v).map(|t| (t.clone(), t)),
        ("bool", Value::Bool(b)) => Some((
            b.to_string(),
            messages::text(&ctx.locale, if *b { "common.yes" } else { "common.no" }).to_owned(),
        )),
        ("number", Value::Number(n)) => {
            let raw = n.to_string();
            let shown = if ctx.fmt_locale() == crate::money::Locale::En {
                raw.clone()
            } else {
                raw.replace('.', ",")
            };
            let label = match unit {
                Some(u) => format!("{shown}\u{a0}{u}"),
                None => shown,
            };
            Some((raw, label))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(color: &str, size: &str, price: i64) -> CandidateVariant {
        CandidateVariant {
            options: BTreeMap::from([
                ("opt.color".into(), color.into()),
                ("opt.size".into(), size.into()),
            ]),
            price_minor: price,
        }
    }

    fn cand(
        n: u128,
        position: i32,
        name: &str,
        variants: Vec<CandidateVariant>,
        material: &str,
    ) -> Candidate {
        Candidate {
            id: Uuid::from_u128(n),
            position,
            created_at: DateTime::from_timestamp(i64::try_from(n).unwrap_or(0) * 1000, 0)
                .unwrap_or_default(),
            name: name.into(),
            brand: Some(if n == 3 { "Acme" } else { "Lnen" }.into()),
            variants,
            params: BTreeMap::from([("param.material".into(), BTreeSet::from([material.into()]))]),
        }
    }

    fn defs() -> Vec<FacetDef> {
        let d = |key: &str, kind, values: &[&str]| FacetDef {
            key: key.into(),
            label: key.into(),
            kind,
            values: values.iter().map(|v| ((*v).into(), (*v).into())).collect(),
        };
        vec![
            d("opt.color", FacetKind::Option, &["red", "blue", "green"]),
            d("opt.size", FacetKind::Option, &["s", "m"]),
            d("param.material", FacetKind::Parameter, &["cotton", "wool"]),
            d("brand", FacetKind::Brand, &["Acme", "Lnen"]),
        ]
    }

    fn catalog() -> Vec<Candidate> {
        vec![
            // Red only in S, blue only in M: must not match "red + M".
            cand(
                1,
                2,
                "Žlutá mikina",
                vec![v("red", "s", 500), v("blue", "m", 700)],
                "cotton",
            ),
            cand(2, 1, "Tričko", vec![v("red", "m", 300)], "cotton"),
            cand(3, 3, "Čepice", vec![v("blue", "s", 900)], "wool"),
            // Not priced in the market: never listed.
            cand(4, 0, "Unpriced", vec![], "cotton"),
        ]
    }

    fn query(filters: &[(&str, &[&str])], sort: Sort) -> ListingQuery {
        ListingQuery {
            filters: filters
                .iter()
                .map(|(k, vs)| ((*k).into(), vs.iter().map(|v| (*v).into()).collect()))
                .collect(),
            sort,
            page: 1,
            per_page: 24,
            ..ListingQuery::default()
        }
    }

    fn ids(l: &Listing) -> Vec<u128> {
        l.product_ids.iter().map(|id| id.as_u128()).collect()
    }

    #[test]
    fn option_filters_match_on_one_variant() {
        let l = select(
            catalog(),
            &defs(),
            &query(
                &[("opt.color", &["red"]), ("opt.size", &["m"])],
                Sort::Recommended,
            ),
        );
        assert_eq!(ids(&l), [2]);
        let l = select(
            catalog(),
            &defs(),
            &query(&[("opt.color", &["red", "blue"])], Sort::Recommended),
        );
        assert_eq!(ids(&l), [2, 1, 3], "OR within a facet, recommended order");
        let l = select(
            catalog(),
            &defs(),
            &query(&[("param.material", &["wool"])], Sort::Recommended),
        );
        assert_eq!(ids(&l), [3]);
        let l = select(
            catalog(),
            &defs(),
            &query(&[("brand", &["Acme"])], Sort::Recommended),
        );
        assert_eq!(ids(&l), [3], "brand restricts in the fallback too");
    }

    #[test]
    fn restrictions_never_widen_and_zero_match_values_are_disabled() {
        // An unknown value of a known facet, or a facet no product has, matches nothing.
        for filters in [
            &[("opt.size", &["xl"][..])][..],
            &[("param.fit", &["slim"][..])][..],
        ] {
            let l = select(catalog(), &defs(), &query(filters, Sort::Recommended));
            assert_eq!(l.total, 0, "{filters:?}");
        }
        // A key that is not a facet key (`colour`) is not a filter at all.
        let l = select(
            catalog(),
            &defs(),
            &query(&[("colour", &["red"])], Sort::Recommended),
        );
        assert_eq!(l.total, 3);
        let l = select(
            catalog(),
            &defs(),
            &query(&[("param.material", &["wool"])], Sort::Recommended),
        );
        let color = l.facets.iter().find(|f| f.key == "opt.color").unwrap();
        let state: Vec<(&str, bool)> = color
            .values
            .iter()
            .map(|v| (v.value.as_str(), v.disabled))
            .collect();
        assert_eq!(state, [("red", true), ("blue", false), ("green", true)]);
        let material = l.facets.iter().find(|f| f.key == "param.material").unwrap();
        // Selecting cotton instead of wool would still give results: enabled.
        assert!(material.values.iter().all(|v| !v.disabled));
        assert!(
            material
                .values
                .iter()
                .any(|v| v.value == "wool" && v.selected)
        );
    }

    #[test]
    fn sorting_and_paging() {
        let l = select(catalog(), &defs(), &query(&[], Sort::PriceAsc));
        assert_eq!(ids(&l), [2, 1, 3]);
        let l = select(catalog(), &defs(), &query(&[], Sort::PriceDesc));
        assert_eq!(ids(&l), [3, 1, 2]);
        let l = select(catalog(), &defs(), &query(&[], Sort::Newest));
        assert_eq!(ids(&l), [3, 2, 1]);
        let mut q = query(&[], Sort::Recommended);
        q.per_page = 2;
        q.page = 2;
        let l = select(catalog(), &defs(), &q);
        assert_eq!((ids(&l), l.total, l.pages), (vec![3], 3, 2));
        q.page = 99;
        assert!(select(catalog(), &defs(), &q).product_ids.is_empty());
    }

    #[test]
    fn folding() {
        assert_eq!(
            fold("Žluťoučký KŮŇ úpěl ďábelské ódy"),
            "zlutoucky kun upel dabelske ody"
        );
        assert_eq!(fold("Ľahká čiapka"), "lahka ciapka");
    }
}
