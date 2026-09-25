//! Bulk edit by prompt (§12.2). The model turns the staff's request into a [`Plan`]: a target
//! selector plus allowlisted operations. The platform validates it, resolves the targets itself
//! (count + sample preview = the dry run), and only after the staff confirms (fresh auth for
//! price changes, A9) a job applies it product by product through the catalog and pricing
//! services, exactly once per product.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::fields::product_input;
use super::{Ai, Call, CallError, Outcome, prompts};
use crate::audit;
use crate::catalog::I18n;
use crate::catalog::products::{self, ParameterValue, ProductInput, ProductStatus};
use crate::markets::is_locale;
use crate::money::{Currency, div_round_half_up};
use crate::pricing::{self, PriceChangeReason, PriceItem, PriceUpsert};

pub const PLAN_JOB: &str = "ai.bulk_plan";
pub const APPLY_JOB: &str = "ai.bulk_apply";
/// Hard caps (§12.2).
pub const MAX_TARGETS: usize = 500;
pub const MAX_OPERATIONS: usize = 10;
/// Every price may move by at most this many percent (either way) and must stay above zero.
pub const MAX_PERCENT: i64 = 50;
const SAMPLE: usize = 10;

// ---------------------------------------------------------------------------------------
// The plan (validated model output)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The model's summary for the staff (plain text).
    pub explanation: String,
    pub selector: Selector,
    pub operations: Vec<Operation>,
}

/// Which products change: every non-empty condition must hold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    /// Category slugs (any locale); subcategories included.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Brand names (case-insensitive).
    #[serde(default)]
    pub brands: Vec<String>,
    #[serde(default)]
    pub statuses: Vec<ProductStatus>,
    #[serde(default)]
    pub parameters: Vec<ParameterFilter>,
    pub price: Option<PriceFilter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterFilter {
    /// Parameter key.
    pub parameter: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceFilter {
    /// Market code or price list code.
    pub market: String,
    pub min_minor: Option<i64>,
    pub max_minor: Option<i64>,
}

/// Text fields a plan may set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanField {
    Brand,
    ShortDescription,
    SeoTitle,
    SeoDescription,
}

impl PlanField {
    fn max_len(self) -> usize {
        match self {
            Self::Brand | Self::SeoTitle => 200,
            Self::SeoDescription => 500,
            Self::ShortDescription => 1000,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Brand => "brand",
            Self::ShortDescription => "short_description",
            Self::SeoTitle => "seo_title",
            Self::SeoDescription => "seo_description",
        }
    }
}

/// The allowlisted operations; anything else fails to parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    SetField {
        field: PlanField,
        /// Required for translated fields, `null` for `brand`.
        locale: Option<String>,
        value: String,
    },
    AdjustPrice {
        /// Market code or price list code.
        market: String,
        /// Percent change (5 = +5 %); exclusive with `amount_minor`.
        percent: Option<f64>,
        /// Fixed change in minor units; exclusive with `percent`.
        amount_minor: Option<i64>,
    },
    AddCategory {
        category: String,
    },
    RemoveCategory {
        category: String,
    },
    SetParameter {
        parameter: String,
        value: String,
        /// Text parameters: one locale, or `null` for every shop locale.
        locale: Option<String>,
    },
    SetStatus {
        status: ProductStatus,
    },
}

/// Percent in basis points (5.25 % = 525), when it has at most two decimals.
fn percent_bps(p: f64) -> Option<i64> {
    let bps = (p * 100.0).round();
    ((p * 100.0 - bps).abs() < 1e-6 && p.is_finite()).then_some(bps as i64)
}

impl Plan {
    pub fn has_price_changes(&self) -> bool {
        self.operations
            .iter()
            .any(|o| matches!(o, Operation::AdjustPrice { .. }))
    }

    /// Checks that need no database. Returns every problem found.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = vec![];
        if self.operations.is_empty() {
            errors.push("the plan has no operations".to_owned());
        }
        if self.operations.len() > MAX_OPERATIONS {
            errors.push(format!("at most {MAX_OPERATIONS} operations"));
        }
        if self.explanation.chars().count() > 2000 {
            errors.push("the explanation is too long".into());
        }
        let s = &self.selector;
        if [s.categories.len(), s.brands.len(), s.parameters.len()]
            .iter()
            .any(|n| *n > 50)
        {
            errors.push("the selector lists too many values".into());
        }
        if let Some(p) = &s.price
            && p.min_minor.zip(p.max_minor).is_some_and(|(a, b)| a > b)
        {
            errors.push("the price filter's minimum is above its maximum".into());
        }
        let mut added = BTreeSet::new();
        let mut removed = BTreeSet::new();
        for op in &self.operations {
            match op {
                Operation::SetField {
                    field,
                    locale,
                    value,
                } => {
                    match (field, locale) {
                        (PlanField::Brand, None) => {}
                        (PlanField::Brand, Some(_)) => {
                            errors.push("brand is not translated: locale must be null".into());
                        }
                        (_, Some(l)) if is_locale(l) => {}
                        (f, _) => errors.push(format!("{} needs a locale", f.as_str())),
                    }
                    let len = value.trim().chars().count();
                    if len == 0 || len > field.max_len() {
                        errors.push(format!(
                            "{} must be 1-{} characters",
                            field.as_str(),
                            field.max_len()
                        ));
                    }
                }
                Operation::AdjustPrice {
                    percent,
                    amount_minor,
                    ..
                } => match (percent, amount_minor) {
                    (Some(p), None) => match percent_bps(*p) {
                        Some(0) => errors.push("a price change of 0 % changes nothing".into()),
                        Some(bps) if bps.abs() > MAX_PERCENT * 100 => errors.push(format!(
                            "price changes are limited to ±{MAX_PERCENT} % (asked {p} %)"
                        )),
                        Some(_) => {}
                        None => errors.push("percent has at most two decimals".into()),
                    },
                    (None, Some(0)) => errors.push("a price change of 0 changes nothing".into()),
                    (None, Some(a)) if a.abs() > 1_000_000_000 => {
                        errors.push("the fixed price change is too large".into());
                    }
                    (None, Some(_)) => {}
                    _ => errors.push("adjust_price needs exactly one of percent and amount_minor".into()),
                },
                Operation::AddCategory { category } => {
                    added.insert(category.as_str());
                }
                Operation::RemoveCategory { category } => {
                    removed.insert(category.as_str());
                }
                Operation::SetParameter { value, locale, .. } => {
                    let len = value.trim().chars().count();
                    if len == 0 || len > 200 {
                        errors.push("parameter values are 1-200 characters".into());
                    }
                    if locale.as_deref().is_some_and(|l| !is_locale(l)) {
                        errors.push("invalid locale".into());
                    }
                }
                Operation::SetStatus { .. } => {}
            }
        }
        if let Some(c) = added.intersection(&removed).next() {
            errors.push(format!("category {c} is both added and removed"));
        }
        errors
    }
}

/// New price after an adjustment, or `None` when it leaves the ±50 % band or is not positive.
pub fn adjusted(amount: i64, percent: Option<f64>, fixed: Option<i64>) -> Option<i64> {
    let new = match (percent.and_then(percent_bps), fixed) {
        (Some(bps), None) => {
            i64::try_from(div_round_half_up(
                i128::from(amount) * i128::from(10_000 + bps),
                10_000,
            ))
            .ok()?
        }
        (None, Some(a)) => amount.checked_add(a)?,
        _ => return None,
    };
    let within = i128::from((new - amount).abs()) * 100 <= i128::from(amount) * i128::from(MAX_PERCENT);
    (new > 0 && within && new <= crate::pricing::cart::MAX_AMOUNT).then_some(new)
}

// ---------------------------------------------------------------------------------------
// Stored plans

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPlan {
    /// What to change, in the staff's words (1-2000 characters).
    #[schema(example = "Raise prices of T-shirts by 5 % in SK")]
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// The model is planning (a job).
    Pending,
    /// Previewed; waiting for confirmation.
    Ready,
    /// Invalid, or nothing to do: see `errors`.
    Rejected,
    Applying,
    Applied,
    Failed,
}

impl PlanStatus {
    fn parse(s: &str) -> Self {
        match s {
            "ready" => Self::Ready,
            "rejected" => Self::Rejected,
            "applying" => Self::Applying,
            "applied" => Self::Applied,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SampleChange {
    /// `brand`, `seo_title (cs)`, `price TS-M (EUR)`, `category`, `parameter material`,
    /// `status`.
    pub what: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SampleRow {
    pub product_id: Uuid,
    pub name: String,
    pub changes: Vec<SampleChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApplyProgress {
    #[serde(default)]
    pub done: i64,
    /// Products deleted meanwhile or no longer valid for a change.
    #[serde(default)]
    pub skipped: i64,
    #[serde(default)]
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkPlan {
    pub id: Uuid,
    pub prompt: String,
    pub status: PlanStatus,
    pub plan: Option<Plan>,
    /// Why the plan was rejected (or failed).
    pub errors: Vec<String>,
    /// Matching products (frozen at preview; the apply changes exactly these).
    pub target_count: i32,
    /// Before/after of the first products.
    pub sample: Vec<SampleRow>,
    pub progress: ApplyProgress,
    /// Confirming needs a sign-in at most 15 minutes old (price changes, A9).
    pub needs_fresh_auth: bool,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub applied_at: Option<DateTime<Utc>>,
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

pub async fn create(
    tx: &mut TenantTx,
    ai: &Ai,
    actor: &str,
    input: &NewPlan,
) -> Result<BulkPlan, Error> {
    let prompt = input.prompt.trim();
    if !(1..=2000).contains(&prompt.chars().count()) {
        return Err(Error::Validation {
            code: "invalid_prompt",
            detail: "the request must be 1-2000 characters".into(),
        });
    }
    super::ensure_quota(tx, ai).await?;
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO ai_bulk_plans (id, tenant_id, prompt, created_by) VALUES ($1, $2, $3, $4)",
        id,
        tx.tenant_id(),
        prompt,
        actor
    )
    .execute(&mut **tx)
    .await?;
    enqueue(tx, PLAN_JOB, id).await?;
    get(tx, id).await
}

async fn enqueue(tx: &mut TenantTx, kind: &'static str, id: Uuid) -> Result<(), Error> {
    let mut job = NewJob::new(kind, json!({ "plan_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = 5;
    job.idempotency_key = Some(format!("{kind}:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    Ok(())
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<BulkPlan, Error> {
    let r = sqlx::query!(
        "SELECT id, prompt, status, plan, errors, target_count, sample, progress, model,
                created_at, updated_at, applied_at
         FROM ai_bulk_plans WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let plan: Option<Plan> = r.plan.map(serde_json::from_value).transpose().map_err(internal)?;
    Ok(BulkPlan {
        id: r.id,
        prompt: r.prompt,
        status: PlanStatus::parse(&r.status),
        needs_fresh_auth: plan.as_ref().is_some_and(Plan::has_price_changes),
        plan,
        errors: serde_json::from_value(r.errors).map_err(internal)?,
        target_count: r.target_count,
        sample: serde_json::from_value(r.sample).map_err(internal)?,
        progress: serde_json::from_value(r.progress).unwrap_or_default(),
        model: r.model,
        created_at: r.created_at,
        updated_at: r.updated_at,
        applied_at: r.applied_at,
    })
}

/// Confirms a previewed plan and queues its application. The caller checks fresh auth when
/// [`BulkPlan::needs_fresh_auth`]. `409 plan_not_ready` unless the plan is `ready`.
pub async fn confirm(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<BulkPlan, Error> {
    let plan = get(tx, id).await?;
    let updated = sqlx::query!(
        "UPDATE ai_bulk_plans SET status = 'applying', applied_by = $2,
                progress = jsonb_build_object('done', 0, 'skipped', 0, 'total', target_count),
                updated_at = now()
         WHERE id = $1 AND status = 'ready'",
        id,
        actor
    )
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::Conflict {
            code: "plan_not_ready",
            detail: "only a previewed (ready) plan can be applied".into(),
        });
    }
    enqueue(tx, APPLY_JOB, id).await?;
    audit::record(
        tx,
        actor,
        "ai.bulk_plan.confirmed",
        "ai_bulk_plan",
        Some(&id.to_string()),
        &json!({ "prompt": plan.prompt, "plan": plan.plan, "target_count": plan.target_count,
                 "model": plan.model }),
    )
    .await?;
    get(tx, id).await
}

// ---------------------------------------------------------------------------------------
// References (catalog data the plan may name)

struct Refs {
    /// Slug (any locale) -> category id.
    categories: HashMap<String, Uuid>,
    /// Market code or price list code -> (price list, currency).
    lists: HashMap<String, (Uuid, Currency)>,
    /// Key -> (parameter id, kind).
    parameters: HashMap<String, (Uuid, String)>,
    /// Every market locale (text parameters without a locale).
    locales: Vec<String>,
    /// For the model: categories, markets, price lists, parameters, brands.
    context: Value,
}

async fn refs(tx: &mut TenantTx) -> Result<Refs, Error> {
    let mut by_category: BTreeMap<Uuid, (Vec<String>, Vec<String>)> = BTreeMap::new();
    for r in sqlx::query!(
        "SELECT category_id, name, slug FROM category_translations ORDER BY category_id, locale"
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let e = by_category.entry(r.category_id).or_default();
        e.0.push(r.slug);
        e.1.push(r.name);
    }
    let markets = crate::markets::list(tx).await?;
    let lists = pricing::list_price_lists(tx).await?;
    let parameters = sqlx::query!(
        "SELECT id, key, name_i18n, kind, unit FROM parameters ORDER BY key LIMIT 300"
    )
    .fetch_all(&mut **tx)
    .await?;
    let brands = sqlx::query_scalar!(
        r#"SELECT DISTINCT brand AS "brand!" FROM products WHERE brand IS NOT NULL
           ORDER BY 1 LIMIT 200"#
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut list_by_code = HashMap::new();
    for l in &lists {
        list_by_code.insert(l.code.clone(), (l.id, l.currency));
    }
    let mut market_context = vec![];
    for m in &markets {
        let list = lists.iter().find(|l| l.market_ids.contains(&m.id));
        if let Some(l) = list {
            list_by_code.insert(m.code.clone(), (l.id, l.currency));
        }
        market_context.push(json!({
            "code": m.code, "name": m.name, "currency": m.currency,
            "countries": m.country_codes, "locales": m.locales,
            "price_list": list.map(|l| l.code.clone()),
        }));
    }
    let locales: BTreeSet<String> = markets.iter().flat_map(|m| m.locales.clone()).collect();
    let context = json!({
        "locales": locales,
        "categories": by_category.values().map(|(slugs, names)| json!({ "slugs": slugs, "names": names })).collect::<Vec<_>>(),
        "brands": brands,
        "markets": market_context,
        "price_lists": lists.iter().map(|l| json!({ "code": l.code, "name": l.name, "currency": l.currency })).collect::<Vec<_>>(),
        "parameters": parameters.iter().map(|p| json!({
            "key": p.key, "names": p.name_i18n, "kind": p.kind, "unit": p.unit,
        })).collect::<Vec<_>>(),
    });
    Ok(Refs {
        categories: by_category
            .iter()
            .flat_map(|(id, (slugs, _))| slugs.iter().map(move |s| (s.clone(), *id)))
            .collect(),
        lists: list_by_code,
        parameters: parameters
            .into_iter()
            .map(|p| (p.key, (p.id, p.kind)))
            .collect(),
        locales: locales.into_iter().collect(),
        context,
    })
}

/// A parameter value as stored for its kind (`None`: not parseable).
fn parameter_value(kind: &str, value: &str) -> Option<Value> {
    match kind {
        "number" => {
            let n: f64 = value.trim().replace(',', ".").parse().ok()?;
            serde_json::Number::from_f64(n).map(Value::Number)
        }
        "bool" => match value.trim().to_lowercase().as_str() {
            "true" | "yes" | "ano" | "áno" | "1" => Some(Value::Bool(true)),
            "false" | "no" | "ne" | "nie" | "0" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => Some(Value::String(value.trim().to_owned())),
    }
}

/// Unknown categories, markets and parameters named by the plan.
fn check_refs(plan: &Plan, r: &Refs) -> Vec<String> {
    let mut errors = vec![];
    let category = |slug: &String, errors: &mut Vec<String>| {
        if !r.categories.contains_key(slug) {
            errors.push(format!("unknown category {slug:?}"));
        }
    };
    let market = |code: &String, errors: &mut Vec<String>| {
        if !r.lists.contains_key(code) {
            errors.push(format!("unknown market or price list {code:?}"));
        }
    };
    let parameter = |key: &String, value: &str, errors: &mut Vec<String>| match r.parameters.get(key) {
        None => errors.push(format!("unknown parameter {key:?}")),
        Some((_, kind)) if parameter_value(kind, value).is_none() => {
            errors.push(format!("{value:?} is not a valid {kind} value for {key}"));
        }
        Some(_) => {}
    };
    for c in &plan.selector.categories {
        category(c, &mut errors);
    }
    for f in &plan.selector.parameters {
        parameter(&f.parameter, &f.value, &mut errors);
    }
    if let Some(p) = &plan.selector.price {
        market(&p.market, &mut errors);
    }
    for op in &plan.operations {
        match op {
            Operation::AdjustPrice { market: m, .. } => market(m, &mut errors),
            Operation::AddCategory { category: c } | Operation::RemoveCategory { category: c } => {
                category(c, &mut errors);
            }
            Operation::SetParameter {
                parameter: p,
                value,
                ..
            } => parameter(p, value, &mut errors),
            Operation::SetField { .. } | Operation::SetStatus { .. } => {}
        }
    }
    errors
}

/// The products the selector matches, by id.
async fn targets(tx: &mut TenantTx, s: &Selector, r: &Refs) -> Result<Vec<Uuid>, Error> {
    let roots: Vec<Uuid> = s
        .categories
        .iter()
        .filter_map(|c| r.categories.get(c).copied())
        .collect();
    let categories = if roots.is_empty() {
        vec![]
    } else {
        sqlx::query_scalar!(
            r#"WITH RECURSIVE sub AS (
                   SELECT id FROM categories WHERE id = ANY($1)
                   UNION SELECT c.id FROM categories c JOIN sub ON c.parent_id = sub.id)
               SELECT id AS "id!" FROM sub"#,
            &roots
        )
        .fetch_all(&mut **tx)
        .await?
    };
    let brands: Vec<String> = s.brands.iter().map(|b| b.trim().to_lowercase()).collect();
    let statuses: Vec<String> = s.statuses.iter().map(|x| x.as_str().to_owned()).collect();
    let price = s
        .price
        .as_ref()
        .and_then(|p| r.lists.get(&p.market).map(|l| (l.0, p.min_minor, p.max_minor)));
    let mut ids = sqlx::query_scalar!(
        r#"SELECT p.id FROM products p
           WHERE (cardinality($1::uuid[]) = 0 OR EXISTS (
                     SELECT 1 FROM product_categories pc
                     WHERE pc.product_id = p.id AND pc.category_id = ANY($1)))
             AND (cardinality($2::text[]) = 0 OR lower(p.brand) = ANY($2))
             AND (cardinality($3::text[]) = 0 OR p.status = ANY($3))
             AND ($4::uuid IS NULL OR EXISTS (
                     SELECT 1 FROM variants v
                     JOIN variant_prices vp ON vp.variant_id = v.id AND vp.price_list_id = $4
                     WHERE v.product_id = p.id
                       AND ($5::bigint IS NULL OR vp.amount_minor >= $5)
                       AND ($6::bigint IS NULL OR vp.amount_minor <= $6)))
           ORDER BY p.id"#,
        &categories,
        &brands,
        &statuses,
        price.map(|p| p.0),
        price.and_then(|p| p.1),
        price.and_then(|p| p.2),
    )
    .fetch_all(&mut **tx)
    .await?;
    for f in &s.parameters {
        let Some((pid, kind)) = r.parameters.get(&f.parameter) else {
            continue;
        };
        let value = parameter_value(kind, &f.value);
        let matching: BTreeSet<Uuid> = sqlx::query_scalar!(
            r#"SELECT DISTINCT product_id AS "id!" FROM product_parameter_values
               WHERE parameter_id = $1 AND (
                   (jsonb_typeof(value) = 'object' AND EXISTS (
                       SELECT 1 FROM jsonb_each_text(value) e WHERE lower(e.value) = lower($2)))
                   OR (jsonb_typeof(value) <> 'object' AND value = $3))"#,
            pid,
            f.value.trim(),
            value.unwrap_or(Value::Null)
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
        ids.retain(|id| matching.contains(id));
    }
    Ok(ids)
}

/// Applies the non-price operations to a product document. Returns what changed (for the
/// preview); an unchanged document means nothing to write.
fn apply_to_input(ops: &[Operation], r: &Refs, input: &mut ProductInput) -> Vec<SampleChange> {
    let mut changes = vec![];
    let mut change = |what: String, before: String, after: String| {
        if before != after {
            changes.push(SampleChange {
                what,
                before,
                after,
            });
        }
    };
    for op in ops {
        match op {
            Operation::SetField {
                field: PlanField::Brand,
                value,
                ..
            } => {
                let before = input.brand.clone().unwrap_or_default();
                input.brand = Some(value.trim().to_owned());
                change("brand".into(), before, value.trim().to_owned());
            }
            Operation::SetField {
                field,
                locale: Some(locale),
                value,
            } => {
                let Some(t) = input.translations.iter_mut().find(|t| &t.locale == locale) else {
                    continue;
                };
                let value = value.trim().to_owned();
                let before = match field {
                    PlanField::ShortDescription => {
                        std::mem::replace(&mut t.short_description, value.clone())
                    }
                    PlanField::SeoTitle => t.seo_title.replace(value.clone()).unwrap_or_default(),
                    PlanField::SeoDescription => {
                        t.seo_description.replace(value.clone()).unwrap_or_default()
                    }
                    PlanField::Brand => continue,
                };
                change(format!("{} ({locale})", field.as_str()), before, value);
            }
            Operation::SetField { .. } | Operation::AdjustPrice { .. } => {}
            Operation::AddCategory { category } => {
                if let Some(id) = r.categories.get(category)
                    && !input.category_ids.contains(id)
                {
                    input.category_ids.push(*id);
                    change("category".into(), String::new(), format!("+ {category}"));
                }
            }
            Operation::RemoveCategory { category } => {
                if let Some(id) = r.categories.get(category)
                    && input.category_ids.contains(id)
                {
                    input.category_ids.retain(|c| c != id);
                    change("category".into(), category.clone(), format!("− {category}"));
                }
            }
            Operation::SetParameter {
                parameter,
                value,
                locale,
            } => {
                let Some((pid, kind)) = r.parameters.get(parameter) else {
                    continue;
                };
                let Some(parsed) = parameter_value(kind, value) else {
                    continue;
                };
                let slot = input
                    .parameters
                    .iter()
                    .position(|p| p.parameter_id == *pid && p.variant_sku.is_none());
                let before = slot.map(|i| input.parameters[i].value.clone());
                let new = match parsed {
                    Value::String(text) => {
                        let mut map: I18n = before
                            .as_ref()
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                        let locales = match locale {
                            Some(l) => vec![l.clone()],
                            None => r.locales.clone(),
                        };
                        for l in locales {
                            map.insert(l, text.clone());
                        }
                        serde_json::to_value(map).unwrap_or(Value::Null)
                    }
                    other => other,
                };
                let show = |v: &Option<Value>| v.as_ref().map(Value::to_string).unwrap_or_default();
                change(
                    format!("parameter {parameter}"),
                    show(&before),
                    show(&Some(new.clone())),
                );
                match slot {
                    Some(i) => input.parameters[i].value = new,
                    None => input.parameters.push(ParameterValue {
                        parameter_id: *pid,
                        variant_sku: None,
                        value: new,
                    }),
                }
            }
            Operation::SetStatus { status } => {
                let before = input.status;
                input.status = *status;
                change(
                    "status".into(),
                    before.as_str().into(),
                    status.as_str().into(),
                );
            }
        }
    }
    changes
}

fn money(minor: i64, currency: Currency) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let m = minor.unsigned_abs();
    format!("{sign}{}.{:02} {currency}", m / 100, m % 100)
}

/// `(variant id, sku, list, currency, before, after)` of every price the plan changes on these
/// products; `Err` lists the prices outside the ±50 % band.
async fn price_changes(
    tx: &mut TenantTx,
    plan: &Plan,
    r: &Refs,
    products: &[Uuid],
) -> Result<Result<Vec<PriceMove>, Vec<String>>, Error> {
    let mut moves = vec![];
    let mut errors = vec![];
    for op in &plan.operations {
        let Operation::AdjustPrice {
            market,
            percent,
            amount_minor,
        } = op
        else {
            continue;
        };
        let Some(&(list, currency)) = r.lists.get(market) else {
            continue;
        };
        let rows = sqlx::query!(
            "SELECT v.product_id, v.id, v.sku, vp.amount_minor, vp.compare_at_minor
             FROM variants v
             JOIN variant_prices vp ON vp.variant_id = v.id AND vp.price_list_id = $1
             WHERE v.product_id = ANY($2) ORDER BY v.product_id, v.position",
            list,
            products
        )
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            // Free items stay free.
            if row.amount_minor == 0 {
                continue;
            }
            match adjusted(row.amount_minor, *percent, *amount_minor) {
                Some(after) => moves.push(PriceMove {
                    product_id: row.product_id,
                    variant_id: row.id,
                    sku: row.sku,
                    list,
                    currency,
                    before: row.amount_minor,
                    after,
                    compare_at: row.compare_at_minor,
                }),
                None => errors.push(format!(
                    "{}: {} would leave the allowed ±{MAX_PERCENT} % band or drop to zero",
                    row.sku,
                    money(row.amount_minor, currency)
                )),
            }
        }
    }
    Ok(if errors.is_empty() {
        Ok(moves)
    } else {
        errors.truncate(5);
        Err(errors)
    })
}

#[derive(Debug, Clone)]
struct PriceMove {
    product_id: Uuid,
    variant_id: Uuid,
    sku: String,
    list: Uuid,
    currency: Currency,
    before: i64,
    after: i64,
    compare_at: Option<i64>,
}

// ---------------------------------------------------------------------------------------
// Planning (the job): model -> validation -> targets -> preview

pub async fn run_plan(
    db: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    id: Uuid,
    last_attempt: bool,
) -> Result<Outcome, Error> {
    let (prompt, actor, context) = {
        let mut tx = tenant_tx(db, tenant).await?;
        let row = sqlx::query!(
            "SELECT prompt, status, created_by FROM ai_bulk_plans WHERE id = $1",
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
        if row.status != "pending" {
            return Ok(Outcome::Done);
        }
        let r = refs(&mut tx).await?;
        tx.commit().await?;
        (row.prompt, row.created_by, r.context)
    };
    let data = json!({ "prompt": prompt, "catalog": context });
    let result = super::call(
        db,
        ai,
        tenant,
        &actor,
        &Call {
            feature: super::BULK_PLAN,
            system: prompts::PLAN_SYSTEM,
            task: "Plan the staff request from the data block as a change plan.",
            data: &data,
            schema: &prompts::plan_schema(),
            max_tokens: 4000,
        },
    )
    .await;
    let (output, model) = match result {
        Ok(ok) => ok,
        Err(e) if e.retryable() && !last_attempt => return Ok(Outcome::Retry(e.to_string())),
        Err(e) => {
            finish(db, tenant, id, "failed", None, &[e.code().to_owned()], None).await?;
            return Ok(Outcome::Done);
        }
    };
    let plan: Plan = match super::parse::<Plan>(output) {
        Ok(p) => p,
        Err(CallError::Failed { detail, .. }) => {
            let msg = format!("the model returned an invalid plan: {detail}");
            finish(db, tenant, id, "rejected", None, &[msg], Some(&model)).await?;
            return Ok(Outcome::Done);
        }
        Err(e) => return Err(Error::Internal(e.to_string())),
    };
    let mut errors = plan.validate();
    if !plan.explanation.trim().is_empty() && plan.operations.is_empty() {
        errors.push(plan.explanation.trim().to_owned());
    }
    let mut tx = tenant_tx(db, tenant).await?;
    let r = refs(&mut tx).await?;
    errors.extend(check_refs(&plan, &r));
    if !errors.is_empty() {
        tx.commit().await?;
        finish(db, tenant, id, "rejected", Some(&plan), &errors, Some(&model)).await?;
        return Ok(Outcome::Done);
    }
    let ids = targets(&mut tx, &plan.selector, &r).await?;
    if ids.is_empty() {
        errors.push("no product matches the selection".into());
    } else if ids.len() > MAX_TARGETS {
        errors.push(format!(
            "{} products match; a plan may change at most {MAX_TARGETS}: narrow the selection",
            ids.len()
        ));
    }
    let moves = if errors.is_empty() {
        match price_changes(&mut tx, &plan, &r, &ids).await? {
            Ok(m) => m,
            Err(mut e) => {
                errors.append(&mut e);
                vec![]
            }
        }
    } else {
        vec![]
    };
    if !errors.is_empty() {
        tx.commit().await?;
        finish(db, tenant, id, "rejected", Some(&plan), &errors, Some(&model)).await?;
        return Ok(Outcome::Done);
    }
    // Preview (dry run): the first products with their before/after.
    let mut sample = vec![];
    for pid in ids.iter().take(SAMPLE) {
        let p = products::get(&mut tx, *pid).await?;
        let mut input = product_input(&p);
        let mut changes = apply_to_input(&plan.operations, &r, &mut input);
        changes.extend(moves.iter().filter(|m| m.product_id == *pid).map(|m| SampleChange {
            what: format!("price {} ({})", m.sku, m.currency),
            before: money(m.before, m.currency),
            after: money(m.after, m.currency),
        }));
        let name = r
            .locales
            .iter()
            .find_map(|l| p.translations.iter().find(|t| &t.locale == l))
            .or_else(|| p.translations.first())
            .map(|t| t.name.clone())
            .unwrap_or_default();
        sample.push(SampleRow {
            product_id: *pid,
            name,
            changes,
        });
    }
    sqlx::query!(
        "INSERT INTO ai_bulk_items (tenant_id, plan_id, product_id)
         SELECT $1, $2, unnest($3::uuid[])",
        tenant,
        id,
        &ids
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE ai_bulk_plans SET status = 'ready', plan = $2, target_count = $3, sample = $4,
                model = $5, updated_at = now()
         WHERE id = $1 AND status = 'pending'",
        id,
        serde_json::to_value(&plan).map_err(internal)?,
        i32::try_from(ids.len()).unwrap_or(i32::MAX),
        serde_json::to_value(&sample).map_err(internal)?,
        model
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Outcome::Done)
}

async fn finish(
    db: &PgPool,
    tenant: Uuid,
    id: Uuid,
    status: &str,
    plan: Option<&Plan>,
    errors: &[String],
    model: Option<&str>,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    sqlx::query!(
        "UPDATE ai_bulk_plans SET status = $2, plan = $3, errors = $4, model = $5,
                updated_at = now()
         WHERE id = $1 AND status = 'pending'",
        id,
        status,
        plan.map(serde_json::to_value).transpose().map_err(internal)?,
        json!(errors),
        model
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Applying (the job): one product per transaction, marked done in the same transaction.

pub async fn run_apply(db: &PgPool, tenant: Uuid, id: Uuid) -> Result<Outcome, Error> {
    let r = {
        let mut tx = tenant_tx(db, tenant).await?;
        let r = refs(&mut tx).await?;
        tx.commit().await?;
        r
    };
    loop {
        let mut tx = tenant_tx(db, tenant).await?;
        let row = sqlx::query!(
            "SELECT status, plan, applied_by FROM ai_bulk_plans WHERE id = $1",
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
        if row.status != "applying" {
            return Ok(Outcome::Done);
        }
        let plan: Plan = row
            .plan
            .map(serde_json::from_value)
            .transpose()
            .map_err(internal)?
            .ok_or_else(|| Error::Internal("applying plan without a plan".into()))?;
        let actor = row.applied_by.unwrap_or_default();
        let next = sqlx::query_scalar!(
            "SELECT product_id FROM ai_bulk_items WHERE plan_id = $1 AND status = 'pending'
             ORDER BY product_id LIMIT 1 FOR UPDATE SKIP LOCKED",
            id
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(product_id) = next else {
            let progress = progress(&mut tx, id).await?;
            sqlx::query!(
                "UPDATE ai_bulk_plans SET status = 'applied', applied_at = now(), progress = $2,
                        updated_at = now()
                 WHERE id = $1",
                id,
                serde_json::to_value(&progress).map_err(internal)?
            )
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                &actor,
                "ai.bulk_plan.applied",
                "ai_bulk_plan",
                Some(&id.to_string()),
                &json!({ "done": progress.done, "skipped": progress.skipped }),
            )
            .await?;
            tx.commit().await?;
            return Ok(Outcome::Done);
        };
        let status = match apply_one(&mut tx, &actor, &plan, &r, product_id).await {
            Ok(done) => {
                if done { "done" } else { "skipped" }
            }
            Err(e) if e.status().is_client_error() => {
                // Invalid for this product now (validation, a slug clash, ...): skip it in a
                // fresh transaction, the failed one may be aborted.
                tracing::info!(plan = %id, product = %product_id, code = e.code(), "bulk plan skipped a product");
                tx.rollback().await?;
                tx = tenant_tx(db, tenant).await?;
                "skipped"
            }
            Err(e) => return Err(e),
        };
        sqlx::query!(
            "UPDATE ai_bulk_items SET status = $3 WHERE plan_id = $1 AND product_id = $2",
            id,
            product_id,
            status
        )
        .execute(&mut *tx)
        .await?;
        let progress = progress(&mut tx, id).await?;
        sqlx::query!(
            "UPDATE ai_bulk_plans SET progress = $2, updated_at = now() WHERE id = $1",
            id,
            serde_json::to_value(&progress).map_err(internal)?
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
}

async fn progress(tx: &mut TenantTx, id: Uuid) -> Result<ApplyProgress, Error> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE status = 'done') AS "done!",
                  count(*) FILTER (WHERE status = 'skipped') AS "skipped!",
                  count(*) AS "total!"
           FROM ai_bulk_items WHERE plan_id = $1"#,
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(ApplyProgress {
        done: r.done,
        skipped: r.skipped,
        total: r.total,
    })
}

/// Applies the plan to one product through the services. `false`: skipped (deleted, or a
/// price moved outside the band since the preview).
async fn apply_one(
    tx: &mut TenantTx,
    actor: &str,
    plan: &Plan,
    r: &Refs,
    product_id: Uuid,
) -> Result<bool, Error> {
    let product = match products::get(tx, product_id).await {
        Ok(p) => p,
        Err(Error::NotFound) => return Ok(false),
        Err(e) => return Err(e),
    };
    let moves = match price_changes(tx, plan, r, &[product_id]).await? {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    let before = product_input(&product);
    let mut input = before.clone();
    apply_to_input(&plan.operations, r, &mut input);
    if input != before {
        products::replace(tx, actor, product_id, &input).await?;
    }
    let mut by_list: BTreeMap<Uuid, Vec<PriceItem>> = BTreeMap::new();
    for m in moves {
        by_list.entry(m.list).or_default().push(PriceItem {
            variant_id: m.variant_id,
            amount_minor: m.after,
            // A "was" price must stay above the price; drop it when the price reaches it.
            compare_at_minor: m.compare_at.filter(|c| *c > m.after),
        });
    }
    for (list, items) in by_list {
        pricing::upsert_prices(
            tx,
            actor,
            list,
            &PriceUpsert {
                reason: PriceChangeReason::Base,
                imported: false,
                items,
            },
        )
        .await?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(ops: Value) -> Result<Plan, serde_json::Error> {
        serde_json::from_value(json!({
            "explanation": "x",
            "selector": {"categories": ["trika"], "brands": [], "statuses": [], "parameters": [], "price": null},
            "operations": ops,
        }))
    }

    #[test]
    fn unknown_operations_and_fields_are_rejected() {
        assert!(plan(json!([{"op": "delete_products"}])).is_err());
        assert!(plan(json!([{"op": "set_field", "field": "price", "locale": null, "value": "1"}])).is_err());
        assert!(plan(json!([{"op": "set_status", "status": "archived", "sql": "DROP TABLE"}])).is_err());
        assert!(plan(json!([{"op": "set_status", "status": "gone"}])).is_err());
        let extra: Result<Plan, _> = serde_json::from_value(json!({
            "explanation": "x", "operations": [], "tool": "exfiltrate",
            "selector": {"categories": [], "brands": [], "statuses": [], "parameters": [], "price": null},
        }));
        assert!(extra.is_err());
    }

    #[test]
    fn caps_and_shapes_are_enforced() {
        let errs = |ops: Value| plan(ops).unwrap().validate();
        assert!(errs(json!([{"op": "adjust_price", "market": "sk", "percent": 5, "amount_minor": null}])).is_empty());
        assert!(errs(json!([{"op": "adjust_price", "market": "sk", "percent": -50, "amount_minor": null}])).is_empty());
        assert!(!errs(json!([{"op": "adjust_price", "market": "sk", "percent": 50.01, "amount_minor": null}])).is_empty());
        assert!(!errs(json!([{"op": "adjust_price", "market": "sk", "percent": 80, "amount_minor": null}])).is_empty());
        assert!(!errs(json!([{"op": "adjust_price", "market": "sk", "percent": 5, "amount_minor": 100}])).is_empty());
        assert!(!errs(json!([{"op": "adjust_price", "market": "sk", "percent": null, "amount_minor": null}])).is_empty());
        assert!(!errs(json!([{"op": "adjust_price", "market": "sk", "percent": 0.001, "amount_minor": null}])).is_empty());
        assert!(!errs(json!([])).is_empty(), "no operations");
        let eleven: Vec<Value> = (0..11).map(|_| json!({"op": "set_status", "status": "draft"})).collect();
        assert!(!errs(Value::Array(eleven)).is_empty());
        assert!(!errs(json!([{"op": "set_field", "field": "seo_title", "locale": null, "value": "x"}])).is_empty());
        assert!(!errs(json!([{"op": "set_field", "field": "brand", "locale": "cs", "value": "x"}])).is_empty());
        assert!(errs(json!([{"op": "set_field", "field": "brand", "locale": null, "value": "Acme"}])).is_empty());
        assert!(!errs(json!([
            {"op": "add_category", "category": "a"}, {"op": "remove_category", "category": "a"}
        ])).is_empty());
    }

    #[test]
    fn price_adjustments_round_and_stay_in_band() {
        assert_eq!(adjusted(520, Some(5.0), None), Some(546));
        assert_eq!(adjusted(12_900, Some(5.0), None), Some(13_545));
        assert_eq!(adjusted(999, Some(-10.0), None), Some(899)); // 899.1
        assert_eq!(adjusted(1_000, Some(2.5), None), Some(1_025));
        assert_eq!(adjusted(1_000, None, Some(500)), Some(1_500));
        assert_eq!(adjusted(1_000, None, Some(501)), None, "over +50 %");
        assert_eq!(adjusted(1_000, None, Some(-600)), None, "under -50 %");
        assert_eq!(adjusted(1, Some(-50.0), None), Some(1), "rounds half away from zero");
        assert_eq!(adjusted(0, Some(5.0), None), None, "free items stay untouched");
    }

    #[test]
    fn parameter_values_follow_the_kind() {
        assert_eq!(parameter_value("number", "2,5"), Some(json!(2.5)));
        assert_eq!(parameter_value("number", "lots"), None);
        assert_eq!(parameter_value("bool", "Ano"), Some(json!(true)));
        assert_eq!(parameter_value("text", " bavlna "), Some(json!("bavlna")));
    }
}
