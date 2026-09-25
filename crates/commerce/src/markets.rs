//! Markets (spec §5.1): a country group with currency and locales, owned by one tenant.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit, unique_violation};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Market {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub country_codes: Vec<String>,
    pub currency: String,
    pub default_locale: String,
    pub locales: Vec<String>,
    pub tax_mode: TaxMode,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

/// Whether prices are entered gross (B2C, default) or net.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaxMode {
    #[default]
    Gross,
    Net,
}

impl TaxMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gross => "gross",
            Self::Net => "net",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "net" { Self::Net } else { Self::Gross }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewMarket {
    /// Lowercase identifier, unique per tenant, e.g. `sk`.
    #[schema(example = "sk")]
    pub code: String,
    #[schema(example = "Slovensko")]
    pub name: String,
    /// ISO 3166-1 alpha-2 ship-to countries.
    #[schema(example = json!(["SK"]))]
    pub country_codes: Vec<String>,
    /// ISO 4217 code.
    #[schema(example = "EUR")]
    pub currency: String,
    #[schema(example = "sk")]
    pub default_locale: String,
    #[schema(example = json!(["sk", "en"]))]
    pub locales: Vec<String>,
    #[serde(default)]
    pub tax_mode: TaxMode,
    /// Makes this the tenant's default market (the previous default loses the flag).
    #[serde(default)]
    pub is_default: bool,
}

pub(crate) fn invalid(code: &'static str, detail: impl Into<String>) -> Error {
    Error::Validation {
        code,
        detail: detail.into(),
    }
}

pub(crate) fn all_bytes(
    s: &str,
    len: std::ops::RangeInclusive<usize>,
    ok: impl Fn(u8) -> bool,
) -> bool {
    len.contains(&s.len()) && s.bytes().all(ok)
}

pub(crate) fn is_locale(s: &str) -> bool {
    match s.split_once('-') {
        None => all_bytes(s, 2..=2, |b| b.is_ascii_lowercase()),
        Some((lang, region)) => {
            all_bytes(lang, 2..=2, |b| b.is_ascii_lowercase())
                && all_bytes(region, 2..=2, |b| b.is_ascii_uppercase())
        }
    }
}

impl NewMarket {
    pub fn validate(&self) -> Result<(), Error> {
        let code_ok = all_bytes(&self.code, 1..=32, |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
        }) && !self.code.starts_with('-');
        if !code_ok {
            return Err(invalid(
                "invalid_code",
                "code must be 1-32 lowercase letters, digits or hyphens",
            ));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 200 {
            return Err(invalid("invalid_name", "name must be 1-200 characters"));
        }
        if self.country_codes.is_empty()
            || self.country_codes.len() > 50
            || !self
                .country_codes
                .iter()
                .all(|c| all_bytes(c, 2..=2, |b| b.is_ascii_uppercase()))
        {
            return Err(invalid(
                "invalid_country_codes",
                "country_codes must be 1-50 ISO 3166-1 alpha-2 codes",
            ));
        }
        if !all_bytes(&self.currency, 3..=3, |b| b.is_ascii_uppercase()) {
            return Err(invalid(
                "invalid_currency",
                "currency must be an ISO 4217 code",
            ));
        }
        if self.locales.is_empty()
            || self.locales.len() > 20
            || !self.locales.iter().all(|l| is_locale(l))
        {
            return Err(invalid(
                "invalid_locales",
                "locales must be 1-20 tags like `cs` or `cs-CZ`",
            ));
        }
        if !self.locales.contains(&self.default_locale) {
            return Err(invalid(
                "invalid_default_locale",
                "default_locale must be one of locales",
            ));
        }
        Ok(())
    }
}

pub async fn list(tx: &mut TenantTx) -> Result<Vec<Market>, Error> {
    let rows = sqlx::query!(
        "SELECT id, code, name, country_codes, currency, default_locale, locales, tax_mode,
                is_default, created_at
         FROM markets ORDER BY is_default DESC, code"
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Market {
            id: r.id,
            code: r.code,
            name: r.name,
            country_codes: r.country_codes,
            currency: r.currency,
            default_locale: r.default_locale,
            locales: r.locales,
            tax_mode: TaxMode::parse(&r.tax_mode),
            is_default: r.is_default,
            created_at: r.created_at,
        })
        .collect())
}

/// Creates a market, records it in the audit log and publishes `market.created`.
pub async fn create(tx: &mut TenantTx, actor: &str, m: &NewMarket) -> Result<Market, Error> {
    m.validate()?;
    let tenant_id = tx.tenant_id();
    if m.is_default {
        sqlx::query!("UPDATE markets SET is_default = false WHERE is_default")
            .execute(&mut **tx)
            .await?;
    }
    let row = sqlx::query!(
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale,
                              locales, tax_mode, is_default)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, created_at",
        tenant_id,
        m.code,
        m.name.trim(),
        &m.country_codes,
        m.currency,
        m.default_locale,
        &m.locales,
        m.tax_mode.as_str(),
        m.is_default
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| {
        if unique_violation(&e) {
            Error::Conflict {
                code: "already_exists",
                detail: format!("market {} already exists", m.code),
            }
        } else {
            e.into()
        }
    })?;
    let market = Market {
        id: row.id,
        code: m.code.clone(),
        name: m.name.trim().to_owned(),
        country_codes: m.country_codes.clone(),
        currency: m.currency.clone(),
        default_locale: m.default_locale.clone(),
        locales: m.locales.clone(),
        tax_mode: m.tax_mode,
        is_default: m.is_default,
        created_at: row.created_at,
    };
    audit::record(
        tx,
        actor,
        "market.created",
        "market",
        Some(&market.id.to_string()),
        &json!({ "after": market }),
    )
    .await?;
    platform::queue::publish(
        &mut **tx,
        "market.created",
        &json!({ "market_id": market.id, "code": market.code }),
    )
    .await?;
    Ok(market)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk() -> NewMarket {
        NewMarket {
            code: "sk".into(),
            name: "Slovensko".into(),
            country_codes: vec!["SK".into()],
            currency: "EUR".into(),
            default_locale: "sk".into(),
            locales: vec!["sk".into(), "en-GB".into()],
            tax_mode: TaxMode::Gross,
            is_default: false,
        }
    }

    fn code_of(m: NewMarket) -> &'static str {
        m.validate().unwrap_err().code()
    }

    #[test]
    fn accepts_valid_market() {
        assert!(sk().validate().is_ok());
    }

    #[test]
    fn rejects_invalid_fields() {
        assert_eq!(
            code_of(NewMarket {
                code: "SK".into(),
                ..sk()
            }),
            "invalid_code"
        );
        assert_eq!(
            code_of(NewMarket {
                name: " ".into(),
                ..sk()
            }),
            "invalid_name"
        );
        assert_eq!(
            code_of(NewMarket {
                country_codes: vec![],
                ..sk()
            }),
            "invalid_country_codes"
        );
        assert_eq!(
            code_of(NewMarket {
                country_codes: vec!["svk".into()],
                ..sk()
            }),
            "invalid_country_codes"
        );
        assert_eq!(
            code_of(NewMarket {
                currency: "eur".into(),
                ..sk()
            }),
            "invalid_currency"
        );
        assert_eq!(
            code_of(NewMarket {
                locales: vec!["sk_SK".into()],
                ..sk()
            }),
            "invalid_locales"
        );
        assert_eq!(
            code_of(NewMarket {
                default_locale: "cs".into(),
                ..sk()
            }),
            "invalid_default_locale"
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = serde_json::from_value::<NewMarket>(serde_json::json!({
            "code": "sk", "name": "SK", "country_codes": ["SK"], "currency": "EUR",
            "default_locale": "sk", "locales": ["sk"], "tenant_id": "x"
        }));
        assert!(err.is_err());
    }
}
