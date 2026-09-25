//! Per-country tax categories (A3). Statutory rates are platform reference data
//! (`platform.tax_categories`, seeded for the EU-27); a product maps to one category per
//! country and uses `standard` where it has no mapping.

use chrono::NaiveDate;
use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::markets::invalid;

pub const STANDARD: &str = "standard";
pub const CODES: &[&str] = &[STANDARD, "reduced", "second_reduced", "super_reduced"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TaxCategory {
    #[schema(example = "CZ")]
    pub country: String,
    #[schema(example = "reduced")]
    pub code: String,
    /// Percent as a decimal string, e.g. `"12"` or `"13.5"`.
    #[schema(example = "12")]
    pub rate: String,
    pub valid_from: NaiveDate,
}

/// The categories in force on `at`, optionally for one country.
pub async fn list(
    tx: &mut TenantTx,
    country: Option<&str>,
    at: NaiveDate,
) -> Result<Vec<TaxCategory>, Error> {
    if country.is_some_and(|c| c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase())) {
        return Err(invalid(
            "invalid_country",
            "country must be an ISO 3166-1 alpha-2 code",
        ));
    }
    Ok(sqlx::query_as!(
        TaxCategory,
        r#"SELECT DISTINCT ON (country, code) country, code, trim_scale(rate)::text AS "rate!", valid_from
           FROM platform.tax_categories
           WHERE valid_from <= $1 AND ($2::text IS NULL OR country = $2)
           ORDER BY country, code, valid_from DESC"#,
        at,
        country
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// The rate of `(country, code)` in force on `at`.
pub async fn rate(
    tx: &mut TenantTx,
    country: &str,
    code: &str,
    at: NaiveDate,
) -> Result<Option<TaxCategory>, Error> {
    Ok(sqlx::query_as!(
        TaxCategory,
        r#"SELECT country, code, trim_scale(rate)::text AS "rate!", valid_from
           FROM platform.tax_categories
           WHERE country = $1 AND code = $2 AND valid_from <= $3
           ORDER BY valid_from DESC LIMIT 1"#,
        country,
        code,
        at
    )
    .fetch_optional(&mut **tx)
    .await?)
}

/// The tax category that applies to `product_id` shipped to `country` on `at`: its mapping
/// for that country, else `standard`. `None` for a country without seeded rates.
pub async fn product_rate(
    tx: &mut TenantTx,
    product_id: Uuid,
    country: &str,
    at: NaiveDate,
) -> Result<Option<TaxCategory>, Error> {
    let code = sqlx::query_scalar!(
        "SELECT code FROM product_tax_categories WHERE product_id = $1 AND country = $2",
        product_id,
        country
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or_else(|| STANDARD.to_owned());
    rate(tx, country, &code, at).await
}
