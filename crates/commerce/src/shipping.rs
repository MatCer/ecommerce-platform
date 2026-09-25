//! Shipping methods (spec §7.4, §10.5): per market, a carrier kind, localized names and the
//! rate rules: a flat price, optional weight tiers (the first tier covering the cart's weight
//! wins; heavier carts cannot use the method), an optional free-over threshold on the goods
//! after the coupon, and cash on delivery with its fee.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::catalog::{I18n, check_i18n};
use crate::markets::invalid;

/// Upper bound for a fee (10^8 minor units).
const MAX_FEE: i64 = 100_000_000;
const MAX_TIERS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Carrier {
    /// Packeta (Zásilkovna) pickup points and boxes, chosen in the Packeta widget.
    PacketaPickup,
    PacketaHome,
    Ppl,
    /// The customer collects the parcel at the merchant.
    PersonalPickup,
}

impl Carrier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PacketaPickup => "packeta_pickup",
            Self::PacketaHome => "packeta_home",
            Self::Ppl => "ppl",
            Self::PersonalPickup => "personal_pickup",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "packeta_pickup" => Self::PacketaPickup,
            "packeta_home" => Self::PacketaHome,
            "ppl" => Self::Ppl,
            "personal_pickup" => Self::PersonalPickup,
            other => return Err(Error::Internal(format!("unknown carrier {other}"))),
        })
    }

    /// The parcel goes to a pickup point chosen in the carrier's widget.
    pub fn needs_pickup_point(self) -> bool {
        self == Self::PacketaPickup
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WeightTier {
    /// Carts up to this weight (grams, inclusive) pay `price_minor`.
    pub up_to_g: i64,
    pub price_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShippingMethod {
    pub id: Uuid,
    pub market_id: Uuid,
    pub carrier: Carrier,
    pub name_i18n: I18n,
    pub description_i18n: I18n,
    /// Flat price (gross, market currency) when there are no weight tiers.
    pub price_minor: i64,
    /// Free when the goods after the coupon reach this amount.
    pub free_over_minor: Option<i64>,
    /// Ascending by weight; empty = flat price.
    pub weight_tiers: Vec<WeightTier>,
    pub cod_allowed: bool,
    /// Cash-on-delivery fee (gross), charged as the payment fee.
    pub cod_fee_minor: i64,
    pub active: bool,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ShippingMethod {
    /// The shipping price for goods worth `goods_minor` (after the coupon) weighing
    /// `weight_g`; `None` when the cart is too heavy for every tier.
    pub fn quote(&self, goods_minor: i64, weight_g: i64) -> Option<i64> {
        let price = if self.weight_tiers.is_empty() {
            self.price_minor
        } else {
            self.weight_tiers
                .iter()
                .find(|t| weight_g <= t.up_to_g)?
                .price_minor
        };
        Some(match self.free_over_minor {
            Some(threshold) if goods_minor >= threshold => 0,
            _ => price,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ShippingMethodInput {
    pub market_id: Uuid,
    pub carrier: Carrier,
    #[schema(example = json!({"cs": "Zásilkovna – výdejní místo"}))]
    pub name_i18n: I18n,
    #[serde(default)]
    pub description_i18n: I18n,
    pub price_minor: i64,
    #[serde(default)]
    pub free_over_minor: Option<i64>,
    #[serde(default)]
    pub weight_tiers: Vec<WeightTier>,
    #[serde(default)]
    pub cod_allowed: bool,
    #[serde(default)]
    pub cod_fee_minor: i64,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub position: i32,
}

fn yes() -> bool {
    true
}

impl ShippingMethodInput {
    pub fn validate(&self) -> Result<(), Error> {
        const CODE: &str = "invalid_shipping_method";
        check_i18n("name_i18n", CODE, &self.name_i18n, 100, true)?;
        check_i18n("description_i18n", CODE, &self.description_i18n, 300, false)?;
        let fee = |v: i64| (0..=MAX_FEE).contains(&v);
        if !fee(self.price_minor) || !fee(self.cod_fee_minor) {
            return Err(invalid(CODE, format!("prices must be 0-{MAX_FEE}")));
        }
        if self
            .free_over_minor
            .is_some_and(|f| !(1..=MAX_FEE * 1000).contains(&f))
        {
            return Err(invalid(CODE, "free_over_minor must be positive"));
        }
        if self.weight_tiers.len() > MAX_TIERS {
            return Err(invalid(CODE, format!("at most {MAX_TIERS} weight tiers")));
        }
        let mut last = 0;
        for t in &self.weight_tiers {
            if t.up_to_g <= last || t.up_to_g > 1_000_000_000 || !fee(t.price_minor) {
                return Err(invalid(
                    CODE,
                    "weight tiers must ascend by up_to_g (grams) with valid prices",
                ));
            }
            last = t.up_to_g;
        }
        if !self.cod_allowed && self.cod_fee_minor != 0 {
            return Err(invalid(CODE, "a COD fee needs cod_allowed"));
        }
        if !(0..=10_000).contains(&self.position) {
            return Err(invalid(CODE, "position must be 0-10000"));
        }
        Ok(())
    }
}

struct Row {
    id: Uuid,
    market_id: Uuid,
    carrier: String,
    name_i18n: serde_json::Value,
    description_i18n: serde_json::Value,
    price_minor: i64,
    free_over_minor: Option<i64>,
    weight_tiers: serde_json::Value,
    cod_allowed: bool,
    cod_fee_minor: i64,
    active: bool,
    position: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Row {
    fn into_method(self) -> Result<ShippingMethod, Error> {
        let bad = |e: serde_json::Error| Error::Internal(format!("stored shipping method: {e}"));
        Ok(ShippingMethod {
            id: self.id,
            market_id: self.market_id,
            carrier: Carrier::parse(&self.carrier)?,
            name_i18n: serde_json::from_value(self.name_i18n).map_err(bad)?,
            description_i18n: serde_json::from_value(self.description_i18n).map_err(bad)?,
            price_minor: self.price_minor,
            free_over_minor: self.free_over_minor,
            weight_tiers: serde_json::from_value(self.weight_tiers).map_err(bad)?,
            cod_allowed: self.cod_allowed,
            cod_fee_minor: self.cod_fee_minor,
            active: self.active,
            position: self.position,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn to_json<T: Serialize>(v: &T) -> Result<serde_json::Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::Internal(e.to_string()))
}

/// Every method of the tenant (or of one market), by market and position.
pub async fn list(
    tx: &mut TenantTx,
    market_id: Option<Uuid>,
) -> Result<Vec<ShippingMethod>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, market_id, carrier, name_i18n, description_i18n, price_minor, free_over_minor,
                weight_tiers, cod_allowed, cod_fee_minor, active, position, created_at, updated_at
         FROM shipping_methods
         WHERE $1::uuid IS NULL OR market_id = $1
         ORDER BY market_id, position, created_at",
        market_id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Row::into_method)
    .collect()
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<ShippingMethod, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, market_id, carrier, name_i18n, description_i18n, price_minor, free_over_minor,
                weight_tiers, cod_allowed, cod_fee_minor, active, position, created_at, updated_at
         FROM shipping_methods WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .into_method()
}

/// The market's active methods (what checkout offers).
pub async fn active(tx: &mut TenantTx, market_id: Uuid) -> Result<Vec<ShippingMethod>, Error> {
    Ok(list(tx, Some(market_id))
        .await?
        .into_iter()
        .filter(|m| m.active)
        .collect())
}

/// The lowest free-shipping threshold among the market's active methods (the theme's
/// free-delivery progress bar).
pub async fn lowest_free_threshold(
    tx: &mut TenantTx,
    market_id: Uuid,
) -> Result<Option<i64>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT min(free_over_minor) FROM shipping_methods WHERE market_id = $1 AND active",
        market_id
    )
    .fetch_one(&mut **tx)
    .await?)
}

async fn market_exists(tx: &mut TenantTx, market_id: Uuid) -> Result<(), Error> {
    sqlx::query_scalar!("SELECT id FROM markets WHERE id = $1", market_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|_| ())
        .ok_or_else(|| invalid("unknown_market", "no such market"))
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &ShippingMethodInput,
) -> Result<ShippingMethod, Error> {
    input.validate()?;
    market_exists(tx, input.market_id).await?;
    let id = sqlx::query_scalar!(
        "INSERT INTO shipping_methods (id, tenant_id, market_id, carrier, name_i18n, description_i18n,
             price_minor, free_over_minor, weight_tiers, cod_allowed, cod_fee_minor, active, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
        crate::id::new_id(),
        tx.tenant_id(),
        input.market_id,
        input.carrier.as_str(),
        to_json(&input.name_i18n)?,
        to_json(&input.description_i18n)?,
        input.price_minor,
        input.free_over_minor,
        to_json(&input.weight_tiers)?,
        input.cod_allowed,
        input.cod_fee_minor,
        input.active,
        input.position
    )
    .fetch_one(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "shipping_method.created",
        "shipping_method",
        Some(&id.to_string()),
        &to_json(input)?,
    )
    .await?;
    get(tx, id).await
}

/// Replaces a method. Its market cannot change (orders and carts point at it).
pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &ShippingMethodInput,
) -> Result<ShippingMethod, Error> {
    input.validate()?;
    let before = get(tx, id).await?;
    if before.market_id != input.market_id {
        return Err(invalid(
            "invalid_shipping_method",
            "a method cannot move to another market",
        ));
    }
    sqlx::query!(
        "UPDATE shipping_methods SET carrier = $2, name_i18n = $3, description_i18n = $4,
             price_minor = $5, free_over_minor = $6, weight_tiers = $7, cod_allowed = $8,
             cod_fee_minor = $9, active = $10, position = $11, updated_at = now()
         WHERE id = $1",
        id,
        input.carrier.as_str(),
        to_json(&input.name_i18n)?,
        to_json(&input.description_i18n)?,
        input.price_minor,
        input.free_over_minor,
        to_json(&input.weight_tiers)?,
        input.cod_allowed,
        input.cod_fee_minor,
        input.active,
        input.position
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "shipping_method.updated",
        "shipping_method",
        Some(&id.to_string()),
        &json!({ "before": before, "after": input }),
    )
    .await?;
    get(tx, id).await
}

/// Deletes a method; carts that had it selected lose the selection, orders keep their snapshot.
pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM shipping_methods WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "shipping_method.deleted",
        "shipping_method",
        Some(&id.to_string()),
        &to_json(&before)?,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(tiers: Vec<WeightTier>, free: Option<i64>) -> ShippingMethod {
        ShippingMethod {
            id: Uuid::nil(),
            market_id: Uuid::nil(),
            carrier: Carrier::Ppl,
            name_i18n: I18n::new(),
            description_i18n: I18n::new(),
            price_minor: 9900,
            free_over_minor: free,
            weight_tiers: tiers,
            cod_allowed: false,
            cod_fee_minor: 0,
            active: true,
            position: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn flat_tiers_and_free_over() {
        let flat = method(vec![], Some(150_000));
        assert_eq!(flat.quote(149_999, 99_999), Some(9900));
        assert_eq!(flat.quote(150_000, 99_999), Some(0));

        let tiers = vec![
            WeightTier {
                up_to_g: 2000,
                price_minor: 7900,
            },
            WeightTier {
                up_to_g: 10_000,
                price_minor: 12_900,
            },
        ];
        let tiered = method(tiers, None);
        assert_eq!(tiered.quote(1, 0), Some(7900));
        assert_eq!(tiered.quote(1, 2000), Some(7900));
        assert_eq!(tiered.quote(1, 2001), Some(12_900));
        assert_eq!(tiered.quote(1, 10_001), None, "too heavy");
    }

    fn input() -> ShippingMethodInput {
        ShippingMethodInput {
            market_id: Uuid::nil(),
            carrier: Carrier::PacketaPickup,
            name_i18n: I18n::from([("cs".into(), "Zásilkovna".into())]),
            description_i18n: I18n::new(),
            price_minor: 7900,
            free_over_minor: Some(150_000),
            weight_tiers: vec![],
            cod_allowed: true,
            cod_fee_minor: 3900,
            active: true,
            position: 0,
        }
    }

    #[test]
    fn validation() {
        assert!(input().validate().is_ok());
        let bad = [
            ShippingMethodInput {
                name_i18n: I18n::new(),
                ..input()
            },
            ShippingMethodInput {
                price_minor: -1,
                ..input()
            },
            ShippingMethodInput {
                free_over_minor: Some(0),
                ..input()
            },
            ShippingMethodInput {
                cod_allowed: false,
                ..input()
            },
            ShippingMethodInput {
                weight_tiers: vec![
                    WeightTier {
                        up_to_g: 5000,
                        price_minor: 1,
                    },
                    WeightTier {
                        up_to_g: 5000,
                        price_minor: 2,
                    },
                ],
                ..input()
            },
        ];
        for b in bad {
            assert!(b.validate().is_err(), "{b:?}");
        }
    }
}
