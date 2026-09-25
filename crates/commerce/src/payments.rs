//! Payments (spec §7.4, §10.4, A10, A16): per-market method configuration, the gateway
//! interface, payment attempts and the fake gateway's signed events.
//!
//! A10 flow: order placement commits the order with a `pending` attempt; right after the
//! commit the API initializes the attempt at the provider ([`init`], idempotent, the attempt
//! id is the provider's idempotency key) and hands the client its next action. A failed
//! attempt leaves the order `pending` with payment `failed`; the customer retries with a new
//! attempt on the same order ([`retry`]) until the order's payment window closes
//! (`checkout::expire_due`). Outcomes go through [`apply_outcome`] and the WP9 payment
//! machine, so a success after expiry is recorded as a late payment (order exception, no
//! restock).
//!
//! WP10 implements the fake gateway (local/e2e, `PAYMENTS_FAKE=1`) and cash on delivery (no
//! provider: the courier collects, A16). Stripe and bank transfer are configured here but stay
//! unavailable at checkout until their adapters land (WP11).

use std::future::Future;

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::catalog::{I18n, check_i18n};
use crate::markets::invalid;
use crate::orders::{self, status::PaymentCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MethodKind {
    Stripe,
    BankTransfer,
    /// Cash on delivery: the fee comes from the shipping method, the order is confirmed on
    /// placement (A16).
    Cod,
    /// Local/test gateway (`PAYMENTS_FAKE=1` only).
    Fake,
}

impl MethodKind {
    pub const ALL: [Self; 4] = [Self::Fake, Self::Stripe, Self::BankTransfer, Self::Cod];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stripe => "stripe",
            Self::BankTransfer => "bank_transfer",
            Self::Cod => "cod",
            Self::Fake => "fake",
        }
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "stripe" => Self::Stripe,
            "bank_transfer" => Self::BankTransfer,
            "cod" => Self::Cod,
            "fake" => Self::Fake,
            other => return Err(Error::Internal(format!("unknown payment method {other}"))),
        })
    }

    /// The payment window when the merchant did not set one (§10.3: Stripe 1 h, bank transfer
    /// 7 days); cash on delivery has none.
    pub fn default_timeout_minutes(self) -> Option<i32> {
        match self {
            Self::Stripe | Self::Fake => Some(60),
            Self::BankTransfer => Some(7 * 24 * 60),
            Self::Cod => None,
        }
    }

    pub fn is_cod(self) -> bool {
        self == Self::Cod
    }
}

// ---------------------------------------------------------------------------------------
// Gateways

/// Platform payment settings.
#[derive(Debug, Clone, Default)]
pub struct Payments {
    /// `PAYMENTS_FAKE=1`: the fake gateway and its signing secret.
    pub fake: Option<FakeGateway>,
}

impl Payments {
    /// Whether checkout may offer `kind` (an adapter exists and is configured).
    pub fn available(&self, kind: MethodKind) -> bool {
        match kind {
            MethodKind::Fake => self.fake.is_some(),
            MethodKind::Cod => true,
            // WP11: Stripe Connect and bank transfer (SPAYD / PAY by square) adapters.
            MethodKind::Stripe | MethodKind::BankTransfer => false,
        }
    }
}

/// What an attempt needs from the provider.
#[derive(Debug, Clone)]
pub struct AttemptInit {
    pub attempt_id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    /// Relative path on the checkout origin to come back to (the order page).
    pub return_path: String,
}

/// What the customer does next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NextAction {
    /// Go to the provider's page (relative or absolute URL).
    Redirect { url: String },
    /// Nothing to pay now (cash on delivery).
    None,
}

/// A provider-side payment for an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub provider_ref: String,
    pub action: NextAction,
}

/// A payment provider adapter. `init` must be idempotent per `attempt_id` (it is the
/// provider's idempotency key, A10). WP11 adds Stripe and bank transfer.
pub trait Gateway {
    fn init(&self, attempt: &AttemptInit) -> impl Future<Output = Result<Intent, Error>> + Send;
}

/// The fake gateway: its "provider page" is `/_p/fake-pay/<attempt>` on the checkout origin,
/// whose Succeed/Fail buttons produce events signed with `secret` (verified like a real
/// provider webhook before anything changes).
#[derive(Clone)]
pub struct FakeGateway {
    secret: Vec<u8>,
}

impl std::fmt::Debug for FakeGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeGateway { .. }")
    }
}

impl FakeGateway {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
        }
    }
}

impl Gateway for FakeGateway {
    async fn init(&self, a: &AttemptInit) -> Result<Intent, Error> {
        let mut url = format!("/_p/fake-pay/{}", a.attempt_id);
        if a.return_path.starts_with("/o/") {
            url.push_str("?return=");
            url.push_str(&a.return_path);
        }
        Ok(Intent {
            provider_ref: format!("fake_{}", a.attempt_id.simple()),
            action: NextAction::Redirect { url },
        })
    }
}

/// Cash on delivery: nothing to set up with a provider.
pub struct CodGateway;

impl Gateway for CodGateway {
    async fn init(&self, a: &AttemptInit) -> Result<Intent, Error> {
        Ok(Intent {
            provider_ref: format!("cod_{}", a.attempt_id.simple()),
            action: NextAction::None,
        })
    }
}

/// Outcome of a payment attempt reported by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
}

/// A fake provider event (the shape a webhook body has).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FakeEvent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub attempt_id: Uuid,
    pub outcome: Outcome,
    pub amount_minor: i64,
    pub currency: String,
}

/// Signatures older than this are refused (replayed captures).
const SIGNATURE_TOLERANCE_SECS: i64 = 300;

impl FakeGateway {
    fn mac(&self, ts: i64, body: &[u8]) -> Result<Hmac<Sha256>, Error> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .map_err(|e| Error::Internal(format!("fake gateway key: {e}")))?;
        mac.update(ts.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        Ok(mac)
    }

    /// `t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>` (the §8.5 webhook format).
    pub fn sign(&self, body: &[u8], now: DateTime<Utc>) -> Result<String, Error> {
        let ts = now.timestamp();
        let tag = self.mac(ts, body)?.finalize().into_bytes();
        Ok(format!("t={ts},v1={}", hex::encode(tag)))
    }

    /// Verifies a signature header in constant time and its freshness.
    pub fn verify(&self, header: &str, body: &[u8], now: DateTime<Utc>) -> Result<(), Error> {
        let bad = || Error::Unauthorized {
            code: "invalid_signature",
        };
        let mut ts = None;
        let mut sig = None;
        for part in header.split(',') {
            match part.trim().split_once('=') {
                Some(("t", v)) => ts = v.parse::<i64>().ok(),
                Some(("v1", v)) => sig = hex::decode(v).ok(),
                _ => {}
            }
        }
        let (ts, sig) = ts.zip(sig).ok_or_else(bad)?;
        if (now.timestamp() - ts).abs() > SIGNATURE_TOLERANCE_SECS {
            return Err(bad());
        }
        self.mac(ts, body)?.verify_slice(&sig).map_err(|_| bad())
    }
}

// ---------------------------------------------------------------------------------------
// Method configuration

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PaymentMethod {
    pub market_id: Uuid,
    pub kind: MethodKind,
    pub enabled: bool,
    /// Checkout label per locale; empty = the platform's default name.
    pub name_i18n: I18n,
    /// Payment window for unpaid orders; `null` = the method's default.
    pub timeout_minutes: Option<i32>,
    pub position: i32,
    /// Whether the platform can take payments with it yet (adapter present and configured).
    pub available: bool,
}

impl PaymentMethod {
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_minutes
            .or_else(|| self.kind.default_timeout_minutes())
            .map(|m| Duration::minutes(i64::from(m)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentMethodInput {
    pub enabled: bool,
    #[serde(default)]
    pub name_i18n: I18n,
    /// 5-43200 minutes; `null` = the method's default (Stripe/fake 60, bank transfer 7 days).
    #[serde(default)]
    pub timeout_minutes: Option<i32>,
    #[serde(default)]
    pub position: i32,
}

impl PaymentMethodInput {
    fn validate(&self, kind: MethodKind) -> Result<(), Error> {
        const CODE: &str = "invalid_payment_method";
        check_i18n("name_i18n", CODE, &self.name_i18n, 100, false)?;
        if let Some(t) = self.timeout_minutes {
            if kind.is_cod() {
                return Err(invalid(CODE, "cash on delivery has no payment window"));
            }
            if !(5..=43_200).contains(&t) {
                return Err(invalid(CODE, "timeout_minutes must be 5-43200"));
            }
        }
        if !(0..=10_000).contains(&self.position) {
            return Err(invalid(CODE, "position must be 0-10000"));
        }
        Ok(())
    }
}

/// Every method kind of a market (unconfigured ones disabled), ordered by position.
pub async fn methods(
    tx: &mut TenantTx,
    payments: &Payments,
    market_id: Uuid,
) -> Result<Vec<PaymentMethod>, Error> {
    let rows = sqlx::query!(
        "SELECT kind, enabled, name_i18n, timeout_minutes, position FROM payment_methods
         WHERE market_id = $1",
        market_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for (i, kind) in MethodKind::ALL.into_iter().enumerate() {
        let row = rows.iter().find(|r| r.kind == kind.as_str());
        out.push(PaymentMethod {
            market_id,
            kind,
            enabled: row.is_some_and(|r| r.enabled),
            name_i18n: row
                .map(|r| serde_json::from_value(r.name_i18n.clone()))
                .transpose()
                .map_err(|e| Error::Internal(format!("stored payment method: {e}")))?
                .unwrap_or_default(),
            timeout_minutes: row.and_then(|r| r.timeout_minutes),
            position: row.map_or(i32::try_from(i).unwrap_or(0), |r| r.position),
            available: payments.available(kind),
        });
    }
    out.sort_by_key(|m| m.position);
    Ok(out)
}

/// Configures one method of a market (payment settings: the caller checks role and fresh
/// authentication, A9).
pub async fn configure(
    tx: &mut TenantTx,
    actor: &str,
    payments: &Payments,
    market_id: Uuid,
    kind: MethodKind,
    input: &PaymentMethodInput,
) -> Result<PaymentMethod, Error> {
    input.validate(kind)?;
    sqlx::query_scalar!("SELECT id FROM markets WHERE id = $1", market_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    sqlx::query!(
        "INSERT INTO payment_methods (tenant_id, market_id, kind, enabled, name_i18n, timeout_minutes, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (tenant_id, market_id, kind) DO UPDATE SET enabled = EXCLUDED.enabled,
             name_i18n = EXCLUDED.name_i18n, timeout_minutes = EXCLUDED.timeout_minutes,
             position = EXCLUDED.position, updated_at = now()",
        tx.tenant_id(),
        market_id,
        kind.as_str(),
        input.enabled,
        serde_json::to_value(&input.name_i18n).map_err(|e| Error::Internal(e.to_string()))?,
        input.timeout_minutes,
        input.position
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "payment_method.configured",
        "payment_method",
        Some(&format!("{market_id}:{}", kind.as_str())),
        &json!({ "kind": kind, "input": input }),
    )
    .await?;
    methods(tx, payments, market_id)
        .await?
        .into_iter()
        .find(|m| m.kind == kind)
        .ok_or_else(|| Error::Internal("configured method vanished".into()))
}

// ---------------------------------------------------------------------------------------
// Attempts

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Pending,
    Succeeded,
    Failed,
    Expired,
}

impl AttemptStatus {
    fn parse(s: &str) -> Self {
        match s {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Attempt {
    pub id: Uuid,
    pub order_id: Uuid,
    pub method: MethodKind,
    pub status: AttemptStatus,
    pub amount_minor: i64,
    pub currency: String,
    /// Set once the provider knows the attempt (after `init`).
    pub provider_ref: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// One attempt. Attempts change only under their order's row lock (`orders::lock`), so
/// reading one after taking that lock sees settled data.
pub async fn attempt(tx: &mut TenantTx, id: Uuid) -> Result<Attempt, Error> {
    let r = sqlx::query!(
        "SELECT id, order_id, method, status, amount_minor, currency, provider_ref, expires_at,
                created_at, completed_at
         FROM payment_attempts WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Attempt {
        id: r.id,
        order_id: r.order_id,
        method: MethodKind::parse(&r.method)?,
        status: AttemptStatus::parse(&r.status),
        amount_minor: r.amount_minor,
        currency: r.currency,
        provider_ref: r.provider_ref,
        expires_at: r.expires_at,
        created_at: r.created_at,
        completed_at: r.completed_at,
    })
}

/// The attempts of an order, oldest first.
pub async fn attempts(tx: &mut TenantTx, order_id: Uuid) -> Result<Vec<Attempt>, Error> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM payment_attempts WHERE order_id = $1 ORDER BY created_at, id",
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(attempt(tx, id).await?);
    }
    Ok(out)
}

/// A new pending attempt for the order's total (placement or a retry).
pub(crate) async fn create_attempt(
    tx: &mut TenantTx,
    order_id: Uuid,
    method: MethodKind,
    amount_minor: i64,
    currency: &str,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Uuid, Error> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO payment_attempts (id, tenant_id, order_id, method, amount_minor, currency, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        crate::id::new_id(),
        tx.tenant_id(),
        order_id,
        method.as_str(),
        amount_minor,
        currency,
        expires_at
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// Initializes an attempt at its provider (A10), idempotently: the provider reference is
/// stored once, later calls rebuild the same next action. `409 attempt_not_pending` once the
/// attempt is finished.
pub async fn init(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    payments: &Payments,
    attempt_id: Uuid,
    return_path: &str,
) -> Result<NextAction, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let a = attempt(&mut tx, attempt_id).await?;
    tx.commit().await?;
    if a.status != AttemptStatus::Pending {
        return Err(Error::Conflict {
            code: "attempt_not_pending",
            detail: "this payment attempt is already finished".into(),
        });
    }
    let req = AttemptInit {
        attempt_id,
        amount_minor: a.amount_minor,
        currency: a.currency.clone(),
        return_path: return_path.to_owned(),
    };
    // The provider call runs outside any transaction; the attempt id makes it idempotent.
    let intent = match a.method {
        MethodKind::Fake => {
            payments
                .fake
                .as_ref()
                .ok_or_else(unavailable)?
                .init(&req)
                .await?
        }
        MethodKind::Cod => CodGateway.init(&req).await?,
        MethodKind::Stripe | MethodKind::BankTransfer => return Err(unavailable()),
    };
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    sqlx::query!(
        "UPDATE payment_attempts SET provider_ref = $2, updated_at = now()
         WHERE id = $1 AND provider_ref IS NULL",
        attempt_id,
        intent.provider_ref
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(intent.action)
}

fn unavailable() -> Error {
    Error::Unavailable("this payment method is not available".into())
}

/// A new attempt after a failed one (A10), within the order's payment window. The order must
/// be `pending` with payment `failed` (or `unpaid` without an open attempt).
pub async fn retry(tx: &mut TenantTx, payments: &Payments, order_id: Uuid) -> Result<Uuid, Error> {
    let mut order = orders::lock(tx, order_id).await?;
    let method = MethodKind::parse(&order.payment_method)?;
    if !payments.available(method) {
        return Err(unavailable());
    }
    let open = sqlx::query_scalar!(
        "SELECT id FROM payment_attempts WHERE order_id = $1 AND status = 'pending'",
        order_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let window_open = order.payment_expires_at.is_none_or(|e| e > Utc::now());
    if order.status != "pending" || open.is_some() || !window_open || method.is_cod() {
        return Err(Error::Conflict {
            code: "retry_not_allowed",
            detail: "this order cannot be paid again".into(),
        });
    }
    if order.payment_status == "failed" {
        orders::apply_payment(tx, &mut order, PaymentCommand::Retry, "customer").await?;
    }
    let id = create_attempt(
        tx,
        order_id,
        method,
        order.total_minor,
        &order.currency,
        order.payment_expires_at,
    )
    .await?;
    orders::event(
        tx,
        order_id,
        "payment_attempt_created",
        &json!({ "attempt_id": id, "method": method }),
        "customer",
    )
    .await?;
    Ok(id)
}

/// Applies a provider outcome to an attempt and its order. Idempotent: a repeated outcome is
/// a no-op; a success on an expired attempt or a cancelled order is recorded as a late
/// payment (A10). Returns the attempt afterwards.
pub async fn apply_outcome(
    tx: &mut TenantTx,
    attempt_id: Uuid,
    outcome: Outcome,
    actor: &str,
) -> Result<Attempt, Error> {
    let order_id = attempt(tx, attempt_id).await?.order_id;
    // Attempts change only under the order's lock (placement, retries, expiry, outcomes).
    let mut order = orders::lock(tx, order_id).await?;
    let a = attempt(tx, attempt_id).await?;
    let target = match outcome {
        Outcome::Succeeded => AttemptStatus::Succeeded,
        Outcome::Failed => AttemptStatus::Failed,
    };
    if a.status == target {
        return Ok(a);
    }
    let accepts = match outcome {
        // Money is never refused: a success after expiry is still recorded (A10).
        Outcome::Succeeded => matches!(
            a.status,
            AttemptStatus::Pending | AttemptStatus::Expired | AttemptStatus::Failed
        ),
        Outcome::Failed => a.status == AttemptStatus::Pending,
    };
    if !accepts {
        return Err(Error::Conflict {
            code: "attempt_finished",
            detail: format!("the attempt is already {:?}", a.status).to_lowercase(),
        });
    }
    let status = match target {
        AttemptStatus::Succeeded => "succeeded",
        _ => "failed",
    };
    sqlx::query!(
        "UPDATE payment_attempts SET status = $2, completed_at = now(), updated_at = now()
         WHERE id = $1",
        attempt_id,
        status
    )
    .execute(&mut **tx)
    .await?;
    orders::event(
        tx,
        order.id,
        match outcome {
            Outcome::Succeeded => "payment_succeeded",
            Outcome::Failed => "payment_failed",
        },
        &json!({ "attempt_id": attempt_id, "method": a.method, "amount_minor": a.amount_minor }),
        actor,
    )
    .await?;
    match outcome {
        Outcome::Succeeded => orders::payment_succeeded(tx, &mut order, actor).await?,
        // A late failure (after the payment already moved on) changes nothing else.
        Outcome::Failed if matches!(order.payment_status.as_str(), "unpaid" | "authorized") => {
            orders::apply_payment(tx, &mut order, PaymentCommand::Fail, actor).await?;
        }
        Outcome::Failed => {}
    }
    attempt(tx, attempt_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_signatures_verify_and_expire() {
        let g = FakeGateway::new(b"secret".to_vec());
        let now = Utc::now();
        let body = br#"{"id":"x"}"#;
        let sig = g.sign(body, now).unwrap();
        assert!(g.verify(&sig, body, now).is_ok());
        assert!(g.verify(&sig, br#"{"id":"y"}"#, now).is_err(), "tampered");
        assert!(
            FakeGateway::new(b"other".to_vec())
                .verify(&sig, body, now)
                .is_err(),
            "wrong key"
        );
        assert!(
            g.verify(&sig, body, now + Duration::seconds(301)).is_err(),
            "stale"
        );
        for junk in ["", "t=1", "v1=00", "t=x,v1=zz"] {
            assert!(g.verify(junk, body, now).is_err(), "{junk}");
        }
    }

    #[test]
    fn availability_and_timeouts() {
        let none = Payments::default();
        assert!(!none.available(MethodKind::Fake));
        assert!(none.available(MethodKind::Cod));
        assert!(!none.available(MethodKind::Stripe));
        let fake = Payments {
            fake: Some(FakeGateway::new(b"k".to_vec())),
        };
        assert!(fake.available(MethodKind::Fake));
        assert_eq!(
            MethodKind::BankTransfer.default_timeout_minutes(),
            Some(10_080)
        );
        assert_eq!(MethodKind::Cod.default_timeout_minutes(), None);
        let input = PaymentMethodInput {
            enabled: true,
            name_i18n: I18n::new(),
            timeout_minutes: Some(30),
            position: 0,
        };
        assert!(input.validate(MethodKind::Fake).is_ok());
        assert!(input.validate(MethodKind::Cod).is_err());
    }

    #[tokio::test]
    async fn fake_init_returns_to_the_order_page_only() {
        let g = FakeGateway::new(b"k".to_vec());
        let id = Uuid::nil();
        let mut req = AttemptInit {
            attempt_id: id,
            amount_minor: 100,
            currency: "CZK".into(),
            return_path: format!("/o/{}", "a".repeat(64)),
        };
        let i = g.init(&req).await.unwrap();
        assert_eq!(
            i.action,
            NextAction::Redirect {
                url: format!("/_p/fake-pay/{id}?return=/o/{}", "a".repeat(64))
            }
        );
        req.return_path = "https://evil.example/".into();
        let i = g.init(&req).await.unwrap();
        assert_eq!(
            i.action,
            NextAction::Redirect {
                url: format!("/_p/fake-pay/{id}")
            }
        );
    }
}
