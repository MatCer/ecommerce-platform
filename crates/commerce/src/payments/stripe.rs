//! Stripe Connect (spec §10.4, A10, A11): direct charges on the tenant's connected account,
//! Stripe-hosted onboarding, webhook events and refunds.
//!
//! - Each attempt is one PaymentIntent created on the connected account (`Stripe-Account`)
//!   with the attempt id as idempotency key and `application_fee_amount = total ×
//!   application_fee_bps / 10000` when positive.
//! - Webhooks ([`receive`]): the `Stripe-Signature` is verified over the raw body, the event
//!   is stored in `platform.provider_events` (unique id: a redelivery is a no-op) and a job is
//!   enqueued, all before the 200. [`process_event`] then matches the event to the tenant's
//!   account, its livemode, the object stored on the attempt, currency and amount; only
//!   `payment_intent.succeeded` confirms, and nothing regresses a succeeded attempt.
//! - Local mode (no `STRIPE_SECRET_KEY`): API calls go to stripe-mock and the order page offers
//!   a simulator whose events are signed with the (test) webhook secret and fed to
//!   [`receive`], so the webhook path runs end to end.

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::config::{StripeConfig, StripeMode};
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{AttemptStatus, MethodKind, Outcome};
use crate::audit;
use crate::orders::{self, status::PaymentCommand};

/// Processes one stored provider event (payload `{"id": <provider_events.id>}`).
pub const EVENT_JOB: &str = "payments.provider_event";
/// `Stripe-Signature` timestamps older than this are refused (Stripe's default tolerance).
const TOLERANCE_SECS: i64 = 300;
const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The Stripe API client. Not `Debug`: it holds the secret key and the webhook secret.
#[derive(Clone)]
pub struct Stripe {
    mode: StripeMode,
    api: String,
    secret_key: String,
    publishable_key: Option<String>,
    webhook_secret: Vec<u8>,
    http: reqwest::Client,
}

impl std::fmt::Debug for Stripe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stripe {{ mode: {:?}, .. }}", self.mode)
    }
}

/// A PaymentIntent as far as checkout cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentIntent {
    pub id: String,
    pub client_secret: Option<String>,
}

impl Stripe {
    pub fn new(cfg: &StripeConfig, http: reqwest::Client) -> Self {
        Self {
            mode: cfg.mode,
            api: cfg.api_url.as_str().trim_end_matches('/').to_owned(),
            secret_key: cfg.secret_key.clone(),
            publishable_key: cfg.publishable_key.clone(),
            webhook_secret: cfg.webhook_secret.clone().into_bytes(),
            http,
        }
    }

    pub fn mode(&self) -> StripeMode {
        self.mode
    }

    pub fn simulator(&self) -> bool {
        self.mode == StripeMode::Simulator
    }

    /// Events and accounts of this platform carry this livemode flag.
    pub fn livemode(&self) -> bool {
        self.mode == StripeMode::Live
    }

    pub fn publishable_key(&self) -> Option<&str> {
        self.publishable_key.as_deref()
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        account: Option<&str>,
        idempotency_key: Option<&str>,
        form: &[(String, String)],
    ) -> Result<Value, Error> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.api))
            .basic_auth(&self.secret_key, None::<&str>)
            .timeout(API_TIMEOUT);
        if let Some(a) = account {
            req = req.header("Stripe-Account", a);
        }
        if let Some(k) = idempotency_key {
            req = req.header("Idempotency-Key", k);
        }
        if method == reqwest::Method::POST {
            req = req.form(form);
        }
        let res = req
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("stripe: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res
            .json()
            .await
            .map_err(|e| Error::Unavailable(format!("stripe: {}", e.without_url())))?;
        if !status.is_success() {
            let code = body
                .pointer("/error/code")
                .or_else(|| body.pointer("/error/type"))
                .and_then(Value::as_str)
                .unwrap_or("error");
            tracing::warn!(%status, code, path, "stripe API error");
            // A 4xx (other than a rate limit or an idempotency clash) is a definite refusal;
            // anything else leaves the outcome unknown.
            let definite = status.is_client_error()
                && status != reqwest::StatusCode::TOO_MANY_REQUESTS
                && status != reqwest::StatusCode::CONFLICT;
            return Err(if definite {
                Error::Conflict {
                    code: "provider_rejected",
                    detail: format!("stripe: {code}"),
                }
            } else {
                Error::Unavailable(format!("stripe: {code}"))
            });
        }
        Ok(body)
    }

    fn str_field(v: &Value, field: &str) -> Result<String, Error> {
        v.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Unavailable(format!("stripe: response without {field}")))
    }

    /// A PaymentIntent for an attempt on the connected account (A10: the attempt id is the
    /// idempotency key, so a retried init returns the same intent).
    pub async fn create_intent(
        &self,
        account: &str,
        attempt: &IntentRequest,
    ) -> Result<PaymentIntent, Error> {
        let mut form = vec![
            ("amount".to_owned(), attempt.amount_minor.to_string()),
            ("currency".to_owned(), attempt.currency.to_ascii_lowercase()),
            (
                "automatic_payment_methods[enabled]".to_owned(),
                "true".to_owned(),
            ),
            (
                "metadata[attempt_id]".to_owned(),
                attempt.attempt_id.to_string(),
            ),
            (
                "metadata[order_id]".to_owned(),
                attempt.order_id.to_string(),
            ),
            (
                "metadata[tenant_id]".to_owned(),
                attempt.tenant_id.to_string(),
            ),
            ("description".to_owned(), attempt.description.clone()),
        ];
        if attempt.application_fee_minor > 0 {
            form.push((
                "application_fee_amount".to_owned(),
                attempt.application_fee_minor.to_string(),
            ));
        }
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/payment_intents",
                Some(account),
                Some(&attempt.attempt_id.to_string()),
                &form,
            )
            .await?;
        Ok(PaymentIntent {
            id: Self::str_field(&v, "id")?,
            client_secret: v
                .get("client_secret")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    pub async fn retrieve_intent(&self, account: &str, id: &str) -> Result<PaymentIntent, Error> {
        if !id.starts_with("pi_") || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(Error::Internal("stored PaymentIntent id".into()));
        }
        let v = self
            .call(
                reqwest::Method::GET,
                &format!("/v1/payment_intents/{id}"),
                Some(account),
                None,
                &[],
            )
            .await?;
        Ok(PaymentIntent {
            id: Self::str_field(&v, "id")?,
            client_secret: v
                .get("client_secret")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    /// A connected account for Stripe-hosted onboarding (idempotent per tenant for 24 h).
    async fn create_account(&self, tenant_id: Uuid, country: &str) -> Result<String, Error> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/accounts",
                None,
                Some(&format!("account:{tenant_id}")),
                &[
                    ("type".to_owned(), "standard".to_owned()),
                    ("country".to_owned(), country.to_owned()),
                    ("metadata[tenant_id]".to_owned(), tenant_id.to_string()),
                ],
            )
            .await?;
        Self::str_field(&v, "id")
    }

    async fn account_link(
        &self,
        account: &str,
        refresh_url: &str,
        return_url: &str,
    ) -> Result<String, Error> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/account_links",
                None,
                None,
                &[
                    ("account".to_owned(), account.to_owned()),
                    ("refresh_url".to_owned(), refresh_url.to_owned()),
                    ("return_url".to_owned(), return_url.to_owned()),
                    ("type".to_owned(), "account_onboarding".to_owned()),
                ],
            )
            .await?;
        Self::str_field(&v, "url")
    }

    async fn retrieve_account(&self, account: &str) -> Result<Value, Error> {
        self.call(
            reqwest::Method::GET,
            &format!("/v1/accounts/{account}"),
            None,
            None,
            &[],
        )
        .await
    }

    /// Refunds (part of) a PaymentIntent on the connected account, the application fee
    /// proportionally (A11). `refund_id` is the idempotency key.
    pub async fn refund(
        &self,
        account: &str,
        payment_intent: &str,
        amount_minor: i64,
        refund_id: Uuid,
    ) -> Result<(String, String), Error> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/v1/refunds",
                Some(account),
                Some(&refund_id.to_string()),
                &[
                    ("payment_intent".to_owned(), payment_intent.to_owned()),
                    ("amount".to_owned(), amount_minor.to_string()),
                    ("refund_application_fee".to_owned(), "true".to_owned()),
                    ("metadata[refund_id]".to_owned(), refund_id.to_string()),
                ],
            )
            .await?;
        Ok((Self::str_field(&v, "id")?, Self::str_field(&v, "status")?))
    }

    /// Our refund (by `metadata.refund_id`) among the PaymentIntent's refunds, if Stripe has it.
    pub async fn find_refund(
        &self,
        account: &str,
        payment_intent: &str,
        refund_id: Uuid,
    ) -> Result<Option<(String, String)>, Error> {
        if !payment_intent.starts_with("pi_")
            || !payment_intent
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error::Internal("stored PaymentIntent id".into()));
        }
        let ours = refund_id.to_string();
        // Every page (Stripe lists newest first, 100 per page): absence must be certain before
        // the caller creates the refund again.
        let mut after: Option<String> = None;
        for _ in 0..100 {
            let cursor = after
                .as_deref()
                .map(|a| format!("&starting_after={a}"))
                .unwrap_or_default();
            let list = self
                .call(
                    reqwest::Method::GET,
                    &format!("/v1/refunds?payment_intent={payment_intent}&limit=100{cursor}"),
                    Some(account),
                    None,
                    &[],
                )
                .await?;
            let data = list.get("data").and_then(Value::as_array);
            let found = data
                .into_iter()
                .flatten()
                .find(|r| r.pointer("/metadata/refund_id").and_then(Value::as_str) == Some(&ours));
            if let Some(r) = found {
                return Ok(Some((
                    Self::str_field(r, "id")?,
                    Self::str_field(r, "status")?,
                )));
            }
            let more = list.get("has_more").and_then(Value::as_bool) == Some(true);
            let last = data
                .and_then(|d| d.last())
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
                .filter(|id| id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
            match (more, last) {
                (true, Some(id)) => after = Some(id.to_owned()),
                _ => return Ok(None),
            }
        }
        Err(Error::Unavailable("too many refunds to search".into()))
    }

    // -----------------------------------------------------------------------------------
    // Webhook signatures (`Stripe-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "t.body")>`)

    fn mac(&self, ts: i64, body: &[u8]) -> Result<Hmac<Sha256>, Error> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.webhook_secret)
            .map_err(|e| Error::Internal(format!("webhook key: {e}")))?;
        mac.update(ts.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        Ok(mac)
    }

    /// A `Stripe-Signature` header for `body` (the simulator, tests).
    pub fn sign(&self, body: &[u8], now: DateTime<Utc>) -> Result<String, Error> {
        let ts = now.timestamp();
        Ok(format!(
            "t={ts},v1={}",
            hex::encode(self.mac(ts, body)?.finalize().into_bytes())
        ))
    }

    /// Verifies the header in constant time: any `v1` signature may match (Stripe sends one
    /// per active secret during rotation); the timestamp must be within the tolerance.
    pub fn verify(&self, header: &str, body: &[u8], now: DateTime<Utc>) -> Result<(), Error> {
        let bad = || Error::Unauthorized {
            code: "invalid_signature",
        };
        let mut ts = None;
        let mut sigs = Vec::new();
        for part in header.split(',') {
            match part.trim().split_once('=') {
                Some(("t", v)) => ts = v.parse::<i64>().ok(),
                Some(("v1", v)) => sigs.extend(hex::decode(v).ok()),
                _ => {}
            }
        }
        let ts = ts.ok_or_else(bad)?;
        if sigs.is_empty() || (now.timestamp() - ts).abs() > TOLERANCE_SECS {
            return Err(bad());
        }
        for sig in sigs {
            if self.mac(ts, body)?.verify_slice(&sig).is_ok() {
                return Ok(());
            }
        }
        Err(bad())
    }
}

/// What a PaymentIntent needs.
#[derive(Debug, Clone)]
pub struct IntentRequest {
    pub tenant_id: Uuid,
    pub order_id: Uuid,
    pub attempt_id: Uuid,
    pub amount_minor: i64,
    pub currency: String,
    pub application_fee_minor: i64,
    pub description: String,
}

/// `application_fee_amount = total × bps / 10000` (rounded down, never negative).
pub fn application_fee(total_minor: i64, bps: i32) -> i64 {
    (i128::from(total_minor) * i128::from(bps.clamp(0, 10_000)) / 10_000)
        .try_into()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// Connected account

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct StripeAccount {
    pub account_id: String,
    pub livemode: bool,
    pub charges_enabled: bool,
    pub details_submitted: bool,
    /// `active`, `inactive` or `pending`.
    pub card_payments: String,
    pub disabled_reason: Option<String>,
    /// Stripe is offered at checkout (charges enabled and card payments active).
    pub ready: bool,
    pub updated_at: DateTime<Utc>,
}

pub async fn account(tx: &mut TenantTx) -> Result<Option<StripeAccount>, Error> {
    Ok(sqlx::query!(
        "SELECT account_id, livemode, charges_enabled, details_submitted, card_payments,
                disabled_reason, updated_at
         FROM stripe_accounts"
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|r| StripeAccount {
        ready: r.charges_enabled && r.card_payments == "active",
        account_id: r.account_id,
        livemode: r.livemode,
        charges_enabled: r.charges_enabled,
        details_submitted: r.details_submitted,
        card_payments: r.card_payments,
        disabled_reason: r.disabled_reason,
        updated_at: r.updated_at,
    }))
}

/// The account's state from a Stripe account object (API response or `account.updated`).
async fn store_account_state(tx: &mut TenantTx, object: &Value) -> Result<(), Error> {
    let card = object
        .pointer("/capabilities/card_payments")
        .and_then(Value::as_str)
        .filter(|c| matches!(*c, "active" | "inactive" | "pending"))
        .unwrap_or("inactive");
    sqlx::query!(
        "UPDATE stripe_accounts SET charges_enabled = $1, details_submitted = $2,
             card_payments = $3, disabled_reason = $4, updated_at = now()",
        object
            .get("charges_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        object
            .get("details_submitted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        card,
        object
            .pointer("/requirements/disabled_reason")
            .and_then(Value::as_str)
            .map(|s| s.chars().take(200).collect::<String>()),
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Starts (or continues) Stripe-hosted onboarding: creates the connected account on first use
/// and returns the onboarding link. In simulator mode the link is `return_url` and the
/// "completed onboarding" arrives as a signed `account.updated` event through [`receive`].
pub async fn start_onboarding(
    db: &sqlx::PgPool,
    stripe: &Stripe,
    tenant_id: Uuid,
    actor: &str,
    return_url: &str,
    refresh_url: &str,
) -> Result<String, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let existing = account(&mut tx).await?;
    let country = sqlx::query_scalar!(
        "SELECT country_codes[1] AS \"country!\" FROM markets ORDER BY is_default DESC, created_at LIMIT 1"
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_else(|| "CZ".into());
    tx.commit().await?;
    let account_id = match existing {
        Some(a) => a.account_id,
        None => {
            let id = stripe.create_account(tenant_id, &country).await?;
            if !id.starts_with("acct_") {
                return Err(Error::Unavailable("stripe: unexpected account id".into()));
            }
            let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
            sqlx::query!(
                "INSERT INTO stripe_accounts (tenant_id, account_id, livemode) VALUES ($1, $2, $3)
                 ON CONFLICT (tenant_id) DO NOTHING",
                tenant_id,
                id,
                stripe.livemode()
            )
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                actor,
                "stripe_account.created",
                "stripe_account",
                Some(&id),
                &json!({ "livemode": stripe.livemode() }),
            )
            .await?;
            let stored = account(&mut tx).await?.ok_or(Error::NotFound)?;
            tx.commit().await?;
            stored.account_id
        }
    };
    if stripe.simulator() {
        simulate_account(db, stripe, &account_id, true).await?;
        return Ok(return_url.to_owned());
    }
    stripe
        .account_link(&account_id, refresh_url, return_url)
        .await
}

/// Re-reads the account from Stripe (after the onboarding return). A no-op in simulator mode,
/// where stripe-mock's fixture would overwrite the simulated state.
pub async fn refresh_account(
    db: &sqlx::PgPool,
    stripe: &Stripe,
    tenant_id: Uuid,
) -> Result<Option<StripeAccount>, Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let Some(existing) = account(&mut tx).await? else {
        return Ok(None);
    };
    tx.commit().await?;
    if !stripe.simulator() {
        let object = stripe.retrieve_account(&existing.account_id).await?;
        let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
        store_account_state(&mut tx, &object).await?;
        tx.commit().await?;
    }
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let a = account(&mut tx).await?;
    tx.commit().await?;
    Ok(a)
}

// ---------------------------------------------------------------------------------------
// Webhooks

#[derive(Deserialize)]
struct Envelope {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    account: Option<String>,
    livemode: bool,
}

/// Verifies and stores an event, then enqueues its processing (A11). Returns `false` for a
/// redelivery of an event already stored.
pub async fn receive(
    db: &sqlx::PgPool,
    stripe: &Stripe,
    signature: &str,
    raw: &[u8],
) -> Result<bool, Error> {
    stripe.verify(signature, raw, Utc::now())?;
    let payload: Value = serde_json::from_slice(raw).map_err(|_| Error::Validation {
        code: "invalid_event",
        detail: "the event is not JSON".into(),
    })?;
    let env: Envelope = serde_json::from_value(payload.clone()).map_err(|_| Error::Validation {
        code: "invalid_event",
        detail: "the event has no id, type or livemode".into(),
    })?;
    if env.id.is_empty() || env.id.len() > 255 || env.kind.is_empty() || env.kind.len() > 100 {
        return Err(Error::Validation {
            code: "invalid_event",
            detail: "invalid event id or type".into(),
        });
    }
    let mut tx = db.begin().await?;
    let stored = sqlx::query_scalar!(
        "INSERT INTO platform.provider_events (provider, event_id, type, account, livemode, payload)
         VALUES ('stripe', $1, $2, $3, $4, $5)
         ON CONFLICT (provider, event_id) DO NOTHING
         RETURNING id",
        env.id,
        env.kind,
        env.account.as_deref().map(|a| a.chars().take(255).collect::<String>()),
        env.livemode,
        payload
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = stored else {
        tx.commit().await?;
        return Ok(false);
    };
    let mut job = platform::queue::NewJob::new(EVENT_JOB, json!({ "id": id }));
    job.idempotency_key = Some(format!("provider_event:{id}"));
    platform::queue::enqueue(&mut *tx, &job).await?;
    tx.commit().await?;
    Ok(true)
}

/// What processing decided (stored on the event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Processed {
    Applied(String),
    Ignored(String),
    Rejected(String),
}

impl Processed {
    fn parts(&self) -> (&'static str, &str) {
        match self {
            Self::Applied(d) => ("applied", d),
            Self::Ignored(d) => ("ignored", d),
            Self::Rejected(d) => ("rejected", d),
        }
    }
}

/// Processes a stored event once (A11). Mismatches are recorded as `rejected` and never
/// applied; database errors are returned so the job retries.
pub async fn process_event(db: &sqlx::PgPool, id: Uuid) -> Result<Processed, Error> {
    let ev = sqlx::query!(
        "SELECT type, account, livemode, payload, processed_at, outcome, detail
         FROM platform.provider_events WHERE id = $1",
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound)?;
    if ev.processed_at.is_some() {
        let d = ev.detail.unwrap_or_default();
        return Ok(match ev.outcome.as_deref() {
            Some("applied") => Processed::Applied(d),
            Some("rejected") => Processed::Rejected(d),
            _ => Processed::Ignored(d),
        });
    }
    let tenant = match &ev.account {
        Some(a) => {
            sqlx::query_scalar!("SELECT platform.stripe_account_tenant($1)", a)
                .fetch_one(db)
                .await?
        }
        None => None,
    };
    let Some(tenant_id) = tenant else {
        let outcome = Processed::Rejected("no connected account of a tenant".into());
        let mut t = db.begin().await?;
        finish(&mut t, id, None, &outcome).await?;
        t.commit().await?;
        return Ok(outcome);
    };
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    // Serializes concurrent deliveries of the same stored event.
    let open = sqlx::query_scalar!(
        "SELECT processed_at IS NULL AS \"open!\" FROM platform.provider_events
         WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !open {
        tx.commit().await?;
        return Box::pin(process_event(db, id)).await;
    }
    let acc = account(&mut tx).await?.ok_or(Error::NotFound)?;
    let outcome = if Some(acc.account_id.as_str()) != ev.account.as_deref() {
        Processed::Rejected("account mismatch".into())
    } else if acc.livemode != ev.livemode {
        Processed::Rejected("livemode mismatch".into())
    } else {
        let object = ev
            .payload
            .pointer("/data/object")
            .cloned()
            .unwrap_or(Value::Null);
        apply(&mut tx, &ev.r#type, &acc, &object).await?
    };
    if let Processed::Rejected(reason) = &outcome {
        tracing::warn!(event = %id, reason, "stripe event rejected");
    }
    finish(&mut tx, id, Some(tenant_id), &outcome).await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn finish(
    conn: &mut sqlx::PgConnection,
    id: Uuid,
    tenant_id: Option<Uuid>,
    outcome: &Processed,
) -> Result<(), Error> {
    let (kind, detail) = outcome.parts();
    sqlx::query!(
        "UPDATE platform.provider_events SET outcome = $2, detail = $3, tenant_id = $4,
             processed_at = now()
         WHERE id = $1",
        id,
        kind,
        detail.chars().take(500).collect::<String>(),
        tenant_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

fn text<'a>(v: &'a Value, field: &str) -> Option<&'a str> {
    v.get(field).and_then(Value::as_str)
}

async fn apply(
    tx: &mut TenantTx,
    kind: &str,
    acc: &StripeAccount,
    object: &Value,
) -> Result<Processed, Error> {
    match kind {
        "payment_intent.succeeded"
        | "payment_intent.payment_failed"
        | "payment_intent.canceled" => apply_intent(tx, kind, object).await,
        "charge.refunded" => apply_refund(tx, object).await,
        "refund.created" | "refund.updated" | "charge.refund.updated" => {
            apply_refund_object(tx, object).await
        }
        "account.updated" => {
            if text(object, "object") != Some("account")
                || text(object, "id") != Some(acc.account_id.as_str())
            {
                return Ok(Processed::Rejected("account object mismatch".into()));
            }
            let before = acc.ready;
            store_account_state(tx, object).await?;
            let after = account(tx).await?.is_some_and(|a| a.ready);
            if before && !after {
                tracing::warn!(account = %acc.account_id, "stripe capability lost: hidden at checkout");
            }
            audit::record(
                tx,
                "stripe",
                "stripe_account.updated",
                "stripe_account",
                Some(&acc.account_id),
                &json!({ "ready": after, "was_ready": before }),
            )
            .await?;
            Ok(Processed::Applied(format!("account ready: {after}")))
        }
        other => Ok(Processed::Ignored(format!("unhandled type {other}"))),
    }
}

async fn apply_intent(tx: &mut TenantTx, kind: &str, pi: &Value) -> Result<Processed, Error> {
    if text(pi, "object") != Some("payment_intent") {
        return Ok(Processed::Rejected("not a payment_intent".into()));
    }
    let Some(attempt_id) = pi
        .pointer("/metadata/attempt_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok())
    else {
        return Ok(Processed::Rejected("no attempt in metadata".into()));
    };
    let a = match super::attempt(tx, attempt_id).await {
        Ok(a) => a,
        Err(Error::NotFound) => return Ok(Processed::Rejected("unknown attempt".into())),
        Err(e) => return Err(e),
    };
    if a.method != MethodKind::Stripe {
        return Ok(Processed::Rejected("not a Stripe attempt".into()));
    }
    let Some(stored) = a.provider_ref.as_deref() else {
        // The intent id is stored right after Stripe creates it, before the customer can pay;
        // an event racing that write is retried (the job backs off) instead of being lost.
        return Err(Error::Conflict {
            code: "provider_ref_pending",
            detail: "the attempt does not know its payment intent yet".into(),
        });
    };
    if Some(stored) != text(pi, "id") {
        return Ok(Processed::Rejected("payment intent mismatch".into()));
    }
    if !text(pi, "currency").is_some_and(|c| c.eq_ignore_ascii_case(&a.currency)) {
        return Ok(Processed::Rejected("currency mismatch".into()));
    }
    if pi.get("amount").and_then(Value::as_i64) != Some(a.amount_minor) {
        return Ok(Processed::Rejected("amount mismatch".into()));
    }
    let outcome = if kind == "payment_intent.succeeded" {
        if pi
            .get("amount_received")
            .and_then(Value::as_i64)
            .is_some_and(|r| r != a.amount_minor)
        {
            return Ok(Processed::Rejected("amount received mismatch".into()));
        }
        Outcome::Succeeded
    } else {
        // Out of order: a failure never regresses a success (A11).
        if a.status != AttemptStatus::Pending {
            return Ok(Processed::Ignored(
                format!("attempt already {:?}", a.status).to_lowercase(),
            ));
        }
        Outcome::Failed
    };
    super::apply_outcome(tx, attempt_id, outcome, "stripe").await?;
    Ok(Processed::Applied(format!("{kind} → attempt {attempt_id}")))
}

async fn apply_refund(tx: &mut TenantTx, charge: &Value) -> Result<Processed, Error> {
    if text(charge, "object") != Some("charge") {
        return Ok(Processed::Rejected("not a charge".into()));
    }
    let Some(pi) = text(charge, "payment_intent") else {
        return Ok(Processed::Rejected("charge without payment intent".into()));
    };
    let Some(a) = sqlx::query!(
        "SELECT id, order_id, amount_minor, currency FROM payment_attempts
         WHERE method = 'stripe' AND provider_ref = $1 AND status = 'succeeded'
         ORDER BY created_at DESC LIMIT 1",
        pi
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Err(Error::Conflict {
            code: "payment_pending",
            detail: "no succeeded attempt for the charge yet".into(),
        });
    };
    if !text(charge, "currency").is_some_and(|c| c.eq_ignore_ascii_case(&a.currency)) {
        return Ok(Processed::Rejected("currency mismatch".into()));
    }
    let refunded = charge
        .get("amount_refunded")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if refunded <= 0 || refunded > a.amount_minor {
        return Ok(Processed::Rejected("refunded amount out of range".into()));
    }
    if !super::is_retained(tx, a.id).await? {
        return Ok(Processed::Ignored(
            "refund of a duplicate or late payment: the order keeps its payment".into(),
        ));
    }
    let full = refunded == a.amount_minor;
    let mut order = orders::lock(tx, a.order_id).await?;
    let target = if full {
        "refunded"
    } else {
        "partially_refunded"
    };
    if order.payment_status == target && !full {
        // A further partial refund keeps the state; nothing to record.
        return Ok(Processed::Ignored("already partially refunded".into()));
    }
    if order.payment_status == "refunded" {
        return Ok(Processed::Ignored("already refunded".into()));
    }
    orders::apply_payment(tx, &mut order, PaymentCommand::Refund { full }, "stripe").await?;
    platform::queue::publish(
        &mut **tx,
        orders::REFUNDED_EVENT,
        &json!({ "order_id": order.id, "number": order.number.to_string(),
                 "total_minor": order.total_minor, "currency": order.currency,
                 "refunded_minor": refunded, "full": full }),
    )
    .await?;
    Ok(Processed::Applied(format!(
        "refunded {refunded} of {}",
        a.amount_minor
    )))
}

/// A Stripe refund object (`refund.*` events): our refund by `metadata.refund_id` (or its
/// provider id) moves to Stripe's state; a refund made elsewhere (the Stripe dashboard) enters
/// the ledger. The order's payment state then follows the ledger.
async fn apply_refund_object(tx: &mut TenantTx, re: &Value) -> Result<Processed, Error> {
    if text(re, "object") != Some("refund") {
        return Ok(Processed::Rejected("not a refund".into()));
    }
    let (Some(re_id), Some(pi)) = (text(re, "id"), text(re, "payment_intent")) else {
        return Ok(Processed::Rejected(
            "refund without id or payment intent".into(),
        ));
    };
    let Some(amount) = re.get("amount").and_then(Value::as_i64).filter(|a| *a > 0) else {
        return Ok(Processed::Rejected("refund without amount".into()));
    };
    let status = super::RefundStatus::from_stripe(text(re, "status").unwrap_or("pending"));
    let Some(a) = sqlx::query!(
        "SELECT id, order_id, currency FROM payment_attempts
         WHERE method = 'stripe' AND provider_ref = $1 AND status = 'succeeded'
         ORDER BY created_at DESC LIMIT 1",
        pi
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        // The payment's success may not be processed yet (events arrive in any order): retry.
        return Err(Error::Conflict {
            code: "payment_pending",
            detail: "no succeeded attempt for the refund yet".into(),
        });
    };
    if !text(re, "currency").is_some_and(|c| c.eq_ignore_ascii_case(&a.currency)) {
        return Ok(Processed::Rejected("currency mismatch".into()));
    }
    let mut order = orders::lock(tx, a.order_id).await?;
    let ours = re
        .pointer("/metadata/refund_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    let existing = sqlx::query_scalar!(
        "SELECT id FROM refunds WHERE attempt_id = $3 AND (id = $1 OR provider_ref = $2)",
        ours,
        re_id,
        a.id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let detail = match existing {
        Some(id) => {
            // Events arrive in any order: a terminal state never goes back to pending, and the
            // only terminal change Stripe makes is succeeded → failed.
            sqlx::query!(
                "UPDATE refunds SET status = $2, provider_ref = $3, updated_at = now()
                 WHERE id = $1
                   AND (status = 'pending' OR (status = 'succeeded' AND $2 = 'failed'))",
                id,
                match status {
                    super::RefundStatus::Pending => "pending",
                    super::RefundStatus::Succeeded => "succeeded",
                    super::RefundStatus::Failed => "failed",
                },
                re_id
            )
            .execute(&mut **tx)
            .await?;
            format!("refund {id} {status:?}").to_lowercase()
        }
        None => {
            let id = crate::id::new_id();
            sqlx::query!(
                "INSERT INTO refunds (id, tenant_id, order_id, attempt_id, amount_minor, currency,
                     reason, status, provider_ref, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6, 'made in Stripe', $7, $8, 'stripe')",
                id,
                tx.tenant_id(),
                a.order_id,
                a.id,
                amount,
                a.currency,
                match status {
                    super::RefundStatus::Pending => "pending",
                    super::RefundStatus::Succeeded => "succeeded",
                    super::RefundStatus::Failed => "failed",
                },
                re_id
            )
            .execute(&mut **tx)
            .await?;
            format!("external refund {re_id} recorded")
        }
    };
    super::settle_refunds(tx, &mut order, "stripe").await?;
    Ok(Processed::Applied(detail))
}

// ---------------------------------------------------------------------------------------
// Simulator (stripe-mock mode only)

fn simulated(event_id: String, kind: &str, account: &str, object: Value) -> Value {
    json!({
        "id": event_id,
        "object": "event",
        "api_version": "2024-06-20",
        "created": Utc::now().timestamp(),
        "type": kind,
        "livemode": false,
        "account": account,
        "data": { "object": object },
    })
}

async fn deliver(db: &sqlx::PgPool, stripe: &Stripe, event: &Value) -> Result<bool, Error> {
    if !stripe.simulator() {
        return Err(Error::NotFound);
    }
    let raw = serde_json::to_vec(event).map_err(|e| Error::Internal(e.to_string()))?;
    let signature = stripe.sign(&raw, Utc::now())?;
    receive(db, stripe, &signature, &raw).await
}

/// Simulates the customer paying (or failing) an attempt: a Stripe-shaped
/// `payment_intent.succeeded` / `payment_intent.payment_failed` event, signed with the webhook
/// secret and received like a real one. The event id is derived from the attempt and outcome,
/// so pressing the button twice is one event.
pub async fn simulate_payment(
    db: &sqlx::PgPool,
    stripe: &Stripe,
    tenant_id: Uuid,
    attempt_id: Uuid,
    outcome: Outcome,
) -> Result<(), Error> {
    let mut tx = platform::db::tenant_tx(db, tenant_id).await?;
    let a = super::attempt(&mut tx, attempt_id).await?;
    let acc = account(&mut tx).await?.ok_or(Error::NotFound)?;
    tx.commit().await?;
    let pi = a.provider_ref.clone().ok_or_else(|| Error::Conflict {
        code: "attempt_not_initialized",
        detail: "the payment was not started at Stripe yet".into(),
    })?;
    if a.method != MethodKind::Stripe {
        return Err(Error::NotFound);
    }
    let (kind, status, suffix) = match outcome {
        Outcome::Succeeded => ("payment_intent.succeeded", "succeeded", "succeeded"),
        Outcome::Failed => (
            "payment_intent.payment_failed",
            "requires_payment_method",
            "failed",
        ),
    };
    let object = json!({
        "id": pi,
        "object": "payment_intent",
        "amount": a.amount_minor,
        "amount_received": if outcome == Outcome::Succeeded { a.amount_minor } else { 0 },
        "currency": a.currency.to_ascii_lowercase(),
        "status": status,
        "livemode": false,
        "metadata": { "attempt_id": a.id, "order_id": a.order_id, "tenant_id": tenant_id },
    });
    let event = simulated(
        format!("evt_sim_{}_{suffix}", a.id.simple()),
        kind,
        &acc.account_id,
        object,
    );
    deliver(db, stripe, &event).await?;
    Ok(())
}

/// Simulates `account.updated`: onboarding completed (`enabled`) or the card_payments
/// capability lost.
pub async fn simulate_account(
    db: &sqlx::PgPool,
    stripe: &Stripe,
    account_id: &str,
    enabled: bool,
) -> Result<(), Error> {
    let state = if enabled { "active" } else { "inactive" };
    let object = json!({
        "id": account_id,
        "object": "account",
        "charges_enabled": enabled,
        "details_submitted": true,
        "capabilities": { "card_payments": state, "transfers": state },
        "requirements": { "disabled_reason": if enabled { Value::Null } else { json!("requirements.past_due") } },
    });
    let event = simulated(
        format!("evt_sim_{}", Uuid::now_v7().simple()),
        "account.updated",
        account_id,
        object,
    );
    deliver(db, stripe, &event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn stripe(mode: StripeMode) -> Stripe {
        Stripe::new(
            &StripeConfig {
                mode,
                api_url: "http://localhost:1/".parse().unwrap(),
                secret_key: "sk_test_x".into(),
                publishable_key: None,
                webhook_secret: "whsec_test_secret_123".into(),
            },
            reqwest::Client::new(),
        )
    }

    #[test]
    fn signatures_use_the_stripe_scheme() {
        let s = stripe(StripeMode::Test);
        let now = Utc::now();
        let body = br#"{"id":"evt_1"}"#;
        let header = s.sign(body, now).unwrap();
        // Stripe's documented scheme: HMAC-SHA256(secret, "<t>.<payload>") as hex.
        let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec_test_secret_123").unwrap();
        mac.update(format!("{}.", now.timestamp()).as_bytes());
        mac.update(body);
        let expected = hex::encode(mac.finalize().into_bytes());
        assert_eq!(header, format!("t={},v1={expected}", now.timestamp()));
        assert!(s.verify(&header, body, now).is_ok());
        // Rotation: another v1 first, the valid one second.
        let rotated = format!(
            "t={},v1={},v1={expected},v0=abc",
            now.timestamp(),
            "00".repeat(32)
        );
        assert!(s.verify(&rotated, body, now).is_ok());
        assert!(
            s.verify(&header, br#"{"id":"evt_2"}"#, now).is_err(),
            "tampered"
        );
        assert!(
            s.verify(&header, body, now + Duration::seconds(301))
                .is_err(),
            "stale"
        );
        assert!(
            s.verify(&header, body, now - Duration::seconds(301))
                .is_err(),
            "future"
        );
        let mut other = stripe(StripeMode::Test);
        other.webhook_secret = b"whsec_other".to_vec();
        assert!(other.verify(&header, body, now).is_err(), "wrong secret");
        for junk in [
            "",
            "t=1",
            "v1=00",
            "t=x,v1=zz",
            &format!("t={}", now.timestamp()),
        ] {
            assert!(s.verify(junk, body, now).is_err(), "{junk}");
        }
    }

    #[test]
    fn application_fees() {
        assert_eq!(application_fee(12_345, 0), 0);
        assert_eq!(application_fee(12_345, 150), 185);
        assert_eq!(application_fee(10_000, 10_000), 10_000);
        assert_eq!(application_fee(10_000, 20_000), 10_000, "clamped");
        assert_eq!(application_fee(10_000, -5), 0);
    }

    #[test]
    fn modes() {
        assert!(stripe(StripeMode::Simulator).simulator());
        assert!(!stripe(StripeMode::Test).livemode());
        assert!(stripe(StripeMode::Live).livemode());
    }
}
