//! Coupons (spec §7.2, §10.2): cart-level codes. At most one coupon per cart, on top of the
//! running sales. Limits are enforced when an order redeems the coupon, under the coupon's row
//! lock, so concurrent checkouts can never exceed `usage_limit` or `per_customer_limit`.
//!
//! A `published` coupon (a code anyone may use) counts as a price reduction for the Omnibus
//! reference (A18). Its terms therefore cannot change once it has started: only its end, its
//! usage limits and its per-customer limit remain editable.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;
use crate::money::Currency;
use crate::pricing::cart::{AppliedCoupon, CouponDiscount, MAX_AMOUNT};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Coupon {
    pub id: Uuid,
    #[schema(example = "PODZIM10")]
    pub code: String,
    pub discount: CouponDiscount,
    /// Currency of a fixed amount and of `min_subtotal_minor`.
    pub currency: Option<Currency>,
    pub min_subtotal_minor: Option<i64>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub per_customer_limit: Option<i32>,
    pub used_count: i32,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CouponInput {
    /// 3-32 characters `A-Z 0-9 _ -`; lowercase input is uppercased.
    #[schema(example = "PODZIM10")]
    pub code: String,
    pub discount: CouponDiscount,
    /// Required for a fixed discount or a minimum subtotal.
    pub currency: Option<Currency>,
    pub min_subtotal_minor: Option<i64>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub per_customer_limit: Option<i32>,
    /// Available to all customers (advertised). Counts for the Omnibus reference.
    #[serde(default)]
    pub published: bool,
}

pub fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

fn code_valid(code: &str) -> bool {
    let b = code.as_bytes();
    (3..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

impl CouponInput {
    pub fn validate(&self) -> Result<(), Error> {
        if !code_valid(&normalize_code(&self.code)) {
            return Err(invalid(
                "invalid_code",
                "code must be 3-32 letters, digits, `_` or `-`",
            ));
        }
        match self.discount {
            CouponDiscount::Percent { basis_points } if !(1..=10_000).contains(&basis_points) => {
                return Err(invalid("invalid_discount", "basis_points must be 1-10000"));
            }
            CouponDiscount::Fixed { amount_minor } if !(1..=MAX_AMOUNT).contains(&amount_minor) => {
                return Err(invalid("invalid_discount", "amount_minor must be positive"));
            }
            CouponDiscount::Fixed { .. } if self.currency.is_none() => {
                return Err(invalid(
                    "currency_required",
                    "a fixed coupon needs a currency",
                ));
            }
            _ => {}
        }
        if let Some(m) = self.min_subtotal_minor {
            if !(1..=MAX_AMOUNT).contains(&m) {
                return Err(invalid(
                    "invalid_min_subtotal",
                    "min_subtotal_minor must be positive",
                ));
            }
            if self.currency.is_none() {
                return Err(invalid(
                    "currency_required",
                    "a minimum subtotal needs a currency",
                ));
            }
        }
        if self.usage_limit.is_some_and(|l| l < 1) || self.per_customer_limit.is_some_and(|l| l < 1)
        {
            return Err(invalid("invalid_limit", "limits must be at least 1"));
        }
        if let (Some(s), Some(e)) = (self.starts_at, self.ends_at)
            && e <= s
        {
            return Err(invalid(
                "invalid_schedule",
                "ends_at must be after starts_at",
            ));
        }
        Ok(())
    }
}

/// Why a coupon cannot be applied (`422` with this code at the cart).
fn reject(code: &'static str, detail: &str) -> Error {
    invalid(code, detail.to_owned())
}

/// Checks a coupon for a cart and returns what `price_cart` applies. Pure. `customer_uses`:
/// this customer's earlier redemptions (0 for an unknown guest). The final limit check
/// happens atomically in [`redeem`].
pub fn evaluate(
    coupon: &Coupon,
    currency: Currency,
    goods_minor: i64,
    customer_uses: i64,
    now: DateTime<Utc>,
) -> Result<AppliedCoupon, Error> {
    if coupon.starts_at.is_some_and(|s| s > now) || coupon.ends_at.is_some_and(|e| e <= now) {
        return Err(reject("coupon_not_active", "this coupon is not valid now"));
    }
    if coupon.currency.is_some_and(|c| c != currency) {
        return Err(reject(
            "coupon_currency_mismatch",
            "this coupon is not valid in this currency",
        ));
    }
    if coupon.min_subtotal_minor.is_some_and(|m| goods_minor < m) {
        return Err(reject(
            "coupon_min_subtotal",
            "the order total is below this coupon's minimum",
        ));
    }
    if coupon.usage_limit.is_some_and(|l| coupon.used_count >= l) {
        return Err(reject("coupon_exhausted", "this coupon has been used up"));
    }
    if coupon
        .per_customer_limit
        .is_some_and(|l| customer_uses >= i64::from(l))
    {
        return Err(reject(
            "coupon_customer_limit",
            "you have already used this coupon",
        ));
    }
    Ok(AppliedCoupon {
        code: coupon.code.clone(),
        discount: coupon.discount,
    })
}

struct Row {
    id: Uuid,
    code: String,
    kind: String,
    value: Option<i64>,
    currency: Option<String>,
    min_subtotal_minor: Option<i64>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    usage_limit: Option<i32>,
    per_customer_limit: Option<i32>,
    used_count: i32,
    published: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl Row {
    fn into_coupon(self) -> Result<Coupon, Error> {
        let bad = || Error::Internal(format!("malformed coupon {}", self.id));
        let discount = match (self.kind.as_str(), self.value) {
            ("percent", Some(v)) => CouponDiscount::Percent {
                basis_points: u32::try_from(v).map_err(|_| bad())?,
            },
            ("fixed", Some(v)) => CouponDiscount::Fixed { amount_minor: v },
            ("free_shipping", None) => CouponDiscount::FreeShipping,
            _ => return Err(bad()),
        };
        let currency = match self.currency.as_deref() {
            Some(c) => Some(Currency::parse(c).ok_or_else(bad)?),
            None => None,
        };
        Ok(Coupon {
            id: self.id,
            code: self.code,
            discount,
            currency,
            min_subtotal_minor: self.min_subtotal_minor,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            usage_limit: self.usage_limit,
            per_customer_limit: self.per_customer_limit,
            used_count: self.used_count,
            published: self.published,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn columns(d: CouponDiscount) -> (&'static str, Option<i64>) {
    match d {
        CouponDiscount::Percent { basis_points } => ("percent", Some(i64::from(basis_points))),
        CouponDiscount::Fixed { amount_minor } => ("fixed", Some(amount_minor)),
        CouponDiscount::FreeShipping => ("free_shipping", None),
    }
}

fn db_error(e: sqlx::Error) -> Error {
    let constraint = e
        .as_database_error()
        .and_then(|d| d.constraint())
        .unwrap_or_default()
        .to_owned();
    match constraint.as_str() {
        "coupons_code_unique" => Error::Conflict {
            code: "code_taken",
            detail: "a coupon with this code already exists".into(),
        },
        "coupons_usage_within_limit" => Error::Conflict {
            code: "usage_limit_below_used",
            detail: "usage_limit is below the number of redemptions".into(),
        },
        _ => e.into(),
    }
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Coupon, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, code, kind, value, currency, min_subtotal_minor, starts_at, ends_at,
                usage_limit, per_customer_limit, used_count, published, created_at, updated_at
         FROM coupons WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .into_coupon()
}

/// The coupon with `code` (case-insensitive), if any.
pub async fn find_by_code(tx: &mut TenantTx, code: &str) -> Result<Option<Coupon>, Error> {
    sqlx::query_as!(
        Row,
        "SELECT id, code, kind, value, currency, min_subtotal_minor, starts_at, ends_at,
                usage_limit, per_customer_limit, used_count, published, created_at, updated_at
         FROM coupons WHERE code = $1",
        normalize_code(code)
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(Row::into_coupon)
    .transpose()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CouponPage {
    pub items: Vec<Coupon>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

/// Newest first.
pub async fn list(
    tx: &mut TenantTx,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<CouponPage, Error> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, code, kind, value, currency, min_subtotal_minor, starts_at, ends_at,
                usage_limit, per_customer_limit, used_count, published, created_at, updated_at
         FROM coupons WHERE $1::uuid IS NULL OR id < $1 ORDER BY id DESC LIMIT $2",
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(Row::into_coupon)
        .collect::<Result<Vec<_>, _>>()?;
    let limit = usize::try_from(limit).unwrap_or(100);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(CouponPage { items, next_cursor })
}

pub async fn create(tx: &mut TenantTx, actor: &str, input: &CouponInput) -> Result<Coupon, Error> {
    input.validate()?;
    let (kind, value) = columns(input.discount);
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO coupons (id, tenant_id, code, kind, value, currency, min_subtotal_minor,
                              starts_at, ends_at, usage_limit, per_customer_limit, published)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        id,
        tx.tenant_id(),
        normalize_code(&input.code),
        kind,
        value,
        input.currency.map(Currency::code),
        input.min_subtotal_minor,
        input.starts_at,
        input.ends_at,
        input.usage_limit,
        input.per_customer_limit,
        input.published
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let coupon = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "coupon.created",
        "coupon",
        Some(&id.to_string()),
        &json!({ "after": coupon }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "coupon.created", &json!({ "coupon_id": id })).await?;
    Ok(coupon)
}

fn started(c: &Coupon, now: DateTime<Utc>) -> bool {
    c.starts_at.unwrap_or(c.created_at) <= now
}

/// Replaces a coupon's settings. Once a published coupon has started, only `ends_at` (not
/// into the past), `usage_limit` and `per_customer_limit` may change: its terms are part of
/// the Omnibus price history.
pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &CouponInput,
) -> Result<Coupon, Error> {
    input.validate()?;
    let before = get(tx, id).await?;
    let now = Utc::now();
    if before.published && started(&before, now) {
        let same_terms = normalize_code(&input.code) == before.code
            && input.discount == before.discount
            && input.currency == before.currency
            && input.min_subtotal_minor == before.min_subtotal_minor
            && input.starts_at == before.starts_at
            && input.published;
        let end_ok = input.ends_at == before.ends_at || input.ends_at.is_some_and(|e| e >= now);
        if !same_terms || !end_ok {
            return Err(Error::Conflict {
                code: "coupon_started",
                detail: "a published coupon's terms cannot change after it started; \
                         only ends_at (not in the past) and limits can"
                    .into(),
            });
        }
    }
    let (kind, value) = columns(input.discount);
    sqlx::query!(
        "UPDATE coupons SET code = $2, kind = $3, value = $4, currency = $5,
                            min_subtotal_minor = $6, starts_at = $7, ends_at = $8,
                            usage_limit = $9, per_customer_limit = $10, published = $11,
                            updated_at = now()
         WHERE id = $1",
        id,
        normalize_code(&input.code),
        kind,
        value,
        input.currency.map(Currency::code),
        input.min_subtotal_minor,
        input.starts_at,
        input.ends_at,
        input.usage_limit,
        input.per_customer_limit,
        input.published
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let after = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "coupon.updated",
        "coupon",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "coupon.updated", &json!({ "coupon_id": id })).await?;
    Ok(after)
}

/// Deletes an unused coupon. A redeemed or a started published coupon is history: end it by
/// setting `ends_at` instead (`409 coupon_in_use`).
pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    let redeemed = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM coupon_redemptions WHERE coupon_id = $1) AS \"x!\"",
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    if redeemed || (before.published && started(&before, Utc::now())) {
        return Err(Error::Conflict {
            code: "coupon_in_use",
            detail: "the coupon was used or advertised; set ends_at to end it".into(),
        });
    }
    sqlx::query!("DELETE FROM coupons WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "coupon.deleted",
        "coupon",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    platform::queue::publish(&mut **tx, "coupon.deleted", &json!({ "coupon_id": id })).await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Redemption {
    pub id: Uuid,
    pub coupon_id: Uuid,
    pub order_ref: String,
    pub redeemed_at: DateTime<Utc>,
}

/// Redeems the coupon for an order, in the order-placement transaction (A12). Idempotent per
/// `(coupon, order_ref)`. The coupon row is locked first, so racing redemptions are
/// serialized and the limits hold: `409 coupon_exhausted` / `coupon_customer_limit`.
pub async fn redeem(
    tx: &mut TenantTx,
    coupon_id: Uuid,
    customer_key: &str,
    order_ref: &str,
    now: DateTime<Utc>,
) -> Result<Redemption, Error> {
    if !(1..=320).contains(&customer_key.len()) || !(1..=255).contains(&order_ref.len()) {
        return Err(invalid(
            "invalid_reference",
            "customer_key or order_ref out of range",
        ));
    }
    let coupon = sqlx::query!(
        "SELECT usage_limit, per_customer_limit, used_count, starts_at, ends_at
         FROM coupons WHERE id = $1 FOR UPDATE",
        coupon_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    if let Some(r) = sqlx::query_as!(
        Redemption,
        "SELECT id, coupon_id, order_ref, redeemed_at FROM coupon_redemptions
         WHERE coupon_id = $1 AND order_ref = $2",
        coupon_id,
        order_ref
    )
    .fetch_optional(&mut **tx)
    .await?
    {
        return Ok(r);
    }
    if coupon.starts_at.is_some_and(|s| s > now) || coupon.ends_at.is_some_and(|e| e <= now) {
        return Err(Error::Conflict {
            code: "coupon_not_active",
            detail: "this coupon is not valid now".into(),
        });
    }
    if coupon.usage_limit.is_some_and(|l| coupon.used_count >= l) {
        return Err(Error::Conflict {
            code: "coupon_exhausted",
            detail: "this coupon has been used up".into(),
        });
    }
    if let Some(limit) = coupon.per_customer_limit {
        let used = sqlx::query_scalar!(
            "SELECT count(*) AS \"n!\" FROM coupon_redemptions
             WHERE coupon_id = $1 AND customer_key = $2",
            coupon_id,
            customer_key
        )
        .fetch_one(&mut **tx)
        .await?;
        if used >= i64::from(limit) {
            return Err(Error::Conflict {
                code: "coupon_customer_limit",
                detail: "the customer has already used this coupon".into(),
            });
        }
    }
    sqlx::query!(
        "UPDATE coupons SET used_count = used_count + 1 WHERE id = $1",
        coupon_id
    )
    .execute(&mut **tx)
    .await?;
    let r = sqlx::query_as!(
        Redemption,
        "INSERT INTO coupon_redemptions (id, tenant_id, coupon_id, customer_key, order_ref, redeemed_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, coupon_id, order_ref, redeemed_at",
        crate::id::new_id(),
        tx.tenant_id(),
        coupon_id,
        customer_key,
        order_ref,
        now
    )
    .fetch_one(&mut **tx)
    .await?;
    platform::queue::publish(
        &mut **tx,
        "coupon.redeemed",
        &json!({ "coupon_id": coupon_id, "order_ref": order_ref }),
    )
    .await?;
    Ok(r)
}

/// Gives a redemption back (order cancelled before payment). Returns whether one existed.
pub async fn release(tx: &mut TenantTx, coupon_id: Uuid, order_ref: &str) -> Result<bool, Error> {
    sqlx::query!("SELECT id FROM coupons WHERE id = $1 FOR UPDATE", coupon_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let deleted = sqlx::query!(
        "DELETE FROM coupon_redemptions WHERE coupon_id = $1 AND order_ref = $2",
        coupon_id,
        order_ref
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if deleted > 0 {
        sqlx::query!(
            "UPDATE coupons SET used_count = used_count - 1 WHERE id = $1",
            coupon_id
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(deleted > 0)
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn input() -> CouponInput {
        CouponInput {
            code: "podzim10".into(),
            discount: CouponDiscount::Percent { basis_points: 1000 },
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: None,
            per_customer_limit: None,
            published: false,
        }
    }

    fn coupon() -> Coupon {
        let now = Utc::now();
        Coupon {
            id: Uuid::nil(),
            code: "PODZIM10".into(),
            discount: CouponDiscount::Fixed {
                amount_minor: 10_000,
            },
            currency: Some(Currency::Czk),
            min_subtotal_minor: Some(50_000),
            starts_at: Some(now - Duration::days(1)),
            ends_at: Some(now + Duration::days(1)),
            usage_limit: Some(10),
            per_customer_limit: Some(1),
            used_count: 3,
            published: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn validation() {
        assert!(input().validate().is_ok());
        assert_eq!(normalize_code(" podzim10 "), "PODZIM10");
        let code = |i: CouponInput| i.validate().unwrap_err().code();
        assert_eq!(
            code(CouponInput {
                code: "ab".into(),
                ..input()
            }),
            "invalid_code"
        );
        assert_eq!(
            code(CouponInput {
                code: "sleva 10".into(),
                ..input()
            }),
            "invalid_code"
        );
        assert_eq!(
            code(CouponInput {
                discount: CouponDiscount::Fixed { amount_minor: 100 },
                ..input()
            }),
            "currency_required"
        );
        assert_eq!(
            code(CouponInput {
                min_subtotal_minor: Some(100),
                ..input()
            }),
            "currency_required"
        );
        assert_eq!(
            code(CouponInput {
                discount: CouponDiscount::Percent {
                    basis_points: 10_001
                },
                ..input()
            }),
            "invalid_discount"
        );
        assert_eq!(
            code(CouponInput {
                usage_limit: Some(0),
                ..input()
            }),
            "invalid_limit"
        );
    }

    #[test]
    fn evaluation() {
        let now = Utc::now();
        let c = coupon();
        let ok = evaluate(&c, Currency::Czk, 60_000, 0, now).unwrap();
        assert_eq!(
            ok.discount,
            CouponDiscount::Fixed {
                amount_minor: 10_000
            }
        );
        let err =
            |c: &Coupon, cur, goods, uses| evaluate(c, cur, goods, uses, now).unwrap_err().code();
        assert_eq!(
            err(&c, Currency::Eur, 60_000, 0),
            "coupon_currency_mismatch"
        );
        assert_eq!(err(&c, Currency::Czk, 49_999, 0), "coupon_min_subtotal");
        assert_eq!(err(&c, Currency::Czk, 60_000, 1), "coupon_customer_limit");
        let used_up = Coupon {
            used_count: 10,
            ..coupon()
        };
        assert_eq!(err(&used_up, Currency::Czk, 60_000, 0), "coupon_exhausted");
        let expired = Coupon {
            ends_at: Some(now),
            ..coupon()
        };
        assert_eq!(err(&expired, Currency::Czk, 60_000, 0), "coupon_not_active");
        let future = Coupon {
            starts_at: Some(now + Duration::hours(1)),
            ..coupon()
        };
        assert_eq!(err(&future, Currency::Czk, 60_000, 0), "coupon_not_active");
    }
}
