//! VAT (spec §10.1, A3): the tenant tax profile, which country's rate applies to a sale, and
//! VAT extraction from gross prices. Statutory rates and the product -> category mapping live
//! in [`crate::catalog::tax`].
//!
//! The configuration here must be confirmed by the merchant's accountant (A3): the platform
//! applies the rules below, it does not decide the merchant's registration status.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::catalog::tax as categories;
use crate::markets::invalid;
use crate::money::div_round_half_up;

/// EU member states (ISO 3166-1 alpha-2; Greece is `GR`, its VAT prefix `EL` is not a country).
pub const EU_COUNTRIES: [&str; 27] = [
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
];

pub fn is_eu(country: &str) -> bool {
    EU_COUNTRIES.contains(&country)
}

/// A VAT rate in hundredths of a percent: `TaxRate(2100)` = 21 %, `TaxRate(1350)` = 13.5 %.
/// Statutory rates are `numeric(5,2)`, so this is exact. Serialized as a decimal string
/// (`"21"`, `"13.5"`), like the tax category API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TaxRate(pub u32);

impl TaxRate {
    pub const ZERO: Self = Self(0);

    /// From the database's `(rate * 100)::int`.
    pub fn from_hundredths(v: i32) -> Result<Self, Error> {
        u32::try_from(v)
            .ok()
            .filter(|v| *v < 10_000)
            .map(Self)
            .ok_or_else(|| Error::Internal(format!("tax rate out of range: {v}")))
    }

    pub fn hundredths(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TaxRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (int, frac) = (self.0 / 100, self.0 % 100);
        match frac {
            0 => write!(f, "{int}"),
            f10 if f10 % 10 == 0 => write!(f, "{int}.{}", f10 / 10),
            _ => write!(f, "{int}.{frac:02}"),
        }
    }
}

impl FromStr for TaxRate {
    type Err = ();

    /// `"21"`, `"13.5"`, `"2.10"`: 0 <= rate < 100 with at most two decimals.
    fn from_str(s: &str) -> Result<Self, ()> {
        let (int, frac) = match s.split_once('.') {
            Some((_, "")) => return Err(()),
            Some(parts) => parts,
            None => (s, ""),
        };
        let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
        if int.is_empty() || int.len() > 2 || frac.len() > 2 || !digits(int) || !digits(frac) {
            return Err(());
        }
        let int: u32 = int.parse().map_err(|_| ())?;
        let frac: u32 = format!("{frac:0<2}").parse().map_err(|_| ())?;
        Ok(Self(int * 100 + frac))
    }
}

impl Serialize for TaxRate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TaxRate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(|()| {
            serde::de::Error::custom("rate must be a percent like \"21\" or \"13.5\"")
        })
    }
}

impl utoipa::PartialSchema for TaxRate {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .description(Some("VAT rate in percent as a decimal string"))
            .examples([json!("21")])
            .into()
    }
}

impl ToSchema for TaxRate {}

/// VAT contained in a gross amount: `round_half_up(gross × r / (100 + r))` (spec §10.1).
pub fn vat_from_gross(gross_minor: i64, rate: TaxRate) -> i64 {
    let bp = i128::from(rate.0);
    let vat = div_round_half_up(i128::from(gross_minor) * bp, 10_000 + bp);
    // |vat| < |gross| for any rate below 100 %, so this always fits.
    i64::try_from(vat).unwrap_or(gross_minor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DistanceSalesMode {
    /// EU B2C distance sales under the EUR 10 000 threshold: the establishment country's VAT
    /// applies everywhere. Needs the merchant's confirmation of eligibility.
    OriginThreshold,
    /// OSS (or local registration): the destination country's VAT applies.
    Destination,
}

impl DistanceSalesMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::OriginThreshold => "origin_threshold",
            Self::Destination => "destination",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "origin_threshold" {
            Self::OriginThreshold
        } else {
            Self::Destination
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TaxProfile {
    #[schema(example = "CZ")]
    pub establishment_country: String,
    pub vat_payer: bool,
    /// DIČ.
    #[schema(example = "CZ12345678")]
    pub vat_id: Option<String>,
    /// SK IČ DPH (Slovak establishments only).
    pub sk_ic_dph: Option<String>,
    pub distance_sales_mode: DistanceSalesMode,
    /// When the merchant confirmed eligibility for the origin-country threshold regime.
    pub origin_threshold_confirmed_at: Option<DateTime<Utc>>,
    /// Whether the cash rounding line is taxed (A16). Default false: outside the VAT base.
    /// Confirm with an accountant before enabling.
    pub cash_rounding_in_vat_base: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaxProfileInput {
    #[schema(example = "CZ")]
    pub establishment_country: String,
    pub vat_payer: bool,
    pub vat_id: Option<String>,
    pub sk_ic_dph: Option<String>,
    pub distance_sales_mode: DistanceSalesMode,
    /// Required (true) when switching to `origin_threshold`: the merchant confirms their EU
    /// distance sales stay under EUR 10 000 per year. The time of confirmation is stored.
    #[serde(default)]
    pub confirm_origin_threshold: bool,
    #[serde(default)]
    pub cash_rounding_in_vat_base: bool,
}

fn matches(s: &str, prefix: &str, digits: std::ops::RangeInclusive<usize>) -> bool {
    s.strip_prefix(prefix).is_some_and(|rest| {
        digits.contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit())
    })
}

impl TaxProfileInput {
    /// Pure validation; `previous` decides whether the threshold confirmation is still needed.
    pub fn validate(&self, previous: Option<&TaxProfile>) -> Result<(), Error> {
        let country = self.establishment_country.as_str();
        if !is_eu(country) {
            return Err(invalid(
                "invalid_establishment_country",
                "establishment_country must be an EU member state (ISO 3166-1 alpha-2)",
            ));
        }
        match (country, self.vat_id.as_deref()) {
            (_, None) => {}
            ("CZ", Some(id)) if matches(id, "CZ", 8..=10) => {}
            ("CZ", Some(_)) => {
                return Err(invalid("invalid_vat_id", "a Czech DIČ is CZ + 8-10 digits"));
            }
            ("SK", Some(id)) if matches(id, "", 10..=10) => {}
            ("SK", Some(_)) => {
                return Err(invalid("invalid_vat_id", "a Slovak DIČ is 10 digits"));
            }
            (_, Some(id))
                if (4..=20).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) => {}
            (_, Some(_)) => {
                return Err(invalid(
                    "invalid_vat_id",
                    "vat_id must be 4-20 uppercase letters or digits",
                ));
            }
        }
        match (country, self.sk_ic_dph.as_deref()) {
            (_, None) => {}
            ("SK", Some(id)) if matches(id, "SK", 10..=10) => {}
            ("SK", Some(_)) => {
                return Err(invalid("invalid_sk_ic_dph", "IČ DPH is SK + 10 digits"));
            }
            (_, Some(_)) => {
                return Err(invalid(
                    "invalid_sk_ic_dph",
                    "sk_ic_dph applies only to Slovak establishments",
                ));
            }
        }
        if self.vat_payer {
            let id_ok = if country == "SK" {
                self.sk_ic_dph.is_some()
            } else {
                self.vat_id.is_some()
            };
            if !id_ok {
                return Err(invalid(
                    "vat_id_required",
                    "a VAT payer needs its VAT identification number (SK: IČ DPH)",
                ));
            }
        }
        let confirmed_before = previous.is_some_and(|p| {
            p.distance_sales_mode == DistanceSalesMode::OriginThreshold
                && p.origin_threshold_confirmed_at.is_some()
        });
        if self.distance_sales_mode == DistanceSalesMode::OriginThreshold
            && !self.confirm_origin_threshold
            && !confirmed_before
        {
            return Err(invalid(
                "origin_threshold_confirmation_required",
                "confirm_origin_threshold must be true to use origin_threshold",
            ));
        }
        Ok(())
    }
}

/// The country whose VAT applies to goods shipped to `ship_to` (A3), or `None` when no VAT is
/// charged (non-VAT-payer). Non-EU destinations are not supported in M1 (exports need
/// zero-rating and customs data) and fail closed.
pub fn liable_country<'a>(
    profile: &'a TaxProfile,
    ship_to: &'a str,
) -> Result<Option<&'a str>, Error> {
    if !is_eu(ship_to) {
        return Err(invalid(
            "ship_to_not_supported",
            format!("shipping to {ship_to} is not supported (EU destinations only)"),
        ));
    }
    if !profile.vat_payer {
        return Ok(None);
    }
    if ship_to == profile.establishment_country {
        return Ok(Some(&profile.establishment_country));
    }
    Ok(Some(match profile.distance_sales_mode {
        DistanceSalesMode::OriginThreshold => &profile.establishment_country,
        DistanceSalesMode::Destination => ship_to,
    }))
}

/// A3: checkout may ship only to a country the market lists and the tax profile covers.
pub fn check_ship_to(
    profile: &TaxProfile,
    market_countries: &[String],
    ship_to: &str,
) -> Result<(), Error> {
    if !market_countries.iter().any(|c| c == ship_to) {
        return Err(invalid(
            "ship_to_not_allowed",
            format!("the market does not ship to {ship_to}"),
        ));
    }
    liable_country(profile, ship_to).map(|_| ())
}

/// The VAT applying to one product: the liable country, the product's category there and
/// the rate on the tax point date. `rate` is zero for a non-VAT-payer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Liability {
    pub country: Option<String>,
    pub category: Option<String>,
    pub rate: TaxRate,
}

// ---------------------------------------------------------------------------------------
// Database services

fn missing_profile() -> Error {
    Error::Conflict {
        code: "tax_profile_missing",
        detail: "the tenant has no tax profile yet".into(),
    }
}

pub async fn get(tx: &mut TenantTx) -> Result<Option<TaxProfile>, Error> {
    let row = sqlx::query!(
        "SELECT establishment_country, vat_payer, vat_id, sk_ic_dph, distance_sales_mode,
                origin_threshold_confirmed_at, cash_rounding_in_vat_base, updated_at
         FROM tax_profiles"
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| TaxProfile {
        establishment_country: r.establishment_country,
        vat_payer: r.vat_payer,
        vat_id: r.vat_id,
        sk_ic_dph: r.sk_ic_dph,
        distance_sales_mode: DistanceSalesMode::parse(&r.distance_sales_mode),
        origin_threshold_confirmed_at: r.origin_threshold_confirmed_at,
        cash_rounding_in_vat_base: r.cash_rounding_in_vat_base,
        updated_at: r.updated_at,
    }))
}

/// The profile, or `409 tax_profile_missing`.
pub async fn require(tx: &mut TenantTx) -> Result<TaxProfile, Error> {
    get(tx).await?.ok_or_else(missing_profile)
}

/// Creates or replaces the tax profile (audited, publishes `tax_profile.updated`).
pub async fn upsert(
    tx: &mut TenantTx,
    actor: &str,
    input: &TaxProfileInput,
) -> Result<TaxProfile, Error> {
    let tenant_id = tx.tenant_id();
    // Serialize concurrent updates so the confirmation logic sees the latest row.
    sqlx::query!("SELECT tenant_id FROM tax_profiles FOR UPDATE")
        .fetch_optional(&mut **tx)
        .await?;
    let before = get(tx).await?;
    input.validate(before.as_ref())?;
    let confirmed_at = match input.distance_sales_mode {
        DistanceSalesMode::Destination => None,
        DistanceSalesMode::OriginThreshold if input.confirm_origin_threshold => Some(Utc::now()),
        DistanceSalesMode::OriginThreshold => before
            .as_ref()
            .and_then(|p| p.origin_threshold_confirmed_at),
    };
    sqlx::query!(
        "INSERT INTO tax_profiles (tenant_id, establishment_country, vat_payer, vat_id, sk_ic_dph,
                                   distance_sales_mode, origin_threshold_confirmed_at,
                                   cash_rounding_in_vat_base)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (tenant_id) DO UPDATE SET
             establishment_country = EXCLUDED.establishment_country,
             vat_payer = EXCLUDED.vat_payer, vat_id = EXCLUDED.vat_id,
             sk_ic_dph = EXCLUDED.sk_ic_dph,
             distance_sales_mode = EXCLUDED.distance_sales_mode,
             origin_threshold_confirmed_at = EXCLUDED.origin_threshold_confirmed_at,
             cash_rounding_in_vat_base = EXCLUDED.cash_rounding_in_vat_base,
             updated_at = now()",
        tenant_id,
        input.establishment_country,
        input.vat_payer,
        input.vat_id,
        input.sk_ic_dph,
        input.distance_sales_mode.as_str(),
        confirmed_at,
        input.cash_rounding_in_vat_base
    )
    .execute(&mut **tx)
    .await?;
    let after = require(tx).await?;
    audit::record(
        tx,
        actor,
        "tax_profile.updated",
        "tax_profile",
        None,
        &json!({ "before": before, "after": after }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "tax_profile.updated", &json!({})).await?;
    Ok(after)
}

/// The VAT for `product_id` shipped to `ship_to` with tax point `at` (A3): the liable country
/// from the profile, the product's category there (default `standard`), its rate on `at`.
pub async fn resolve(
    tx: &mut TenantTx,
    product_id: Uuid,
    ship_to: &str,
    at: NaiveDate,
) -> Result<Liability, Error> {
    let profile = require(tx).await?;
    let Some(country) = liable_country(&profile, ship_to)? else {
        return Ok(Liability {
            country: None,
            category: None,
            rate: TaxRate::ZERO,
        });
    };
    let category = categories::product_rate(tx, product_id, country, at)
        .await?
        .ok_or_else(|| {
            invalid(
                "no_tax_rate",
                format!("no VAT rate is known for {country} on {at}"),
            )
        })?;
    Ok(Liability {
        country: Some(category.country),
        rate: category
            .rate
            .parse()
            .map_err(|()| Error::Internal(format!("bad stored rate {}", category.rate)))?,
        category: Some(category.code),
    })
}

/// A3 at checkout: `ship_to` must be one of the market's countries and covered by the profile.
pub async fn validate_ship_to(
    tx: &mut TenantTx,
    market_id: Uuid,
    ship_to: &str,
) -> Result<(), Error> {
    let countries =
        sqlx::query_scalar!("SELECT country_codes FROM markets WHERE id = $1", market_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(Error::NotFound)?;
    let profile = require(tx).await?;
    check_ship_to(&profile, &countries, ship_to)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn input(country: &str) -> TaxProfileInput {
        TaxProfileInput {
            establishment_country: country.into(),
            vat_payer: true,
            vat_id: Some("CZ12345678".into()),
            sk_ic_dph: None,
            distance_sales_mode: DistanceSalesMode::Destination,
            confirm_origin_threshold: false,
            cash_rounding_in_vat_base: false,
        }
    }

    fn profile(mode: DistanceSalesMode, payer: bool) -> TaxProfile {
        TaxProfile {
            establishment_country: "CZ".into(),
            vat_payer: payer,
            vat_id: Some("CZ12345678".into()),
            sk_ic_dph: None,
            distance_sales_mode: mode,
            origin_threshold_confirmed_at: Some(Utc::now()),
            cash_rounding_in_vat_base: false,
            updated_at: Utc::now(),
        }
    }

    fn code(r: Result<(), Error>) -> &'static str {
        r.unwrap_err().code()
    }

    #[test]
    fn rates_parse_and_print() {
        for (s, v, back) in [
            ("21", 2100, "21"),
            ("13.5", 1350, "13.5"),
            ("2.10", 210, "2.1"),
            ("5.25", 525, "5.25"),
            ("0", 0, "0"),
        ] {
            let r: TaxRate = s.parse().unwrap();
            assert_eq!(r, TaxRate(v));
            assert_eq!(r.to_string(), back);
        }
        for bad in ["", "100", "1.234", "-1", "a", "1.", ".5", "21 "] {
            assert!(bad.parse::<TaxRate>().is_err(), "{bad}");
        }
        assert_eq!(serde_json::to_value(TaxRate(1350)).unwrap(), json!("13.5"));
        assert!(serde_json::from_value::<TaxRate>(json!(21)).is_err());
    }

    #[test]
    fn vat_extraction() {
        // 121,00 Kč incl. 21 %: 21,00 VAT.
        assert_eq!(vat_from_gross(12_100, TaxRate(2100)), 2100);
        // 100,00 Kč incl. 21 %: 17,355… -> 17,36.
        assert_eq!(vat_from_gross(10_000, TaxRate(2100)), 1736);
        // 12 % reduced: 100 * 12/112 = 10,714… -> 10,71.
        assert_eq!(vat_from_gross(10_000, TaxRate(1200)), 1071);
        // Tiny amounts: 1 * 21/121 = 0.17 -> 0; 3 * 21/121 = 0.52 -> 1.
        assert_eq!(vat_from_gross(1, TaxRate(2100)), 0);
        assert_eq!(vat_from_gross(3, TaxRate(2100)), 1);
        assert_eq!(vat_from_gross(-10_000, TaxRate(2100)), -1736);
        assert_eq!(vat_from_gross(10_000, TaxRate::ZERO), 0);
    }

    #[test]
    fn profile_validation() {
        assert!(input("CZ").validate(None).is_ok());
        assert_eq!(
            code(input("US").validate(None)),
            "invalid_establishment_country"
        );
        assert_eq!(
            code(
                TaxProfileInput {
                    vat_id: Some("12345678".into()),
                    ..input("CZ")
                }
                .validate(None)
            ),
            "invalid_vat_id"
        );
        assert_eq!(
            code(
                TaxProfileInput {
                    vat_id: None,
                    ..input("CZ")
                }
                .validate(None)
            ),
            "vat_id_required"
        );
        // Non-payers need no identifier.
        assert!(
            TaxProfileInput {
                vat_id: None,
                vat_payer: false,
                ..input("CZ")
            }
            .validate(None)
            .is_ok()
        );
        let sk = TaxProfileInput {
            vat_id: Some("2020123456".into()),
            sk_ic_dph: Some("SK2020123456".into()),
            ..input("SK")
        };
        assert!(sk.validate(None).is_ok());
        assert_eq!(
            code(
                TaxProfileInput {
                    sk_ic_dph: None,
                    ..sk.clone()
                }
                .validate(None)
            ),
            "vat_id_required"
        );
        assert_eq!(
            code(
                TaxProfileInput {
                    sk_ic_dph: Some("SK1".into()),
                    ..sk
                }
                .validate(None)
            ),
            "invalid_sk_ic_dph"
        );
        assert_eq!(
            code(
                TaxProfileInput {
                    sk_ic_dph: Some("SK2020123456".into()),
                    ..input("CZ")
                }
                .validate(None)
            ),
            "invalid_sk_ic_dph"
        );
        let origin = TaxProfileInput {
            distance_sales_mode: DistanceSalesMode::OriginThreshold,
            ..input("CZ")
        };
        assert_eq!(
            code(origin.validate(None)),
            "origin_threshold_confirmation_required"
        );
        assert!(
            TaxProfileInput {
                confirm_origin_threshold: true,
                ..origin.clone()
            }
            .validate(None)
            .is_ok()
        );
        // Already confirmed earlier: no need to confirm again.
        let prev = profile(DistanceSalesMode::OriginThreshold, true);
        assert!(origin.validate(Some(&prev)).is_ok());
        assert!(serde_json::from_value::<TaxProfileInput>(json!({
            "establishment_country": "CZ", "vat_payer": false, "distance_sales_mode": "destination",
            "oss": true
        }))
        .is_err());
    }

    #[test]
    fn liability_per_a3() {
        let dest = profile(DistanceSalesMode::Destination, true);
        assert_eq!(liable_country(&dest, "CZ").unwrap(), Some("CZ"));
        assert_eq!(liable_country(&dest, "SK").unwrap(), Some("SK"));
        let origin = profile(DistanceSalesMode::OriginThreshold, true);
        assert_eq!(liable_country(&origin, "SK").unwrap(), Some("CZ"));
        assert_eq!(liable_country(&origin, "CZ").unwrap(), Some("CZ"));
        let non_payer = profile(DistanceSalesMode::Destination, false);
        assert_eq!(liable_country(&non_payer, "SK").unwrap(), None);
        assert_eq!(
            liable_country(&dest, "CH").unwrap_err().code(),
            "ship_to_not_supported"
        );
        assert_eq!(
            liable_country(&non_payer, "US").unwrap_err().code(),
            "ship_to_not_supported"
        );
    }

    #[test]
    fn ship_to_allowlist() {
        let p = profile(DistanceSalesMode::Destination, true);
        let market = vec!["CZ".to_owned(), "SK".to_owned(), "CH".to_owned()];
        assert!(check_ship_to(&p, &market, "SK").is_ok());
        assert_eq!(
            code(check_ship_to(&p, &market, "PL")),
            "ship_to_not_allowed"
        );
        // Listed by the market but not covered by the tax setup.
        assert_eq!(
            code(check_ship_to(&p, &market, "CH")),
            "ship_to_not_supported"
        );
    }

    proptest! {
        #[test]
        fn vat_is_bounded_and_net_nonnegative(gross in 0i64..1_000_000_000_000, rate in 0u32..10_000) {
            let vat = vat_from_gross(gross, TaxRate(rate));
            prop_assert!(vat >= 0 && vat <= gross);
            // Within half a unit of the exact value.
            let exact_num = i128::from(gross) * i128::from(rate);
            let den = 10_000 + i128::from(rate);
            prop_assert!((i128::from(vat) * den - exact_num).abs() * 2 <= den);
        }

        #[test]
        fn rate_text_roundtrip(rate in 0u32..10_000) {
            let r = TaxRate(rate);
            prop_assert_eq!(r.to_string().parse::<TaxRate>().unwrap(), r);
        }
    }
}
