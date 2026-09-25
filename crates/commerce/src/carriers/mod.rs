//! Carrier integrations (spec §10.5): Packeta (REST/XML API) and PPL (CPL API, OAuth client
//! credentials): shipment creation with the label PDF, tracking, and Packeta pickup-point
//! validation. Credentials are per tenant, sealed with `SecretBox` (AAD
//! `carrier:<tenant>:<carrier>`) and never returned by the API.
//!
//! The endpoints are platform configuration (`PACKETA_API_URL`, `PPL_API_URL`), never tenant
//! input, so the plain HTTP client is used (not the SSRF-safe fetcher); locally they point to
//! `apps/mocks`, which serves the same request/response shapes.

pub mod packeta;
pub mod ppl;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::crypto::SecretBox;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

/// Per carrier API call.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(20);
/// Label PDFs larger than this are refused (a label is a few kB).
pub(crate) const MAX_LABEL_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CarrierKind {
    Packeta,
    Ppl,
}

impl CarrierKind {
    pub const ALL: [Self; 2] = [Self::Packeta, Self::Ppl];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Packeta => "packeta",
            Self::Ppl => "ppl",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The account a shipping method's carrier uses (`None`: no carrier, personal pickup).
    pub fn of(method: crate::shipping::Carrier) -> Option<Self> {
        use crate::shipping::Carrier as C;
        match method {
            C::PacketaPickup | C::PacketaHome => Some(Self::Packeta),
            C::Ppl => Some(Self::Ppl),
            C::PersonalPickup => None,
        }
    }
}

/// PPL access tokens per client id, with their expiry.
type TokenCache = HashMap<String, (String, DateTime<Utc>)>;

/// Endpoints and shared state (the PPL token cache).
#[derive(Clone)]
pub struct Carriers {
    pub http: reqwest::Client,
    pub packeta_url: String,
    pub packeta_validate_url: String,
    pub ppl_url: String,
    /// `SECRETS_KEY`; without it no credentials can be stored or used.
    pub secrets: Option<Arc<SecretBox>>,
    ppl_tokens: Arc<Mutex<TokenCache>>,
}

impl std::fmt::Debug for Carriers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Carriers")
            .field("packeta_url", &self.packeta_url)
            .field("ppl_url", &self.ppl_url)
            .field("secrets", &self.secrets.is_some())
            .finish_non_exhaustive()
    }
}

impl Carriers {
    pub fn new(
        http: reqwest::Client,
        packeta_url: String,
        packeta_validate_url: String,
        ppl_url: String,
        secrets: Option<Arc<SecretBox>>,
    ) -> Self {
        Self {
            http,
            packeta_url,
            packeta_validate_url,
            ppl_url: ppl_url.trim_end_matches('/').to_owned(),
            secrets,
            ppl_tokens: Arc::default(),
        }
    }

    fn secrets(&self) -> Result<&SecretBox, Error> {
        self.secrets.as_deref().ok_or_else(|| {
            Error::Unavailable(
                "SECRETS_KEY is not configured: carrier credentials are unusable".into(),
            )
        })
    }

    /// A cached PPL token for these client credentials, or a fresh one.
    pub(crate) fn cached_ppl_token(&self, client_id: &str) -> Option<String> {
        let tokens = self.ppl_tokens.lock().ok()?;
        tokens
            .get(client_id)
            .filter(|(_, until)| *until > Utc::now())
            .map(|(t, _)| t.clone())
    }

    pub(crate) fn store_ppl_token(&self, client_id: &str, token: String, until: DateTime<Utc>) {
        if let Ok(mut tokens) = self.ppl_tokens.lock() {
            // ponytail: unbounded only in theory (one entry per configured PPL account).
            tokens.insert(client_id.to_owned(), (token, until));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Accounts

/// Decrypted credentials (never serialized to clients).
#[derive(Clone, Serialize, Deserialize)]
pub enum Credentials {
    Packeta {
        api_password: String,
    },
    Ppl {
        client_id: String,
        client_secret: String,
    },
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CarrierAccount {
    pub carrier: CarrierKind,
    pub configured: bool,
    /// Packeta: the public widget/API key (first 16 characters of the API password).
    pub public_key: Option<String>,
    /// The sender name on labels (Packeta `eshop`).
    pub sender_label: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Credentials to store. Packeta: `api_password`; PPL: `client_id` + `client_secret`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CarrierAccountInput {
    #[serde(default)]
    pub api_password: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub sender_label: String,
}

fn aad(tenant: Uuid, carrier: CarrierKind) -> Vec<u8> {
    format!("carrier:{tenant}:{}", carrier.as_str()).into_bytes()
}

pub async fn accounts(tx: &mut TenantTx) -> Result<Vec<CarrierAccount>, Error> {
    let rows =
        sqlx::query!("SELECT carrier, public_key, sender_label, updated_at FROM carrier_accounts")
            .fetch_all(&mut **tx)
            .await?;
    Ok(CarrierKind::ALL
        .into_iter()
        .map(|c| {
            let row = rows.iter().find(|r| r.carrier == c.as_str());
            CarrierAccount {
                carrier: c,
                configured: row.is_some(),
                public_key: row.and_then(|r| r.public_key.clone()),
                sender_label: row.map(|r| r.sender_label.clone()),
                updated_at: row.map(|r| r.updated_at),
            }
        })
        .collect())
}

/// Stores (replaces) a carrier's credentials (audited; the secret itself never is).
pub async fn configure(
    tx: &mut TenantTx,
    carriers: &Carriers,
    actor: &str,
    carrier: CarrierKind,
    input: &CarrierAccountInput,
) -> Result<CarrierAccount, Error> {
    let label = input.sender_label.trim();
    if label.is_empty() || label.chars().count() > 100 {
        return Err(invalid(
            "invalid_carrier_account",
            "sender_label: 1-100 characters",
        ));
    }
    let (creds, public_key) = match carrier {
        CarrierKind::Packeta => {
            let pw = input.api_password.as_deref().unwrap_or_default().trim();
            if pw.len() != 32 || !pw.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid(
                    "invalid_carrier_account",
                    "api_password: the 32-character Packeta API password",
                ));
            }
            let pw = pw.to_ascii_lowercase();
            (
                Credentials::Packeta {
                    api_password: pw.clone(),
                },
                Some(pw[..16].to_owned()),
            )
        }
        CarrierKind::Ppl => {
            let id = input.client_id.as_deref().unwrap_or_default().trim();
            let secret = input.client_secret.as_deref().unwrap_or_default().trim();
            let id_ok = (3..=100).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if !id_ok || !(8..=500).contains(&secret.len()) {
                return Err(invalid(
                    "invalid_carrier_account",
                    "client_id and client_secret of the PPL CPL API are required",
                ));
            }
            (
                Credentials::Ppl {
                    client_id: id.to_owned(),
                    client_secret: secret.to_owned(),
                },
                None,
            )
        }
    };
    let plain = serde_json::to_vec(&creds).map_err(|e| Error::Internal(e.to_string()))?;
    let sealed = carriers
        .secrets()?
        .seal(&plain, &aad(tx.tenant_id(), carrier));
    sqlx::query!(
        "INSERT INTO carrier_accounts (tenant_id, carrier, credentials, public_key, sender_label,
             updated_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (tenant_id, carrier) DO UPDATE SET credentials = EXCLUDED.credentials,
             public_key = EXCLUDED.public_key, sender_label = EXCLUDED.sender_label,
             updated_by = EXCLUDED.updated_by, updated_at = now()",
        tx.tenant_id(),
        carrier.as_str(),
        sealed,
        public_key,
        label,
        actor
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "carrier_account.updated",
        "carrier_account",
        Some(carrier.as_str()),
        &json!({ "carrier": carrier, "sender_label": label }),
    )
    .await?;
    accounts(tx)
        .await?
        .into_iter()
        .find(|a| a.carrier == carrier)
        .ok_or(Error::NotFound)
}

/// Removes a carrier's credentials (audited).
pub async fn remove(tx: &mut TenantTx, actor: &str, carrier: CarrierKind) -> Result<(), Error> {
    sqlx::query!(
        "DELETE FROM carrier_accounts WHERE carrier = $1",
        carrier.as_str()
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "carrier_account.removed",
        "carrier_account",
        Some(carrier.as_str()),
        &json!({ "carrier": carrier }),
    )
    .await?;
    Ok(())
}

/// A carrier account ready to call: credentials + sender label.
#[derive(Debug, Clone)]
pub struct Account {
    pub carrier: CarrierKind,
    pub credentials: Credentials,
    pub sender_label: String,
}

/// The tenant's account for `carrier`. `409 carrier_not_configured` without one.
pub async fn account(
    tx: &mut TenantTx,
    carriers: &Carriers,
    carrier: CarrierKind,
) -> Result<Account, Error> {
    let row = sqlx::query!(
        "SELECT credentials, sender_label FROM carrier_accounts WHERE carrier = $1",
        carrier.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| Error::Conflict {
        code: "carrier_not_configured",
        detail: format!("set up the {} account first", carrier.as_str()),
    })?;
    let plain = carriers
        .secrets()?
        .open(&row.credentials, &aad(tx.tenant_id(), carrier))
        .map_err(|e| Error::Internal(e.to_string()))?;
    let credentials: Credentials =
        serde_json::from_slice(&plain).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(Account {
        carrier,
        credentials,
        sender_label: row.sender_label,
    })
}

/// The tenant's Packeta widget key, if configured.
pub async fn packeta_public_key(tx: &mut TenantTx) -> Result<Option<String>, Error> {
    Ok(
        sqlx::query_scalar!("SELECT public_key FROM carrier_accounts WHERE carrier = 'packeta'")
            .fetch_optional(&mut **tx)
            .await?
            .flatten(),
    )
}

// ---------------------------------------------------------------------------------------
// Shipments and tracking

/// What a carrier needs to create a shipment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipmentRequest {
    /// The order number (the carrier's reference, the COD variable symbol).
    pub reference: String,
    pub recipient_name: String,
    pub company: Option<String>,
    pub email: String,
    pub phone: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    /// ISO alpha-2.
    pub country: String,
    /// Packeta pickup point id (pickup delivery).
    pub pickup_point: Option<String>,
    /// Cash on delivery amount (minor units, order currency).
    pub cod_minor: Option<i64>,
    pub value_minor: i64,
    pub currency: String,
    pub weight_g: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub carrier_ref: String,
    pub tracking_number: String,
    pub tracking_url: String,
    pub label_pdf: Vec<u8>,
}

/// A carrier's tracking state mapped onto ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tracked {
    /// Data received, not handed over yet.
    Announced,
    /// The carrier has the parcel.
    InTransit,
    Delivered,
    /// On the way back to the sender, or back.
    Returned,
    Cancelled,
}

impl Carriers {
    pub async fn create(
        &self,
        account: &Account,
        method: crate::shipping::Carrier,
        req: &ShipmentRequest,
    ) -> Result<Created, Error> {
        match &account.credentials {
            Credentials::Packeta { api_password } => {
                packeta::create(self, api_password, &account.sender_label, method, req).await
            }
            Credentials::Ppl {
                client_id,
                client_secret,
            } => ppl::create(self, client_id, client_secret, req).await,
        }
    }

    /// The parcel's state: ours + the carrier's text.
    pub async fn track(
        &self,
        account: &Account,
        carrier_ref: &str,
    ) -> Result<(Tracked, String), Error> {
        match &account.credentials {
            Credentials::Packeta { api_password } => {
                packeta::track(self, api_password, carrier_ref).await
            }
            Credentials::Ppl {
                client_id,
                client_secret,
            } => ppl::track(self, client_id, client_secret, carrier_ref).await,
        }
    }
}

/// Carrier kind from a stored name.
pub fn parse_kind(s: &str) -> Result<CarrierKind, Error> {
    CarrierKind::parse(s).ok_or_else(|| Error::Internal(format!("unknown carrier {s}")))
}

/// Minor units as a decimal string (`12900` → `129.00`).
pub(crate) fn decimal(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// Upstream failures: a rejection is the merchant's to fix (`422 carrier_rejected`), anything
/// else is transient (`503`).
pub(crate) fn rejected(carrier: &str, detail: impl Into<String>) -> Error {
    let detail: String = detail.into();
    Error::Validation {
        code: "carrier_rejected",
        detail: format!(
            "{carrier}: {}",
            detail.chars().take(300).collect::<String>()
        ),
    }
}

pub(crate) fn unavailable(carrier: &str, e: impl std::fmt::Display) -> Error {
    Error::Unavailable(format!("{carrier} is unavailable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(decimal(12_900), "129.00");
        assert_eq!(decimal(5), "0.05");
        assert_eq!(decimal(-150), "-1.50");
    }
}
