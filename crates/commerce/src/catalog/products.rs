//! Products as one document: attributes, translations, options, variants, categories, media,
//! parameter values and tax categories. `create` and `replace` share validation and saving;
//! variants are upserted by id (prices and stock reference them), every other child
//! collection is replaced. Array order is position.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    I18n, MAX_HTML, check_i18n, check_opt_text, check_text, code_valid, db_error, ean_valid,
    sanitize_html, slug_valid, tax,
};
use crate::audit;
use crate::markets::{invalid, is_locale};

pub const MAX_VARIANTS: usize = 500;
pub const MAX_OPTIONS: usize = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProductStatus {
    #[default]
    Draft,
    Active,
    Archived,
}

impl ProductStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "archived" => Self::Archived,
            _ => Self::Draft,
        }
    }
}

/// Basis of the unit price shown next to the price (per kg, per l, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnitMeasure {
    Kg,
    L,
    M,
    M2,
    Pcs,
}

impl UnitMeasure {
    fn as_str(self) -> &'static str {
        match self {
            Self::Kg => "kg",
            Self::L => "l",
            Self::M => "m",
            Self::M2 => "m2",
            Self::Pcs => "pcs",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "kg" => Self::Kg,
            "l" => Self::L,
            "m" => Self::M,
            "m2" => Self::M2,
            "pcs" => Self::Pcs,
            _ => return None,
        })
    }
}

/// EU General Product Safety Regulation data (Regulation (EU) 2023/988, art. 19).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Gpsr {
    pub manufacturer: Option<GpsrParty>,
    /// Required when the manufacturer is established outside the EU.
    pub eu_responsible_person: Option<GpsrParty>,
    /// Safety information per locale.
    #[serde(default)]
    pub safety_info: I18n,
    /// Warnings per locale.
    #[serde(default)]
    pub warnings: I18n,
}

/// Name, postal address and an electronic contact (email or URL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GpsrParty {
    pub name: String,
    pub address: String,
    pub email: Option<String>,
    pub url: Option<String>,
    pub phone: Option<String>,
}

impl Gpsr {
    pub fn validate(&self) -> Result<(), Error> {
        const CODE: &str = "invalid_gpsr";
        for party in [&self.manufacturer, &self.eu_responsible_person]
            .into_iter()
            .flatten()
        {
            check_text("gpsr party name", CODE, &party.name, 1, 200)?;
            check_text("gpsr party address", CODE, &party.address, 1, 500)?;
            check_opt_text("gpsr party email", CODE, party.email.as_deref(), 254)?;
            check_opt_text("gpsr party url", CODE, party.url.as_deref(), 500)?;
            check_opt_text("gpsr party phone", CODE, party.phone.as_deref(), 50)?;
            if party.email.as_deref().is_some_and(|e| !e.contains('@')) {
                return Err(invalid(CODE, "gpsr party email is not an email address"));
            }
            if party
                .url
                .as_deref()
                .is_some_and(|u| !(u.starts_with("https://") || u.starts_with("http://")))
            {
                return Err(invalid(CODE, "gpsr party url must be http(s)"));
            }
            if party.email.is_none() && party.url.is_none() {
                return Err(invalid(
                    CODE,
                    "gpsr party needs an electronic contact (email or url)",
                ));
            }
        }
        check_i18n("gpsr safety_info", CODE, &self.safety_info, 5000, false)?;
        check_i18n("gpsr warnings", CODE, &self.warnings, 5000, false)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductTranslation {
    #[schema(example = "cs")]
    pub locale: String,
    #[schema(example = "Tričko Basic")]
    pub name: String,
    /// Unique per tenant and locale.
    #[schema(example = "tricko-basic")]
    pub slug: String,
    /// Sanitized on write: unsafe markup is removed.
    #[serde(default)]
    pub description_html: String,
    #[serde(default)]
    pub short_description: String,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductOption {
    /// Referenced by `variants[].option_values`, e.g. `color`.
    #[schema(example = "color")]
    pub code: String,
    pub name_i18n: I18n,
    pub values: Vec<OptionValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionValue {
    #[schema(example = "red")]
    pub code: String,
    pub name_i18n: I18n,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantInput {
    /// Existing variant to update; omit for a new variant.
    pub id: Option<Uuid>,
    /// Unique per tenant.
    #[schema(example = "TS-RED-M")]
    pub sku: String,
    /// GTIN-8/12/13/14 with a valid check digit.
    pub ean: Option<String>,
    /// One value code per product option: `{"color": "red", "size": "m"}`.
    #[serde(default)]
    pub option_values: BTreeMap<String, String>,
    pub weight_g: Option<i32>,
    /// At most one; defaults to the first variant.
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Variant {
    pub id: Uuid,
    pub sku: String,
    pub ean: Option<String>,
    pub option_values: BTreeMap<String, String>,
    pub weight_g: Option<i32>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductMedia {
    pub asset_id: Uuid,
    /// Shows the image for one variant only (by SKU).
    pub variant_sku: Option<String>,
    #[serde(default)]
    pub alt_i18n: I18n,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterValue {
    pub parameter_id: Uuid,
    /// Variant-level value (by SKU); omit for a product-level value.
    pub variant_sku: Option<String>,
    /// `{"cs": "bavlna"}` for text parameters, a number or a boolean otherwise.
    pub value: Value,
}

/// A product document, as written by `POST /products` and `PUT /products/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProductInput {
    #[serde(default)]
    pub status: ProductStatus,
    pub brand: Option<String>,
    #[serde(default)]
    pub gpsr: Gpsr,
    pub unit_measure: Option<UnitMeasure>,
    /// Content in `unit_measure` units (0.75 for 750 ml with `l`); set together with it.
    pub unit_quantity: Option<f64>,
    pub heureka_category: Option<String>,
    pub google_category: Option<String>,
    pub translations: Vec<ProductTranslation>,
    #[serde(default)]
    pub options: Vec<ProductOption>,
    #[serde(default)]
    pub variants: Vec<VariantInput>,
    #[serde(default)]
    pub category_ids: Vec<Uuid>,
    /// In display order.
    #[serde(default)]
    pub media: Vec<ProductMedia>,
    #[serde(default)]
    pub parameters: Vec<ParameterValue>,
    /// Country -> tax category code (`reduced`, ...); countries not listed use `standard` (A3).
    #[serde(default)]
    pub tax_categories: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Product {
    pub id: Uuid,
    pub status: ProductStatus,
    pub brand: Option<String>,
    pub gpsr: Gpsr,
    pub unit_measure: Option<UnitMeasure>,
    pub unit_quantity: Option<f64>,
    pub heureka_category: Option<String>,
    pub google_category: Option<String>,
    pub translations: Vec<ProductTranslation>,
    pub options: Vec<ProductOption>,
    pub variants: Vec<Variant>,
    pub category_ids: Vec<Uuid>,
    pub media: Vec<ProductMedia>,
    pub parameters: Vec<ParameterValue>,
    pub tax_categories: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn opt_str(v: &Option<String>) -> Option<&str> {
    v.as_deref()
}

impl ProductInput {
    /// Everything that can be checked without the database.
    pub fn validate(&self) -> Result<(), Error> {
        check_opt_text("brand", "invalid_brand", opt_str(&self.brand), 200)?;
        check_opt_text(
            "heureka_category",
            "invalid_category_path",
            opt_str(&self.heureka_category),
            500,
        )?;
        check_opt_text(
            "google_category",
            "invalid_category_path",
            opt_str(&self.google_category),
            500,
        )?;
        self.gpsr.validate()?;
        match (self.unit_measure, self.unit_quantity) {
            (None, None) => {}
            (Some(_), Some(q)) if unit_quantity_valid(q) => {}
            _ => {
                return Err(invalid(
                    "invalid_unit",
                    "unit_measure and unit_quantity (0.0001-1000000, at most 4 decimals) are set together",
                ));
            }
        }
        self.validate_translations()?;
        self.validate_options_and_variants()?;
        let skus: BTreeSet<&str> = self.variants.iter().map(|v| v.sku.as_str()).collect();

        if self.category_ids.len() > 50 || !all_unique(self.category_ids.iter()) {
            return Err(invalid(
                "invalid_categories",
                "category_ids must be at most 50 distinct ids",
            ));
        }
        if self.media.len() > 100 || !all_unique(self.media.iter().map(|m| m.asset_id)) {
            return Err(invalid(
                "invalid_media",
                "media must be at most 100 distinct assets",
            ));
        }
        for m in &self.media {
            check_i18n("alt_i18n", "invalid_media", &m.alt_i18n, 500, false)?;
            if m.variant_sku.as_deref().is_some_and(|s| !skus.contains(s)) {
                return Err(invalid(
                    "invalid_media",
                    "media variant_sku is not a variant",
                ));
            }
        }
        if self.parameters.len() > 500
            || !all_unique(
                self.parameters
                    .iter()
                    .map(|p| (p.parameter_id, p.variant_sku.as_deref())),
            )
        {
            return Err(invalid(
                "invalid_parameters",
                "at most 500 parameter values, one per parameter and variant",
            ));
        }
        if self
            .parameters
            .iter()
            .any(|p| p.variant_sku.as_deref().is_some_and(|s| !skus.contains(s)))
        {
            return Err(invalid(
                "invalid_parameters",
                "parameter variant_sku is not a variant",
            ));
        }
        for (country, code) in &self.tax_categories {
            if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(invalid(
                    "invalid_tax_category",
                    "tax_categories keys are ISO 3166-1 alpha-2 codes",
                ));
            }
            if !tax::CODES.contains(&code.as_str()) {
                return Err(invalid(
                    "invalid_tax_category",
                    format!("unknown tax category {code:?}"),
                ));
            }
        }
        Ok(())
    }

    fn validate_translations(&self) -> Result<(), Error> {
        const CODE: &str = "invalid_translation";
        if self.translations.is_empty() || self.translations.len() > 20 {
            return Err(invalid(CODE, "1-20 translations are required"));
        }
        if !all_unique(self.translations.iter().map(|t| t.locale.as_str())) {
            return Err(invalid(CODE, "one translation per locale"));
        }
        for t in &self.translations {
            if !is_locale(&t.locale) {
                return Err(invalid(CODE, format!("invalid locale {:?}", t.locale)));
            }
            check_text("name", CODE, &t.name, 1, 300)?;
            if !slug_valid(&t.slug) {
                return Err(invalid(
                    "invalid_slug",
                    "slug must be lowercase letters and digits joined by hyphens",
                ));
            }
            check_text("description_html", CODE, &t.description_html, 0, MAX_HTML)?;
            check_text("short_description", CODE, &t.short_description, 0, 1000)?;
            check_opt_text("seo_title", CODE, opt_str(&t.seo_title), 200)?;
            check_opt_text("seo_description", CODE, opt_str(&t.seo_description), 500)?;
        }
        Ok(())
    }

    fn validate_options_and_variants(&self) -> Result<(), Error> {
        const OPT: &str = "invalid_options";
        const VAR: &str = "invalid_variants";
        if self.options.len() > MAX_OPTIONS {
            return Err(invalid(OPT, format!("at most {MAX_OPTIONS} options")));
        }
        if !all_unique(self.options.iter().map(|o| o.code.as_str())) {
            return Err(invalid(OPT, "option codes must be unique"));
        }
        for o in &self.options {
            if !code_valid(&o.code) {
                return Err(invalid(OPT, format!("invalid option code {:?}", o.code)));
            }
            check_i18n("option name_i18n", OPT, &o.name_i18n, 100, true)?;
            if o.values.is_empty() || o.values.len() > 100 {
                return Err(invalid(OPT, "every option needs 1-100 values"));
            }
            if !all_unique(o.values.iter().map(|v| v.code.as_str())) {
                return Err(invalid(OPT, "value codes must be unique per option"));
            }
            for v in &o.values {
                if !code_valid(&v.code) {
                    return Err(invalid(OPT, format!("invalid value code {:?}", v.code)));
                }
                check_i18n("value name_i18n", OPT, &v.name_i18n, 100, true)?;
            }
        }

        if self.variants.len() > MAX_VARIANTS {
            return Err(invalid(VAR, format!("at most {MAX_VARIANTS} variants")));
        }
        if self.status == ProductStatus::Active && self.variants.is_empty() {
            return Err(invalid(VAR, "an active product needs at least one variant"));
        }
        if !all_unique(self.variants.iter().filter_map(|v| v.id)) {
            return Err(invalid(VAR, "variant ids must be unique"));
        }
        if !all_unique(self.variants.iter().map(|v| v.sku.as_str())) {
            return Err(invalid(VAR, "SKUs must be unique"));
        }
        if !all_unique(self.variants.iter().map(|v| &v.option_values)) {
            return Err(invalid(VAR, "two variants have the same option values"));
        }
        if self.variants.iter().filter(|v| v.is_default).count() > 1 {
            return Err(invalid(VAR, "at most one default variant"));
        }
        let options: BTreeMap<&str, BTreeSet<&str>> = self
            .options
            .iter()
            .map(|o| {
                (
                    o.code.as_str(),
                    o.values.iter().map(|v| v.code.as_str()).collect(),
                )
            })
            .collect();
        for v in &self.variants {
            let sku_ok =
                (1..=64).contains(&v.sku.len()) && v.sku.bytes().all(|b| b.is_ascii_graphic());
            if !sku_ok {
                return Err(invalid(
                    "invalid_sku",
                    "SKU must be 1-64 visible ASCII characters",
                ));
            }
            if v.ean.as_deref().is_some_and(|e| !ean_valid(e)) {
                return Err(invalid(
                    "invalid_ean",
                    format!("EAN of {} has an invalid check digit or length", v.sku),
                ));
            }
            if v.weight_g.is_some_and(|w| !(0..=10_000_000).contains(&w)) {
                return Err(invalid(VAR, "weight_g must be 0-10000000"));
            }
            let matches = v.option_values.len() == options.len()
                && v.option_values.iter().all(|(opt, val)| {
                    options
                        .get(opt.as_str())
                        .is_some_and(|vals| vals.contains(val.as_str()))
                });
            if !matches {
                return Err(invalid(
                    VAR,
                    format!(
                        "variant {} must have exactly one existing value per option",
                        v.sku
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// Fits `numeric(12, 4)` exactly: positive, at most 1e6, at most 4 decimal places.
fn unit_quantity_valid(q: f64) -> bool {
    let scaled = q * 10_000.0;
    q.is_finite() && (0.0001..=1_000_000.0).contains(&q) && (scaled - scaled.round()).abs() < 1e-6
}

fn all_unique<T: Ord>(items: impl Iterator<Item = T>) -> bool {
    let mut seen = BTreeSet::new();
    items.into_iter().all(|i| seen.insert(i))
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

// ---------------------------------------------------------------------------------------
// Persistence.

/// Creates a product; audit `product.created`, event `product.created`.
pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &ProductInput,
) -> Result<Product, Error> {
    input.validate()?;
    let tenant_id = tx.tenant_id();
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO products (id, tenant_id) VALUES ($1, $2)",
        id,
        tenant_id
    )
    .execute(&mut **tx)
    .await?;
    save(tx, id, input).await?;
    let product = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "product.created",
        "product",
        Some(&id.to_string()),
        &json!({ "after": product }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "product.created", &json!({ "product_id": id })).await?;
    Ok(product)
}

/// Replaces the whole document of an existing product.
pub async fn replace(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &ProductInput,
) -> Result<Product, Error> {
    input.validate()?;
    // Locks the row: concurrent replaces of one product run one after the other.
    let before = get_locked(tx, id).await?;
    save(tx, id, input).await?;
    // Category membership decides which sales apply (A18 intervals).
    crate::pricing::refresh_product(tx, id).await?;
    let product = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "product.updated",
        "product",
        Some(&id.to_string()),
        &json!({ "before": before, "after": product }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "product.updated", &json!({ "product_id": id })).await?;
    Ok(product)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get_locked(tx, id).await?;
    sqlx::query!("DELETE FROM products WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "product.deleted",
        "product",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "product.deleted", &json!({ "product_id": id })).await?;
    Ok(())
}

async fn get_locked(tx: &mut TenantTx, id: Uuid) -> Result<Product, Error> {
    sqlx::query_scalar!("SELECT id FROM products WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    get(tx, id).await
}

async fn save(tx: &mut TenantTx, id: Uuid, p: &ProductInput) -> Result<(), Error> {
    let tenant_id = tx.tenant_id();
    check_references(tx, p).await?;

    let gpsr = serde_json::to_value(&p.gpsr).map_err(internal)?;
    sqlx::query!(
        "UPDATE products SET status = $2, brand = $3, gpsr = $4, unit_measure = $5,
                unit_quantity = $6::float8, heureka_category = $7, google_category = $8,
                updated_at = now()
         WHERE id = $1",
        id,
        p.status.as_str(),
        p.brand.as_deref().map(str::trim),
        gpsr,
        p.unit_measure.map(UnitMeasure::as_str),
        p.unit_quantity,
        p.heureka_category.as_deref(),
        p.google_category.as_deref(),
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query!("DELETE FROM product_translations WHERE product_id = $1", id)
        .execute(&mut **tx)
        .await?;
    for t in &p.translations {
        sqlx::query!(
            "INSERT INTO product_translations (tenant_id, product_id, locale, name, slug,
                 description_html, short_description, seo_title, seo_description)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            tenant_id,
            id,
            t.locale,
            t.name.trim(),
            t.slug,
            sanitize_html(&t.description_html),
            t.short_description,
            t.seo_title,
            t.seo_description,
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }

    sqlx::query!("DELETE FROM product_options WHERE product_id = $1", id)
        .execute(&mut **tx)
        .await?;
    for (pos, o) in p.options.iter().enumerate() {
        sqlx::query!(
            r#"INSERT INTO product_options (tenant_id, product_id, code, position, name_i18n, "values")
               VALUES ($1, $2, $3, $4, $5, $6)"#,
            tenant_id,
            id,
            o.code,
            position(pos),
            serde_json::to_value(&o.name_i18n).map_err(internal)?,
            serde_json::to_value(&o.values).map_err(internal)?,
        )
        .execute(&mut **tx)
        .await?;
    }

    let variant_ids = save_variants(tx, id, &p.variants).await?;

    // Categories: keep the product's position in categories it stays in, append to new ones.
    sqlx::query!(
        "DELETE FROM product_categories WHERE product_id = $1 AND NOT (category_id = ANY($2))",
        id,
        &p.category_ids
    )
    .execute(&mut **tx)
    .await?;
    for category_id in &p.category_ids {
        sqlx::query!(
            "INSERT INTO product_categories (tenant_id, product_id, category_id, position)
             SELECT $1, $2, $3, coalesce(max(position) + 1, 0)
             FROM product_categories WHERE category_id = $3
             ON CONFLICT DO NOTHING",
            tenant_id,
            id,
            category_id
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }

    sqlx::query!("DELETE FROM product_media WHERE product_id = $1", id)
        .execute(&mut **tx)
        .await?;
    for (pos, m) in p.media.iter().enumerate() {
        let variant_id = m.variant_sku.as_ref().and_then(|s| variant_ids.get(s));
        sqlx::query!(
            "INSERT INTO product_media (tenant_id, product_id, variant_id, asset_id, position, alt_i18n)
             VALUES ($1, $2, $3, $4, $5, $6)",
            tenant_id,
            id,
            variant_id,
            m.asset_id,
            position(pos),
            serde_json::to_value(&m.alt_i18n).map_err(internal)?,
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }

    sqlx::query!(
        "DELETE FROM product_parameter_values WHERE product_id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    for (pos, v) in p.parameters.iter().enumerate() {
        let variant_id = v.variant_sku.as_ref().and_then(|s| variant_ids.get(s));
        sqlx::query!(
            "INSERT INTO product_parameter_values
                 (tenant_id, product_id, variant_id, parameter_id, value, position)
             VALUES ($1, $2, $3, $4, $5, $6)",
            tenant_id,
            id,
            variant_id,
            v.parameter_id,
            v.value,
            position(pos),
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }

    sqlx::query!(
        "DELETE FROM product_tax_categories WHERE product_id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    for (country, code) in &p.tax_categories {
        sqlx::query!(
            "INSERT INTO product_tax_categories (tenant_id, product_id, country, code)
             VALUES ($1, $2, $3, $4)",
            tenant_id,
            id,
            country,
            code
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Upserts variants by id and deletes the ones not listed; returns SKU -> id.
async fn save_variants(
    tx: &mut TenantTx,
    product_id: Uuid,
    variants: &[VariantInput],
) -> Result<HashMap<String, Uuid>, Error> {
    let tenant_id = tx.tenant_id();
    let existing: BTreeSet<Uuid> =
        sqlx::query_scalar!("SELECT id FROM variants WHERE product_id = $1", product_id)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .collect();
    if let Some(v) = variants
        .iter()
        .find(|v| v.id.is_some_and(|id| !existing.contains(&id)))
    {
        return Err(invalid(
            "unknown_variant",
            format!("variant {} does not belong to this product", v.sku),
        ));
    }
    let keep: Vec<Uuid> = variants.iter().filter_map(|v| v.id).collect();
    sqlx::query!(
        "DELETE FROM variants WHERE product_id = $1 AND NOT (id = ANY($2))",
        product_id,
        &keep
    )
    .execute(&mut **tx)
    .await?;
    // Swapping SKUs or option combinations between variants passes through duplicates.
    sqlx::query!("SET CONSTRAINTS variants_sku_unique, variants_options_unique DEFERRED")
        .execute(&mut **tx)
        .await?;
    sqlx::query!(
        "UPDATE variants SET is_default = false WHERE product_id = $1 AND is_default",
        product_id
    )
    .execute(&mut **tx)
    .await?;

    let default_idx = variants.iter().position(|v| v.is_default).unwrap_or(0);
    let mut ids = HashMap::new();
    for (pos, v) in variants.iter().enumerate() {
        let option_values = serde_json::to_value(&v.option_values).map_err(internal)?;
        let id = v.id.unwrap_or_else(crate::id::new_id);
        sqlx::query!(
            "INSERT INTO variants (id, tenant_id, product_id, sku, ean, option_values, weight_g,
                                   position, is_default)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (id) DO UPDATE SET sku = $4, ean = $5, option_values = $6,
                 weight_g = $7, position = $8, is_default = $9, updated_at = now()",
            id,
            tenant_id,
            product_id,
            v.sku,
            v.ean,
            option_values,
            v.weight_g,
            position(pos),
            pos == default_idx,
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        ids.insert(v.sku.clone(), id);
    }
    sqlx::query!("SET CONSTRAINTS variants_sku_unique, variants_options_unique IMMEDIATE")
        .execute(&mut **tx)
        .await
        .map_err(|e| match db_error(e) {
            Error::Conflict {
                code: "already_exists",
                ..
            } => Error::Conflict {
                code: "variant_options_taken",
                detail: "two variants have the same option values".into(),
            },
            other => other,
        })?;
    Ok(ids)
}

/// References the database constraints cannot express: parameter kinds and statutory tax
/// categories. Categories and assets are checked by their composite foreign keys.
async fn check_references(tx: &mut TenantTx, p: &ProductInput) -> Result<(), Error> {
    let ids: Vec<Uuid> = p.parameters.iter().map(|v| v.parameter_id).collect();
    let kinds: HashMap<Uuid, String> =
        // FOR SHARE: a concurrent kind change waits until these values are committed.
        sqlx::query!("SELECT id, kind FROM parameters WHERE id = ANY($1) ORDER BY id FOR SHARE", &ids)
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| (r.id, r.kind))
            .collect();
    for v in &p.parameters {
        let kind = kinds.get(&v.parameter_id).ok_or_else(|| {
            invalid(
                "unknown_reference",
                format!("parameter {} does not exist", v.parameter_id),
            )
        })?;
        super::parameters::check_value(kind, &v.value)?;
    }
    let today = Utc::now().date_naive();
    for (country, code) in &p.tax_categories {
        if tax::rate(tx, country, code, today).await?.is_none() {
            return Err(invalid(
                "invalid_tax_category",
                format!("{country} has no {code} tax category"),
            ));
        }
    }
    Ok(())
}

fn position(i: usize) -> i32 {
    i32::try_from(i).unwrap_or(i32::MAX)
}

/// The document, read consistently: the row lock waits for (and then blocks) a concurrent
/// `replace`, so the separate child queries below cannot mix two versions.
pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Product, Error> {
    let row = sqlx::query!(
        r#"SELECT id, status, brand, gpsr, unit_measure, unit_quantity::float8 AS unit_quantity,
                  heureka_category, google_category, created_at, updated_at
           FROM products WHERE id = $1 FOR SHARE"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;

    let translations = sqlx::query_as!(
        ProductTranslation,
        "SELECT locale, name, slug, description_html, short_description, seo_title, seo_description
         FROM product_translations WHERE product_id = $1 ORDER BY locale",
        id
    )
    .fetch_all(&mut **tx)
    .await?;

    let options = sqlx::query!(
        r#"SELECT code, name_i18n, "values" AS vals FROM product_options
           WHERE product_id = $1 ORDER BY position"#,
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(ProductOption {
            code: r.code,
            name_i18n: serde_json::from_value(r.name_i18n).map_err(internal)?,
            values: serde_json::from_value(r.vals).map_err(internal)?,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;

    let variants = sqlx::query!(
        "SELECT id, sku, ean, option_values, weight_g, is_default FROM variants
         WHERE product_id = $1 ORDER BY position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(Variant {
            id: r.id,
            sku: r.sku,
            ean: r.ean,
            option_values: serde_json::from_value(r.option_values).map_err(internal)?,
            weight_g: r.weight_g,
            is_default: r.is_default,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;

    let category_ids = sqlx::query_scalar!(
        "SELECT category_id FROM product_categories WHERE product_id = $1 ORDER BY category_id",
        id
    )
    .fetch_all(&mut **tx)
    .await?;

    let media = sqlx::query!(
        "SELECT m.asset_id, v.sku AS \"variant_sku?\", m.alt_i18n
         FROM product_media m LEFT JOIN variants v ON v.id = m.variant_id
         WHERE m.product_id = $1 ORDER BY m.position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(ProductMedia {
            asset_id: r.asset_id,
            variant_sku: r.variant_sku,
            alt_i18n: serde_json::from_value(r.alt_i18n).map_err(internal)?,
        })
    })
    .collect::<Result<Vec<_>, Error>>()?;

    let parameters = sqlx::query!(
        "SELECT pv.parameter_id, v.sku AS \"variant_sku?\", pv.value
         FROM product_parameter_values pv LEFT JOIN variants v ON v.id = pv.variant_id
         WHERE pv.product_id = $1 ORDER BY pv.position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| ParameterValue {
        parameter_id: r.parameter_id,
        variant_sku: r.variant_sku,
        value: r.value,
    })
    .collect();

    let tax_categories = sqlx::query!(
        "SELECT country, code FROM product_tax_categories WHERE product_id = $1",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| (r.country, r.code))
    .collect();

    Ok(Product {
        id: row.id,
        status: ProductStatus::parse(&row.status),
        brand: row.brand,
        gpsr: serde_json::from_value(row.gpsr).map_err(internal)?,
        unit_measure: row.unit_measure.as_deref().and_then(UnitMeasure::parse),
        unit_quantity: row.unit_quantity,
        heureka_category: row.heureka_category,
        google_category: row.google_category,
        translations,
        options,
        variants,
        category_ids,
        media,
        parameters,
        tax_categories,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// A row of the product list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProductSummary {
    pub id: Uuid,
    pub status: ProductStatus,
    pub brand: Option<String>,
    /// Locale -> name.
    pub name: I18n,
    pub default_sku: Option<String>,
    pub variant_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProductPage {
    pub items: Vec<ProductSummary>,
    /// Pass as `cursor` for the next page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct ProductFilter {
    pub status: Option<ProductStatus>,
    pub category_id: Option<Uuid>,
    /// Case-insensitive substring of a name (any locale) or a SKU.
    pub q: Option<String>,
}

pub const MAX_PAGE: i64 = 100;

/// Newest first; UUIDv7 ids make the last id the cursor.
/// `q` matches a name (any locale) or SKU substring through `platform.search_product_ids`,
/// which can use the pg_trgm indexes (RLS would block them for non-leakproof `ILIKE`).
pub async fn list(
    tx: &mut TenantTx,
    filter: &ProductFilter,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<ProductPage, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_PAGE}"),
        ));
    }
    let pattern = match filter.q.as_deref().map(str::trim) {
        Some(q) if q.chars().count() > 100 => {
            return Err(invalid("invalid_query", "q is at most 100 characters"));
        }
        Some(q) if !q.is_empty() => Some(format!("%{}%", like_escape(q))),
        _ => None,
    };
    let rows = sqlx::query!(
        r#"SELECT p.id, p.status, p.brand, p.created_at, p.updated_at,
                  coalesce((SELECT jsonb_object_agg(t.locale, t.name) FROM product_translations t
                            WHERE t.product_id = p.id), '{}') AS "name!",
                  (SELECT v.sku FROM variants v WHERE v.product_id = p.id AND v.is_default) AS default_sku,
                  (SELECT count(*) FROM variants v WHERE v.product_id = p.id) AS "variant_count!"
           FROM products p
           WHERE ($1::uuid IS NULL OR p.id < $1)
             AND ($2::text IS NULL OR p.status = $2)
             AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM product_categories c
                                              WHERE c.product_id = p.id AND c.category_id = $3))
             AND ($4::text IS NULL OR p.id IN (SELECT platform.search_product_ids($4)))
           ORDER BY p.id DESC
           LIMIT $5"#,
        cursor,
        filter.status.map(ProductStatus::as_str),
        filter.category_id,
        pattern,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(|r| {
            Ok(ProductSummary {
                id: r.id,
                status: ProductStatus::parse(&r.status),
                brand: r.brand,
                name: serde_json::from_value(r.name).map_err(internal)?,
                default_sku: r.default_sku,
                variant_count: r.variant_count,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        items.last().map(|p| p.id)
    } else {
        None
    };
    Ok(ProductPage { items, next_cursor })
}

/// Escapes `LIKE` wildcards so user input matches literally.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i18n(pairs: &[(&str, &str)]) -> I18n {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn variant(sku: &str, color: &str, size: &str) -> VariantInput {
        VariantInput {
            id: None,
            sku: sku.into(),
            ean: None,
            option_values: [("color", color), ("size", size)]
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            weight_g: Some(200),
            is_default: false,
        }
    }

    fn option(code: &str, values: &[&str]) -> ProductOption {
        ProductOption {
            code: code.into(),
            name_i18n: i18n(&[("cs", code)]),
            values: values
                .iter()
                .map(|v| OptionValue {
                    code: (*v).into(),
                    name_i18n: i18n(&[("cs", v)]),
                })
                .collect(),
        }
    }

    pub(crate) fn tshirt() -> ProductInput {
        ProductInput {
            status: ProductStatus::Active,
            brand: Some("Basic".into()),
            gpsr: Gpsr::default(),
            unit_measure: None,
            unit_quantity: None,
            heureka_category: None,
            google_category: None,
            translations: vec![ProductTranslation {
                locale: "cs".into(),
                name: "Tričko".into(),
                slug: "tricko".into(),
                description_html: "<p>Bavlna</p>".into(),
                short_description: String::new(),
                seo_title: None,
                seo_description: None,
            }],
            options: vec![
                option("color", &["red", "blue"]),
                option("size", &["s", "m"]),
            ],
            variants: vec![
                variant("TS-R-S", "red", "s"),
                variant("TS-R-M", "red", "m"),
                variant("TS-B-S", "blue", "s"),
                variant("TS-B-M", "blue", "m"),
            ],
            category_ids: vec![],
            media: vec![],
            parameters: vec![],
            tax_categories: BTreeMap::new(),
        }
    }

    fn code_of(p: &ProductInput) -> &'static str {
        p.validate().unwrap_err().code()
    }

    #[test]
    fn accepts_a_valid_document() {
        tshirt().validate().unwrap();
    }

    #[test]
    fn rejects_inconsistent_variants() {
        let mut p = tshirt();
        p.variants[1] = variant("TS-R-S2", "red", "s");
        assert_eq!(code_of(&p), "invalid_variants", "duplicate combination");

        let mut p = tshirt();
        p.variants[0].option_values.remove("size");
        assert_eq!(code_of(&p), "invalid_variants", "missing option");

        let mut p = tshirt();
        p.variants[0]
            .option_values
            .insert("color".into(), "green".into());
        assert_eq!(code_of(&p), "invalid_variants", "unknown value");

        let mut p = tshirt();
        p.variants[1].sku = "TS-R-S".into();
        assert_eq!(code_of(&p), "invalid_variants", "duplicate sku");

        let mut p = tshirt();
        p.variants[0].is_default = true;
        p.variants[1].is_default = true;
        assert_eq!(code_of(&p), "invalid_variants", "two defaults");

        let mut p = tshirt();
        p.variants.clear();
        assert_eq!(code_of(&p), "invalid_variants", "active without variants");
        p.status = ProductStatus::Draft;
        p.validate().unwrap();
    }

    #[test]
    fn rejects_bad_fields() {
        let mut p = tshirt();
        p.variants[0].ean = Some("4006381333932".into());
        assert_eq!(code_of(&p), "invalid_ean");

        let mut p = tshirt();
        p.variants[0].sku = "has space".into();
        assert_eq!(code_of(&p), "invalid_sku");

        let mut p = tshirt();
        p.translations[0].slug = "Tričko".into();
        assert_eq!(code_of(&p), "invalid_slug");

        let mut p = tshirt();
        p.translations.push(p.translations[0].clone());
        assert_eq!(code_of(&p), "invalid_translation");

        let mut p = tshirt();
        p.translations.clear();
        assert_eq!(code_of(&p), "invalid_translation");

        let mut p = tshirt();
        p.unit_measure = Some(UnitMeasure::Kg);
        assert_eq!(code_of(&p), "invalid_unit");
        p.unit_quantity = Some(0.5);
        p.validate().unwrap();
        p.unit_quantity = Some(f64::NAN);
        assert_eq!(code_of(&p), "invalid_unit");
        for bad in [0.00001, 0.12345, 0.0, -1.0, 1_000_000.5] {
            p.unit_quantity = Some(bad);
            assert_eq!(code_of(&p), "invalid_unit", "{bad}");
        }
        for ok in [0.0001, 0.75, 1.5, 0.3333, 1_000_000.0] {
            p.unit_quantity = Some(ok);
            p.validate().unwrap();
        }

        let mut p = tshirt();
        p.tax_categories.insert("cz".into(), "reduced".into());
        assert_eq!(code_of(&p), "invalid_tax_category");
        let mut p = tshirt();
        p.tax_categories.insert("CZ".into(), "luxury".into());
        assert_eq!(code_of(&p), "invalid_tax_category");

        let mut p = tshirt();
        p.media.push(ProductMedia {
            asset_id: Uuid::nil(),
            variant_sku: Some("NOPE".into()),
            alt_i18n: I18n::new(),
        });
        assert_eq!(code_of(&p), "invalid_media");
    }

    #[test]
    fn gpsr_party_needs_an_electronic_contact() {
        let mut p = tshirt();
        p.gpsr.manufacturer = Some(GpsrParty {
            name: "Výrobce s.r.o.".into(),
            address: "Praha 1".into(),
            email: None,
            url: None,
            phone: None,
        });
        assert_eq!(code_of(&p), "invalid_gpsr");
        if let Some(m) = p.gpsr.manufacturer.as_mut() {
            m.email = Some("info@vyrobce.cz".into());
        }
        p.validate().unwrap();
        let unknown = serde_json::from_value::<Gpsr>(json!({ "manufacturer": null, "x": 1 }));
        assert!(unknown.is_err());
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(like_escape(r"50%_a\b"), r"50\%\_a\\b");
    }
}
