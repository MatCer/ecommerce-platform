//! Ad-platform forwarders (spec §11.3, §14, A20, A21): Meta Conversions API, GA4 Measurement
//! Protocol, Google Ads (Data Manager API) and Seznam SEM server-to-server.
//!
//! - **Config** ([`list`], [`update`]): one row per tenant and platform: enabled markets,
//!   pause, test mode, non-secret ids ([`Settings`]) and credentials ([`Credentials`], sealed
//!   with the platform key, never returned). Enabling needs a complete configuration.
//! - **Capture** ([`capture_events`], [`capture_purchase`], [`capture_refund`]): only if the
//!   consent records grant `ads` to the subject now (A20; purposes a client claims are never
//!   read). One [`ad_deliveries`] row per platform that takes the event, with a stable
//!   `event_id` shared by the platforms (the vendors' dedupe key), and a job. Rows are
//!   minimized: no email, phone or IP (the order is read and hashed when sending).
//! - **Delivery** ([`deliver`], the worker's [`DELIVER_JOB`]): consent is resolved again right
//!   before sending (the subject still grants `ads` and the customer has not refused it), the
//!   request is built for the vendor ([`vendors`]) and sent through the SSRF-safe client to
//!   fixed vendor hosts (A21), rate-limited per tenant and platform. Retries are the queue's
//!   (A14: backoff with jitter, [`MAX_ATTEMPTS`]); a permanent answer (4xx other than
//!   408/429) or the last failed attempt marks the delivery `dead`. The log keeps the status,
//!   the response code and our own short error, never a payload.
//! - **Withdrawal** ([`cancel_for_subject`]): recording `ads = false` cancels the subject's
//!   open deliveries in the same transaction. A send already in flight completes (its consent
//!   check ran before the withdrawal committed); nothing starts afterwards.
//!
//! [`ad_deliveries`]: Delivery

pub mod normalize;
pub mod vendors;

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use platform::Error;
use platform::crypto::SecretBox;
use platform::db::{TenantTx, tenant_tx};
use platform::http::{Limits, SafeClient};
use platform::queue::{self, NewJob};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::analytics::CleanEvent;
use crate::audit;
use crate::consent::{self, ConsentPurpose, Subject};
use crate::storefront::PublicUrls;
use vendors::{BuildError, Endpoints};

/// One delivery attempt (payload `{delivery_id}`).
pub const DELIVER_JOB: &str = "adtracking.deliver";
/// An outbox `order.refunded` → refund deliveries (payload: the outbox event).
pub const REFUND_JOB: &str = "adtracking.refund";
/// Queue attempts per delivery (backoff 5 s doubling, capped at an hour: about 2.5 hours).
pub const MAX_ATTEMPTS: i32 = 12;
const PAGE_MAX: i64 = 100;
/// A21 caps for vendor calls: 10 s, the answer read up to 256 kB.
const LIMITS: Limits = Limits {
    max_bytes: 256 * 1024,
    timeout: Duration::from_secs(10),
};

/// The ad platforms.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = AdPlatform)]
pub enum Platform {
    Meta,
    Ga4,
    GoogleAds,
    Sklik,
}

impl Platform {
    pub const ALL: [Self; 4] = [Self::Meta, Self::Ga4, Self::GoogleAds, Self::Sklik];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Meta => "meta",
            Self::Ga4 => "ga4",
            Self::GoogleAds => "google_ads",
            Self::Sklik => "sklik",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Our event names the platform takes.
    pub fn events(self) -> Vec<&'static str> {
        EVENTS
            .into_iter()
            .filter(|e| vendors::event_name(self, e).is_some())
            .collect()
    }

    fn setting_fields(self) -> &'static [&'static str] {
        match self {
            Self::Meta => &["pixel_id", "test_event_code"],
            Self::Ga4 => &["measurement_id"],
            Self::GoogleAds => &["customer_id", "conversion_action_id", "login_customer_id"],
            Self::Sklik => &["sem_id"],
        }
    }

    fn required_settings(self, test_mode: bool) -> &'static [&'static str] {
        match self {
            Self::Meta if test_mode => &["pixel_id", "test_event_code"],
            Self::Meta => &["pixel_id"],
            Self::Ga4 => &["measurement_id"],
            Self::GoogleAds => &["customer_id", "conversion_action_id"],
            Self::Sklik => &["sem_id"],
        }
    }

    fn credential_fields(self) -> &'static [&'static str] {
        match self {
            Self::Meta => &["access_token"],
            Self::Ga4 => &["api_secret"],
            Self::GoogleAds => &["client_id", "client_secret", "refresh_token"],
            Self::Sklik => &[],
        }
    }

    /// Requests per second per tenant (vendor limits are far higher; this keeps one tenant's
    /// backlog from hammering a vendor after an outage).
    fn per_second(self) -> u32 {
        match self {
            Self::Meta | Self::Ga4 => 20,
            Self::GoogleAds => 5,
            Self::Sklik => 10,
        }
    }

    /// Vendors reject events older than this (Meta 7 days, GA4 72 hours).
    fn max_age(self) -> chrono::Duration {
        match self {
            Self::Meta | Self::Sklik => chrono::Duration::days(7),
            Self::Ga4 => chrono::Duration::hours(72),
            Self::GoogleAds => chrono::Duration::days(60),
        }
    }

    /// Known limits the admin shows next to the platform (codes, translated in the admin).
    fn notices(self) -> Vec<String> {
        match self {
            Self::Sklik => vec![
                "sklik_no_browser_ids".into(),
                "sklik_czk_only".into(),
                "sklik_no_test_channel".into(),
            ],
            Self::GoogleAds => vec!["google_ads_purchases_only".into()],
            Self::Meta | Self::Ga4 => vec![],
        }
    }
}

/// Our event names (a subset of the analytics event types, plus `refund`).
pub const EVENTS: [&str; 6] = [
    "page_view",
    "view_item",
    "add_to_cart",
    "begin_checkout",
    "purchase",
    "refund",
];

/// Non-secret ids. Only the fields of the platform are accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AdPlatformSettings)]
pub struct Settings {
    /// Meta: the dataset (pixel) id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_id: Option<String>,
    /// Meta: the Events Manager test code used in test mode (`TEST12345`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_event_code: Option<String>,
    /// GA4: `G-XXXXXXX`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement_id: Option<String>,
    /// Google Ads: the account the conversions belong to (10 digits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    /// Google Ads: the conversion action (type "import from clicks").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversion_action_id: Option<String>,
    /// Google Ads: the manager account used to sign in, when not the account itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_customer_id: Option<String>,
    /// Seznam: the server-to-server SEM id (differs from the browser SEM id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sem_id: Option<String>,
}

impl Settings {
    fn get(&self, field: &str) -> Option<&String> {
        match field {
            "pixel_id" => self.pixel_id.as_ref(),
            "test_event_code" => self.test_event_code.as_ref(),
            "measurement_id" => self.measurement_id.as_ref(),
            "customer_id" => self.customer_id.as_ref(),
            "conversion_action_id" => self.conversion_action_id.as_ref(),
            "login_customer_id" => self.login_customer_id.as_ref(),
            "sem_id" => self.sem_id.as_ref(),
            _ => None,
        }
    }

    fn present(&self) -> Vec<&'static str> {
        [
            "pixel_id",
            "test_event_code",
            "measurement_id",
            "customer_id",
            "conversion_action_id",
            "login_customer_id",
            "sem_id",
        ]
        .into_iter()
        .filter(|f| self.get(f).is_some())
        .collect()
    }
}

/// Secrets. Write-only: merged into the stored ones (a field left out keeps its value).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AdPlatformCredentials)]
pub struct Credentials {
    /// Meta: a Conversions API access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// GA4: a Measurement Protocol API secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_secret: Option<String>,
    /// Google: OAuth client id, client secret and a refresh token with the
    /// `https://www.googleapis.com/auth/datamanager` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

impl Credentials {
    fn get(&self, field: &str) -> Option<&String> {
        match field {
            "access_token" => self.access_token.as_ref(),
            "api_secret" => self.api_secret.as_ref(),
            "client_id" => self.client_id.as_ref(),
            "client_secret" => self.client_secret.as_ref(),
            "refresh_token" => self.refresh_token.as_ref(),
            _ => None,
        }
    }

    fn present(&self) -> Vec<&'static str> {
        [
            "access_token",
            "api_secret",
            "client_id",
            "client_secret",
            "refresh_token",
        ]
        .into_iter()
        .filter(|f| self.get(f).is_some())
        .collect()
    }

    fn merged(mut self, new: Self) -> Self {
        self.access_token = new.access_token.or(self.access_token);
        self.api_secret = new.api_secret.or(self.api_secret);
        self.client_id = new.client_id.or(self.client_id);
        self.client_secret = new.client_secret.or(self.client_secret);
        self.refresh_token = new.refresh_token.or(self.refresh_token);
        self
    }

    /// The last characters of the main secret, to tell credentials apart.
    fn hint(&self) -> Option<String> {
        let main = self
            .access_token
            .as_ref()
            .or(self.api_secret.as_ref())
            .or(self.refresh_token.as_ref())?;
        let chars: Vec<char> = main.chars().collect();
        Some(chars[chars.len().saturating_sub(4)..].iter().collect())
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = AdPlatformConfig)]
pub struct PlatformConfig {
    pub platform: Platform,
    pub enabled: bool,
    /// Paused: events are kept (`paused`) and sent after resuming.
    pub paused: bool,
    /// The vendor's test channel (Meta test code, GA4 validation server, Google Ads
    /// validate-only); Seznam has none, so nothing is sent.
    pub test_mode: bool,
    /// Markets whose events are forwarded.
    pub market_ids: Vec<Uuid>,
    pub settings: Settings,
    pub has_credentials: bool,
    pub credentials_hint: Option<String>,
    /// Settings and credential fields the platform uses.
    pub setting_fields: Vec<String>,
    pub credential_fields: Vec<String>,
    /// Our event names the platform takes.
    pub events: Vec<String>,
    /// Settings and credentials are complete (the platform can be enabled).
    pub complete: bool,
    /// Known limits, as codes: `sklik_no_browser_ids`, `sklik_czk_only`,
    /// `sklik_no_test_channel`, `google_ads_purchases_only`.
    pub notices: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = AdPlatformList)]
pub struct PlatformList {
    pub items: Vec<PlatformConfig>,
}

/// A partial update; absent fields keep their value.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = AdPlatformUpdate)]
pub struct PlatformUpdate {
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub test_mode: Option<bool>,
    pub market_ids: Option<Vec<Uuid>>,
    /// Replaces the settings.
    pub settings: Option<Settings>,
    /// Merged into the stored credentials.
    pub credentials: Option<Credentials>,
}

impl PlatformUpdate {
    /// Changes where data goes or what it is sent with (needs a fresh sign-in, A9).
    pub fn is_sensitive(&self) -> bool {
        self.credentials.is_some()
            || self.settings.is_some()
            || self.market_ids.is_some()
            || self.enabled == Some(true)
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = AdDelivery)]
pub struct Delivery {
    pub id: Uuid,
    pub platform: Platform,
    /// Our event name (`purchase`, `view_item`, ...).
    pub event_name: String,
    /// The dedupe key sent to the vendor.
    pub event_id: Uuid,
    pub order_id: Option<Uuid>,
    /// `pending`, `retrying`, `paused`, `sending`, `succeeded`, `dead`, `cancelled` or `skipped`.
    pub status: String,
    pub attempts: i32,
    pub response_code: Option<i32>,
    pub last_error: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = AdDeliveryPage)]
pub struct DeliveryPage {
    pub items: Vec<Delivery>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeliveryQuery {
    pub platform: Option<Platform>,
    pub status: Option<String>,
    pub cursor: Option<Uuid>,
    /// 1-100, default 50.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[schema(as = AdConnectionTest)]
pub struct ConnectionTest {
    pub ok: bool,
    /// The vendor's HTTP status, when it answered.
    pub response_code: Option<i32>,
    /// What was checked or what failed (no secrets).
    pub message: String,
}

/// Services for config, capture and delivery: the secret box, the SSRF-safe client, where
/// the vendors are (mocks in dev), the shop URL shape, rate limiters and cached Google access
/// tokens.
#[derive(Clone)]
pub struct AdTracking {
    pub secrets: SecretBox,
    pub http: SafeClient,
    pub endpoints: Endpoints,
    pub urls: PublicUrls,
    limiters: Arc<HashMap<Platform, DefaultKeyedRateLimiter<Uuid>>>,
    /// ponytail: per process; a shared cache when many workers refresh the same tokens.
    tokens: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}

impl AdTracking {
    pub fn new(
        secrets: SecretBox,
        http: SafeClient,
        endpoints: Endpoints,
        urls: PublicUrls,
    ) -> Self {
        let limiters = Platform::ALL
            .into_iter()
            .map(|p| {
                let rate = NonZeroU32::new(p.per_second()).unwrap_or(NonZeroU32::MIN);
                (p, RateLimiter::keyed(Quota::per_second(rate)))
            })
            .collect();
        Self {
            secrets,
            http,
            endpoints,
            urls,
            limiters: Arc::new(limiters),
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// `AD_PLATFORMS_BASE_URL` (honored only with `APP_ENV=dev`) sends every vendor call to
    /// the local mocks.
    pub fn endpoints_from_env() -> Endpoints {
        let base = std::env::var("AD_PLATFORMS_BASE_URL")
            .ok()
            .filter(|b| !b.trim().is_empty());
        let dev = std::env::var("APP_ENV").is_ok_and(|e| e == "dev");
        if base.is_some() && !dev {
            tracing::warn!("AD_PLATFORMS_BASE_URL is ignored outside APP_ENV=dev");
            return Endpoints::default();
        }
        Endpoints { base }
    }
}

fn invalid(code: &'static str, detail: impl Into<String>) -> Error {
    Error::Validation {
        code,
        detail: detail.into(),
    }
}

fn aad(tenant: Uuid, platform: Platform) -> Vec<u8> {
    format!("adplatform:{tenant}:{}", platform.as_str()).into_bytes()
}

fn all_digits(s: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// Validates the settings of `platform` and returns them normalized (trimmed, Google
/// account ids without dashes).
fn check_settings(platform: Platform, s: &Settings) -> Result<Settings, Error> {
    for f in s.present() {
        if !platform.setting_fields().contains(&f) {
            return Err(invalid(
                "invalid_settings",
                format!("{} takes no {f}", platform.as_str()),
            ));
        }
    }
    let t = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let digits = |v: &Option<String>| t(v).map(|v| v.replace('-', ""));
    let out = Settings {
        pixel_id: t(&s.pixel_id),
        test_event_code: t(&s.test_event_code),
        measurement_id: t(&s.measurement_id),
        customer_id: digits(&s.customer_id),
        conversion_action_id: t(&s.conversion_action_id),
        login_customer_id: digits(&s.login_customer_id),
        sem_id: t(&s.sem_id),
    };
    let ok = |v: &Option<String>, f: &dyn Fn(&str) -> bool| v.as_deref().is_none_or(f);
    let alnum = |v: &str, max: usize| {
        (1..=max).contains(&v.len())
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    let checks: [(&str, bool); 7] = [
        ("pixel_id", ok(&out.pixel_id, &|v| all_digits(v, 5..=20))),
        (
            "test_event_code",
            ok(&out.test_event_code, &|v| alnum(v, 32)),
        ),
        (
            "measurement_id",
            ok(&out.measurement_id, &|v| {
                v.strip_prefix("G-").is_some_and(|r| {
                    (4..=20).contains(&r.len())
                        && r.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                })
            }),
        ),
        (
            "customer_id",
            ok(&out.customer_id, &|v| all_digits(v, 10..=10)),
        ),
        (
            "conversion_action_id",
            ok(&out.conversion_action_id, &|v| all_digits(v, 1..=20)),
        ),
        (
            "login_customer_id",
            ok(&out.login_customer_id, &|v| all_digits(v, 10..=10)),
        ),
        ("sem_id", ok(&out.sem_id, &|v| alnum(v, 64))),
    ];
    if let Some((f, _)) = checks.iter().find(|(_, good)| !good) {
        return Err(invalid(
            "invalid_settings",
            format!("{f} has the wrong format"),
        ));
    }
    Ok(out)
}

fn check_credentials(platform: Platform, c: &Credentials) -> Result<(), Error> {
    for f in c.present() {
        if !platform.credential_fields().contains(&f) {
            return Err(invalid(
                "invalid_credentials",
                format!("{} takes no {f}", platform.as_str()),
            ));
        }
        let v = c.get(f).map(String::as_str).unwrap_or_default();
        if !(1..=2048).contains(&v.len()) || !v.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(invalid(
                "invalid_credentials",
                format!("{f} must be 1-2048 visible ASCII characters"),
            ));
        }
    }
    Ok(())
}

fn complete(platform: Platform, test_mode: bool, s: &Settings, c: Option<&Credentials>) -> bool {
    platform
        .required_settings(test_mode)
        .iter()
        .all(|f| s.get(f).is_some())
        && platform
            .credential_fields()
            .iter()
            .all(|f| c.is_some_and(|c| c.get(f).is_some()))
}

struct Row {
    platform: String,
    enabled: bool,
    paused: bool,
    test_mode: bool,
    market_ids: Vec<Uuid>,
    settings: Value,
    credentials_ciphertext: Option<Vec<u8>>,
    credentials_hint: Option<String>,
    updated_at: DateTime<Utc>,
}

fn open_credentials(
    ads: &AdTracking,
    tenant: Uuid,
    platform: Platform,
    sealed: Option<&[u8]>,
) -> Result<Option<Credentials>, Error> {
    let Some(sealed) = sealed else {
        return Ok(None);
    };
    let plain = ads
        .secrets
        .open(sealed, &aad(tenant, platform))
        .map_err(|_| {
            Error::Internal("stored ad-platform credentials cannot be decrypted".into())
        })?;
    serde_json::from_slice(&plain)
        .map(Some)
        .map_err(|_| Error::Internal("stored ad-platform credentials are malformed".into()))
}

fn view(platform: Platform, row: Option<&Row>) -> PlatformConfig {
    let settings: Settings = row
        .and_then(|r| serde_json::from_value(r.settings.clone()).ok())
        .unwrap_or_default();
    let has_credentials = row.is_some_and(|r| r.credentials_ciphertext.is_some());
    let test_mode = row.is_some_and(|r| r.test_mode);
    // Stored credentials are complete ([`update`] refuses partial ones).
    let creds_complete = platform.credential_fields().is_empty() || has_credentials;
    PlatformConfig {
        platform,
        enabled: row.is_some_and(|r| r.enabled),
        paused: row.is_some_and(|r| r.paused),
        test_mode,
        market_ids: row.map(|r| r.market_ids.clone()).unwrap_or_default(),
        complete: creds_complete
            && platform
                .required_settings(test_mode)
                .iter()
                .all(|f| settings.get(f).is_some()),
        settings,
        has_credentials,
        credentials_hint: row.and_then(|r| r.credentials_hint.clone()),
        setting_fields: platform
            .setting_fields()
            .iter()
            .map(|f| (*f).to_owned())
            .collect(),
        credential_fields: platform
            .credential_fields()
            .iter()
            .map(|f| (*f).to_owned())
            .collect(),
        events: platform.events().into_iter().map(str::to_owned).collect(),
        notices: platform.notices(),
        updated_at: row.map(|r| r.updated_at),
    }
}

async fn rows(tx: &mut TenantTx) -> Result<Vec<Row>, Error> {
    Ok(sqlx::query_as!(
        Row,
        "SELECT platform, enabled, paused, test_mode, market_ids, settings, credentials_ciphertext,
                credentials_hint, updated_at
         FROM ad_platforms"
    )
    .fetch_all(&mut **tx)
    .await?)
}

/// Every platform's configuration (unconfigured ones as defaults). Never the credentials.
pub async fn list(tx: &mut TenantTx) -> Result<PlatformList, Error> {
    let rows = rows(tx).await?;
    Ok(PlatformList {
        items: Platform::ALL
            .into_iter()
            .map(|p| view(p, rows.iter().find(|r| r.platform == p.as_str())))
            .collect(),
    })
}

async fn get(tx: &mut TenantTx, platform: Platform) -> Result<PlatformConfig, Error> {
    let rows = rows(tx).await?;
    Ok(view(
        platform,
        rows.iter().find(|r| r.platform == platform.as_str()),
    ))
}

/// Updates (or creates) a platform's configuration. Enabling needs complete settings and
/// credentials; resuming queues the deliveries held while paused.
pub async fn update(
    tx: &mut TenantTx,
    ads: &AdTracking,
    actor: &str,
    platform: Platform,
    input: &PlatformUpdate,
) -> Result<PlatformConfig, Error> {
    let tenant = tx.tenant_id();
    let before = sqlx::query_as!(
        Row,
        "SELECT platform, enabled, paused, test_mode, market_ids, settings, credentials_ciphertext,
                credentials_hint, updated_at
         FROM ad_platforms WHERE platform = $1 FOR UPDATE",
        platform.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?;
    let old_settings: Settings = before
        .as_ref()
        .and_then(|r| serde_json::from_value(r.settings.clone()).ok())
        .unwrap_or_default();
    let settings = match &input.settings {
        Some(s) => check_settings(platform, s)?,
        None => old_settings,
    };
    let old_creds = open_credentials(
        ads,
        tenant,
        platform,
        before
            .as_ref()
            .and_then(|r| r.credentials_ciphertext.as_deref()),
    )?;
    let creds = match &input.credentials {
        Some(c) => {
            check_credentials(platform, c)?;
            let merged = old_creds.clone().unwrap_or_default().merged(c.clone());
            // Stored credentials are always complete, so the list can tell without them.
            if !platform
                .credential_fields()
                .iter()
                .all(|f| merged.get(f).is_some())
            {
                return Err(invalid(
                    "incomplete_credentials",
                    format!(
                        "{} needs {}",
                        platform.as_str(),
                        platform.credential_fields().join(", ")
                    ),
                ));
            }
            Some(merged)
        }
        None => old_creds,
    };
    let market_ids = match &input.market_ids {
        Some(ids) => {
            let mut ids = ids.clone();
            ids.sort();
            ids.dedup();
            let known: i64 = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM markets WHERE id = ANY ($1)"#,
                &ids
            )
            .fetch_one(&mut **tx)
            .await?;
            if usize::try_from(known).ok() != Some(ids.len()) || ids.len() > 50 {
                return Err(invalid("invalid_markets", "unknown market"));
            }
            ids
        }
        None => before
            .as_ref()
            .map(|r| r.market_ids.clone())
            .unwrap_or_default(),
    };
    let enabled = input
        .enabled
        .unwrap_or(before.as_ref().is_some_and(|r| r.enabled));
    let paused = input
        .paused
        .unwrap_or(before.as_ref().is_some_and(|r| r.paused));
    let test_mode = input
        .test_mode
        .unwrap_or(before.as_ref().is_some_and(|r| r.test_mode));
    if enabled && !complete(platform, test_mode, &settings, creds.as_ref()) {
        return Err(invalid(
            "incomplete_configuration",
            "fill in every required setting and credential before enabling",
        ));
    }
    let sealed = creds
        .as_ref()
        .filter(|c| !c.present().is_empty())
        .map(|c| -> Result<Vec<u8>, Error> {
            let plain = serde_json::to_vec(c).map_err(|e| Error::Internal(e.to_string()))?;
            Ok(ads.secrets.seal(&plain, &aad(tenant, platform)))
        })
        .transpose()?;
    let settings_json =
        serde_json::to_value(&settings).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "INSERT INTO ad_platforms (tenant_id, platform, enabled, paused, test_mode, market_ids,
                                   settings, credentials_ciphertext, credentials_hint)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (tenant_id, platform) DO UPDATE
         SET enabled = $3, paused = $4, test_mode = $5, market_ids = $6, settings = $7,
             credentials_ciphertext = $8, credentials_hint = $9, updated_at = now()",
        tenant,
        platform.as_str(),
        enabled,
        paused,
        test_mode,
        &market_ids,
        settings_json,
        sealed,
        creds.as_ref().and_then(Credentials::hint)
    )
    .execute(&mut **tx)
    .await?;
    if before.as_ref().is_some_and(|r| r.paused) && !paused {
        resume(tx, platform).await?;
    }
    let after = get(tx, platform).await?;
    audit::record(
        tx,
        actor,
        "ad_platform.updated",
        "ad_platform",
        Some(platform.as_str()),
        &json!({
            "before": before.as_ref().map(|r| json!({
                "enabled": r.enabled, "paused": r.paused, "test_mode": r.test_mode,
                "market_ids": r.market_ids, "settings": r.settings,
                "credentials_hint": r.credentials_hint,
            })),
            "after": {
                "enabled": after.enabled, "paused": after.paused, "test_mode": after.test_mode,
                "market_ids": after.market_ids, "settings": after.settings,
                "credentials_hint": after.credentials_hint,
            },
            "credentials_changed": input.credentials.is_some(),
        }),
    )
    .await?;
    Ok(after)
}

fn deliver_job(tenant: Uuid, delivery: Uuid, key: &str) -> NewJob<'static> {
    let mut job = NewJob::new(DELIVER_JOB, json!({ "delivery_id": delivery }));
    job.tenant_id = Some(tenant);
    job.max_attempts = MAX_ATTEMPTS;
    job.idempotency_key = Some(format!("adtracking:{delivery}:{key}"));
    job
}

/// Queues the deliveries held while the platform was paused.
async fn resume(tx: &mut TenantTx, platform: Platform) -> Result<(), Error> {
    let held = sqlx::query_scalar!(
        "UPDATE ad_deliveries SET status = 'pending', updated_at = now()
         WHERE platform = $1 AND status = 'paused' RETURNING id",
        platform.as_str()
    )
    .fetch_all(&mut **tx)
    .await?;
    let key = format!("resume-{}", Utc::now().timestamp_micros());
    for id in held {
        let job = deliver_job(tx.tenant_id(), id, &key);
        queue::enqueue(&mut **tx, &job).await?;
    }
    Ok(())
}

// --- consent -----------------------------------------------------------------------------------

/// A20 at execution time: the anonymous subject grants `ads` now, and the customer (if the
/// event belongs to one) has not refused it on their account (e.g. from another device).
async fn ads_allowed(
    tx: &mut TenantTx,
    subject: &str,
    customer: Option<Uuid>,
) -> Result<bool, Error> {
    if !consent::well_formed_anon(subject)
        || !consent::current(tx, &Subject::Anon(subject.to_owned()), ConsentPurpose::Ads).await?
    {
        return Ok(false);
    }
    match customer {
        Some(c) => Ok(
            consent::latest(tx, &Subject::Customer(c), ConsentPurpose::Ads).await? != Some(false),
        ),
        None => Ok(true),
    }
}

/// Called when `ads = false` is recorded for `subject`: its open deliveries are cancelled.
pub async fn cancel_for_subject(tx: &mut TenantTx, subject: &Subject) -> Result<u64, Error> {
    let (anon, customer) = match subject {
        Subject::Anon(a) => (Some(a.as_str()), None),
        Subject::Customer(c) => (None, Some(*c)),
        Subject::Email(_) => return Ok(0),
    };
    Ok(sqlx::query!(
        "UPDATE ad_deliveries
         SET status = 'cancelled', last_error = 'ads consent withdrawn', user_agent = NULL,
             finished_at = now(), updated_at = now()
         WHERE status IN ('pending', 'retrying', 'paused', 'sending')
           AND (subject = $1 OR customer_id = $2)",
        anon,
        customer
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

// --- capture -----------------------------------------------------------------------------------

/// Enabled platforms of a market: `(platform, paused)`.
async fn active(tx: &mut TenantTx, market: Uuid) -> Result<Vec<(Platform, bool)>, Error> {
    Ok(sqlx::query!(
        // FOR SHARE: a concurrent pause/resume ([`update`]) waits, so a delivery inserted as
        // `paused` is always seen by the resume that follows.
        "SELECT platform, paused FROM ad_platforms WHERE enabled AND $1 = ANY (market_ids)
         FOR SHARE",
        market
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .filter_map(|r| Platform::parse(&r.platform).map(|p| (p, r.paused)))
    .collect())
}

struct NewDelivery<'a> {
    event_id: Uuid,
    event_name: &'a str,
    market: Uuid,
    subject: &'a str,
    customer: Option<Uuid>,
    order: Option<Uuid>,
    props: Value,
    user_agent: Option<&'a str>,
    at: DateTime<Utc>,
}

/// One delivery per platform that takes the event (+ its job unless paused). Idempotent per
/// (platform, event id).
async fn insert(
    tx: &mut TenantTx,
    platforms: &[(Platform, bool)],
    d: &NewDelivery<'_>,
) -> Result<usize, Error> {
    let mut n = 0;
    for (platform, paused) in platforms {
        if vendors::event_name(*platform, d.event_name).is_none() {
            continue;
        }
        let id = sqlx::query_scalar!(
            "INSERT INTO ad_deliveries (tenant_id, platform, event_id, event_name, market_id,
                                        subject, customer_id, order_id, props, user_agent,
                                        occurred_at, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (tenant_id, platform, event_id) DO NOTHING
             RETURNING id",
            tx.tenant_id(),
            platform.as_str(),
            d.event_id,
            d.event_name,
            d.market,
            d.subject,
            d.customer,
            d.order,
            d.props,
            d.user_agent
                .map(|u| u.chars().take(512).collect::<String>()),
            d.at,
            if *paused { "paused" } else { "pending" }
        )
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(id) = id {
            n += 1;
            if !paused {
                let job = deliver_job(tx.tenant_id(), id, "first");
                queue::enqueue(&mut **tx, &job).await?;
            }
        }
    }
    Ok(n)
}

/// Browser-side events of a visitor (the beacon, and the cart steps the API records itself):
/// forwarded only while the subject grants `ads`. Props: the catalog SKUs (the ids the export
/// feeds use), the quantity and the page, derived here from the catalog (the product page in
/// the market's default locale, else the home page): paths a client reports are never stored,
/// so no identifier or capability in a URL can reach a vendor.
pub async fn capture_events(
    tx: &mut TenantTx,
    market: Uuid,
    subject: Option<&str>,
    events: &[CleanEvent],
    user_agent: Option<&str>,
    now: DateTime<Utc>,
) -> Result<usize, Error> {
    let Some(subject) = subject.filter(|s| consent::well_formed_anon(s)) else {
        return Ok(0);
    };
    let platforms = active(tx, market).await?;
    if platforms.is_empty() || !ads_allowed(tx, subject, None).await? {
        return Ok(0);
    }
    let mut n = 0;
    for e in events {
        if !matches!(
            e.kind,
            "page_view" | "view_item" | "add_to_cart" | "begin_checkout"
        ) {
            continue;
        }
        let uuid = |k: &str| e.props[k].as_str().and_then(|s| Uuid::parse_str(s).ok());
        let variant = uuid("variant_id");
        let product = match (uuid("product_id"), variant) {
            (Some(p), _) => Some(p),
            (None, Some(v)) => {
                sqlx::query_scalar!("SELECT product_id FROM variants WHERE id = $1", v)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            (None, None) => None,
        };
        let skus: Vec<String> =
            match (variant, product) {
                (Some(v), _) => {
                    sqlx::query_scalar!("SELECT sku FROM variants WHERE id = $1", v)
                        .fetch_all(&mut **tx)
                        .await?
                }
                (None, Some(p)) => sqlx::query_scalar!(
                    "SELECT sku FROM variants WHERE product_id = $1 ORDER BY position, id LIMIT 20",
                    p
                )
                .fetch_all(&mut **tx)
                .await?,
                (None, None) => vec![],
            };
        let slug = match product {
            Some(p) => {
                sqlx::query_scalar!(
                    "SELECT pt.slug FROM product_translations pt
                     JOIN markets m ON m.id = $2 AND pt.locale = m.default_locale
                     WHERE pt.product_id = $1",
                    p,
                    market
                )
                .fetch_optional(&mut **tx)
                .await?
            }
            None => None,
        };
        let path = slug.map_or_else(|| "/".to_owned(), |s| format!("/p/{s}"));
        let mut props = json!({ "path": path });
        if !skus.is_empty() {
            props["skus"] = json!(skus);
        }
        if let Some(q) = e.props["quantity"].as_i64() {
            props["quantity"] = json!(q);
        }
        n += insert(
            tx,
            &platforms,
            &NewDelivery {
                event_id: crate::id::new_id(),
                event_name: e.kind,
                market,
                subject,
                customer: None,
                order: None,
                props,
                user_agent,
                at: now,
            },
        )
        .await?;
    }
    Ok(n)
}

/// A stable event id per order and event: a replayed placement or a redelivered outbox event
/// never creates a second delivery.
fn order_event_id(order: Uuid, event: &str) -> Uuid {
    let d = Sha256::digest(format!("ad-{event}:{order}"));
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    Uuid::from_bytes(b)
}

/// The purchase of a placed order, in the placement transaction, when the checkout request
/// carried a consent subject that grants `ads`. Amounts and identifiers are read from the
/// order when sending, never from the client.
pub async fn capture_purchase(
    tx: &mut TenantTx,
    order: Uuid,
    subject: &str,
    user_agent: Option<&str>,
) -> Result<usize, Error> {
    let Some(o) = sqlx::query!(
        "SELECT market_id, customer_id, currency, placed_at FROM orders WHERE id = $1",
        order
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(0);
    };
    let mut platforms = active(tx, o.market_id).await?;
    // Seznam takes CZK only.
    platforms.retain(|(p, _)| *p != Platform::Sklik || o.currency == "CZK");
    if platforms.is_empty() || !ads_allowed(tx, subject, o.customer_id).await? {
        return Ok(0);
    }
    insert(
        tx,
        &platforms,
        &NewDelivery {
            event_id: order_event_id(order, "purchase"),
            event_name: "purchase",
            market: o.market_id,
            subject,
            customer: o.customer_id,
            order: Some(order),
            props: json!({}),
            user_agent,
            at: o.placed_at,
        },
    )
    .await
}

/// Worker step for [`REFUND_JOB`]: a refund of an order whose purchase was forwarded goes to
/// the platforms that take refunds, for the same subject (consent is checked again).
/// `amount_minor` is the refunded amount (the order total when absent).
pub async fn capture_refund(
    db: &PgPool,
    tenant: Uuid,
    outbox_id: i64,
    data: &Value,
) -> Result<usize, Error> {
    let Some(order) = data["order_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(0);
    };
    let mut tx = tenant_tx(db, tenant).await?;
    let Some(p) = sqlx::query!(
        "SELECT market_id, subject, customer_id FROM ad_deliveries
         WHERE order_id = $1 AND event_name = 'purchase' ORDER BY created_at LIMIT 1",
        order
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(0);
    };
    let platforms = active(&mut tx, p.market_id).await?;
    if platforms.is_empty() || !ads_allowed(&mut tx, &p.subject, p.customer_id).await? {
        return Ok(0);
    }
    let amount = data["amount_minor"].as_i64().filter(|a| *a > 0);
    let n = insert(
        &mut tx,
        &platforms,
        &NewDelivery {
            event_id: order_event_id(order, &format!("refund-{outbox_id}")),
            event_name: "refund",
            market: p.market_id,
            subject: &p.subject,
            customer: p.customer_id,
            order: Some(order),
            props: json!({ "amount_minor": amount }),
            user_agent: None,
            at: Utc::now(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(n)
}

// --- log ---------------------------------------------------------------------------------------

const STATUSES: [&str; 8] = [
    "pending",
    "retrying",
    "paused",
    "sending",
    "succeeded",
    "dead",
    "cancelled",
    "skipped",
];

/// The delivery log, newest first.
pub async fn deliveries(tx: &mut TenantTx, q: &DeliveryQuery) -> Result<DeliveryPage, Error> {
    if let Some(s) = &q.status
        && !STATUSES.contains(&s.as_str())
    {
        return Err(invalid("invalid_status", "unknown delivery status"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, PAGE_MAX);
    let rows = sqlx::query!(
        "SELECT id, platform, event_name, event_id, order_id, status, attempts, response_code,
                last_error, occurred_at, created_at, finished_at
         FROM ad_deliveries
         WHERE ($1::text IS NULL OR platform = $1) AND ($2::text IS NULL OR status = $2)
           AND ($3::uuid IS NULL OR id < $3)
         ORDER BY id DESC LIMIT $4",
        q.platform.map(Platform::as_str),
        q.status,
        q.cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items: Vec<Delivery> = rows
        .into_iter()
        .filter_map(|r| {
            Some(Delivery {
                id: r.id,
                platform: Platform::parse(&r.platform)?,
                event_name: r.event_name,
                event_id: r.event_id,
                order_id: r.order_id,
                status: r.status,
                attempts: r.attempts,
                response_code: r.response_code,
                last_error: r.last_error,
                occurred_at: r.occurred_at,
                created_at: r.created_at,
                finished_at: r.finished_at,
            })
        })
        .collect();
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(DeliveryPage { items, next_cursor })
}

// --- delivery ----------------------------------------------------------------------------------

/// What one run of a delivery job did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    /// A retryable failure (recorded as `retrying`); the job should be retried.
    Retry(String),
    Dead,
    /// Consent is gone: nothing was sent.
    Cancelled,
    /// Disabled platform or test mode without a test channel: nothing was sent.
    Skipped,
    /// The platform is paused: held until it resumes.
    Paused,
    /// Already finished (or a duplicate job).
    Stale,
}

fn pseudonym(tenant: Uuid, subject: &str) -> String {
    hex::encode(Sha256::digest(format!("adtracking:{tenant}:{subject}")))
}

/// Records the delivery's next state unless it finished meanwhile: a terminal one (the user
/// agent is dropped with it), `retrying`, `paused` or `sending`.
async fn finish_in(
    tx: &mut TenantTx,
    id: Uuid,
    status: &str,
    attempts: Option<i32>,
    code: Option<i32>,
    error: Option<&str>,
) -> Result<bool, Error> {
    let terminal = !matches!(status, "paused" | "retrying" | "sending");
    Ok(sqlx::query!(
        "UPDATE ad_deliveries
         SET status = $2, attempts = coalesce($3, attempts),
             response_code = coalesce($4, response_code), last_error = $5, updated_at = now(),
             finished_at = CASE WHEN $6 THEN now() END,
             user_agent = CASE WHEN $6 THEN NULL ELSE user_agent END
         WHERE id = $1 AND status IN ('pending', 'retrying', 'sending')",
        id,
        status,
        attempts,
        code,
        error.map(|e| e.chars().take(300).collect::<String>()),
        terminal
    )
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
}

async fn finish(
    db: &PgPool,
    tenant: Uuid,
    id: Uuid,
    status: &str,
    attempts: Option<i32>,
    code: Option<i32>,
    error: Option<&str>,
) -> Result<bool, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let done = finish_in(&mut tx, id, status, attempts, code, error).await?;
    tx.commit().await?;
    Ok(done)
}

/// Why a send failed: our own words (a vendor body may echo the request).
enum Failure {
    Retryable(Option<i32>, String),
    Permanent(Option<i32>, String),
}

fn classify(code: u16) -> Result<(), Failure> {
    match code {
        200..=299 => Ok(()),
        408 | 429 | 500..=599 => Err(Failure::Retryable(
            Some(i32::from(code)),
            format!("HTTP {code}"),
        )),
        _ => Err(Failure::Permanent(
            Some(i32::from(code)),
            format!("HTTP {code}: the platform rejected the request"),
        )),
    }
}

async fn post_json(
    ads: &AdTracking,
    url: &str,
    bearer: Option<&str>,
    extra: &[(&'static str, String)],
    body: &Value,
) -> Result<(u16, Vec<u8>), Failure> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let bad = |_| Failure::Permanent(None, "invalid header value".into());
    if let Some(t) = bearer {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {t}")).map_err(bad)?,
        );
    }
    for (k, v) in extra {
        headers.insert(
            HeaderName::from_static(k),
            HeaderValue::from_str(v).map_err(bad)?,
        );
    }
    let body =
        serde_json::to_vec(body).map_err(|_| Failure::Permanent(None, "unserializable".into()))?;
    ads.http
        .post_read(url, headers, body, LIMITS)
        .await
        .map_err(|e| Failure::Retryable(None, network_error(&e)))
}

/// A transport failure without anything request-specific.
fn network_error(e: &platform::http::FetchError) -> String {
    match e {
        platform::http::FetchError::Blocked(_) => "the platform's address is not public".into(),
        platform::http::FetchError::TooLarge(_) => "the answer was too large".into(),
        platform::http::FetchError::InvalidUrl => "invalid platform URL".into(),
        _ => "the platform could not be reached".into(),
    }
}

/// A Google OAuth access token for the credentials, cached until a minute before expiry.
async fn google_token(ads: &AdTracking, creds: &Credentials) -> Result<String, Failure> {
    let missing = |f| Failure::Permanent(None, format!("{f} is not configured"));
    let client_id = creds
        .client_id
        .as_deref()
        .ok_or_else(|| missing("client_id"))?;
    let secret = creds
        .client_secret
        .as_deref()
        .ok_or_else(|| missing("client_secret"))?;
    let refresh = creds
        .refresh_token
        .as_deref()
        .ok_or_else(|| missing("refresh_token"))?;
    let key = hex::encode(Sha256::digest(format!("{client_id}\n{secret}\n{refresh}")));
    if let Ok(cache) = ads.tokens.lock()
        && let Some((token, until)) = cache.get(&key)
        && *until > Instant::now()
    {
        return Ok(token.clone());
    }
    let form = format!(
        "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
        vendors::query_escape(client_id),
        vendors::query_escape(secret),
        vendors::query_escape(refresh)
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );
    let (code, body) = ads
        .http
        .post_read(
            &ads.endpoints.google_token(),
            headers,
            form.into_bytes(),
            LIMITS,
        )
        .await
        .map_err(|e| Failure::Retryable(None, network_error(&e)))?;
    match code {
        200..=299 => {}
        400 | 401 | 403 => {
            return Err(Failure::Permanent(
                Some(i32::from(code)),
                format!("HTTP {code}: Google refused the OAuth credentials"),
            ));
        }
        _ => {
            return Err(Failure::Retryable(
                Some(i32::from(code)),
                format!("HTTP {code} from the OAuth endpoint"),
            ));
        }
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    let token = v["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Failure::Retryable(None, "the OAuth answer has no access token".into()))?
        .to_owned();
    let ttl = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(60);
    if let Ok(mut cache) = ads.tokens.lock() {
        cache.retain(|_, (_, until)| *until > Instant::now());
        cache.insert(
            key,
            (token.clone(), Instant::now() + Duration::from_secs(ttl)),
        );
    }
    Ok(token)
}

/// The Google access token for the request (other platforms need none).
async fn bearer(
    ads: &AdTracking,
    platform: Platform,
    creds: &Credentials,
) -> Result<Option<String>, Failure> {
    match platform {
        Platform::GoogleAds => Ok(Some(google_token(ads, creds).await?)),
        _ => Ok(None),
    }
}

/// Sends a built request and checks the answer.
async fn send(
    ads: &AdTracking,
    platform: Platform,
    req: &vendors::Request,
    bearer: Option<&str>,
) -> Result<i32, Failure> {
    let (code, body) = post_json(ads, &req.url, bearer, &req.headers, &req.body).await?;
    classify(code)?;
    // The GA4 validation server answers 200 with its findings.
    if platform == Platform::Ga4 && req.url.contains("/debug/mp/collect") {
        let v: Value = serde_json::from_slice(&body).unwrap_or_default();
        if v["validationMessages"]
            .as_array()
            .is_some_and(|m| !m.is_empty())
        {
            return Err(Failure::Permanent(
                Some(i32::from(code)),
                "GA4 validation reported problems with the event".into(),
            ));
        }
    }
    Ok(i32::from(code))
}

/// The last decision before a send, in one transaction: the platform row is locked (shared)
/// against a concurrent pause/resume, the platform must still be on for the delivery's market,
/// and consent is resolved now (A20). Only then the delivery becomes `sending`; a withdrawal
/// committed before this point cancels it, one committed after it finds it already sending.
async fn claim_send(
    db: &PgPool,
    tenant: Uuid,
    id: Uuid,
    attempt: i32,
) -> Result<Option<Outcome>, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let Some(d) = sqlx::query!(
        "SELECT d.status, d.subject, d.customer_id, d.market_id, p.enabled, p.paused, p.market_ids
         FROM ad_deliveries d
         JOIN ad_platforms p ON p.tenant_id = d.tenant_id AND p.platform = d.platform
         WHERE d.id = $1
         FOR SHARE OF p",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Some(Outcome::Stale));
    };
    let outcome = if !matches!(d.status.as_str(), "pending" | "retrying" | "sending") {
        Some(Outcome::Stale)
    } else if !d.enabled || !d.market_ids.contains(&d.market_id) {
        let why = "the platform is off for this market";
        finish_in(&mut tx, id, "skipped", None, None, Some(why)).await?;
        Some(Outcome::Skipped)
    } else if d.paused {
        finish_in(&mut tx, id, "paused", None, None, None).await?;
        Some(Outcome::Paused)
    } else if !ads_allowed(&mut tx, &d.subject, d.customer_id).await? {
        let why = "ads consent not granted at send time";
        finish_in(&mut tx, id, "cancelled", None, None, Some(why)).await?;
        Some(Outcome::Cancelled)
    } else if finish_in(&mut tx, id, "sending", Some(attempt), None, None).await? {
        None
    } else {
        // Cancelled (or finished) between the reads and this update.
        Some(Outcome::Stale)
    };
    tx.commit().await?;
    Ok(outcome)
}

/// Worker step for [`DELIVER_JOB`]: one attempt. `attempt` is the job's attempt number;
/// `last` says whether the queue will give up after this one. The request (with the order
/// read and hashed now) and the Google token are prepared first; [`claim_send`] then decides
/// right before sending. A crash while `sending` retries the send (at least once: vendors
/// dedupe on the event id), re-checking consent first.
pub async fn deliver(
    db: &PgPool,
    ads: &AdTracking,
    tenant: Uuid,
    id: Uuid,
    attempt: i32,
    last: bool,
) -> Result<Outcome, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let Some(d) = sqlx::query!(
        "SELECT d.platform, d.event_id, d.event_name, d.subject, d.order_id, d.props,
                d.user_agent, d.occurred_at, d.status, p.test_mode, p.settings,
                p.credentials_ciphertext,
                (SELECT h.hostname FROM platform.domains h
                 WHERE h.market_id = d.market_id AND h.verified_at IS NOT NULL
                 ORDER BY h.is_primary DESC, h.hostname LIMIT 1) AS host
         FROM ad_deliveries d
         JOIN ad_platforms p ON p.tenant_id = d.tenant_id AND p.platform = d.platform
         WHERE d.id = $1",
        id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Outcome::Stale);
    };
    if !matches!(d.status.as_str(), "pending" | "retrying" | "sending") {
        return Ok(Outcome::Stale);
    }
    let platform = Platform::parse(&d.platform)
        .ok_or_else(|| Error::Internal(format!("unknown platform {}", d.platform)))?;
    if Utc::now() - d.occurred_at > platform.max_age() {
        let why = "the event is older than the platform accepts";
        finish(db, tenant, id, "dead", Some(attempt), None, Some(why)).await?;
        return Ok(Outcome::Dead);
    }
    if d.test_mode && platform == Platform::Sklik {
        let why = "test mode: Seznam has no test channel, nothing was sent";
        finish(db, tenant, id, "skipped", None, None, Some(why)).await?;
        return Ok(Outcome::Skipped);
    }
    let order = match d.order_id {
        Some(o) => load_order(&mut tx, o).await?,
        None => None,
    };
    tx.commit().await?;

    let settings: Settings = serde_json::from_value(d.settings).unwrap_or_default();
    let creds = open_credentials(ads, tenant, platform, d.credentials_ciphertext.as_deref())?
        .unwrap_or_default();
    let path = d.props["path"].as_str().unwrap_or("/");
    let url = match &d.host {
        Some(h) => format!("{}{path}", ads.urls.base(h)),
        None => path.to_owned(),
    };
    let event = vendors::Event {
        event_name: d.event_name,
        event_id: d.event_id,
        occurred_at: d.occurred_at,
        url,
        pseudonym: pseudonym(tenant, &d.subject),
        user_agent: d.user_agent,
        props: d.props,
        order,
        test_mode: d.test_mode,
    };
    let req = match vendors::build(&ads.endpoints, platform, &settings, &creds, &event) {
        Ok(r) => r,
        Err(e @ BuildError::NoIdentifiers) => {
            finish(db, tenant, id, "skipped", None, None, Some(&e.to_string())).await?;
            return Ok(Outcome::Skipped);
        }
        Err(e) => {
            let why = e.to_string();
            finish(db, tenant, id, "dead", Some(attempt), None, Some(&why)).await?;
            return Ok(Outcome::Dead);
        }
    };
    if let Some(l) = ads.limiters.get(&platform) {
        l.until_key_ready(&tenant).await;
    }
    let result = match bearer(ads, platform, &creds).await {
        Ok(token) => match claim_send(db, tenant, id, attempt).await? {
            Some(outcome) => return Ok(outcome),
            None => send(ads, platform, &req, token.as_deref()).await,
        },
        Err(f) => Err(f),
    };
    Ok(match result {
        Ok(code) => {
            finish(db, tenant, id, "succeeded", Some(attempt), Some(code), None).await?;
            Outcome::Succeeded
        }
        Err(Failure::Retryable(code, why)) if !last => {
            finish(db, tenant, id, "retrying", Some(attempt), code, Some(&why)).await?;
            Outcome::Retry(why)
        }
        Err(Failure::Retryable(code, why) | Failure::Permanent(code, why)) => {
            finish(db, tenant, id, "dead", Some(attempt), code, Some(&why)).await?;
            Outcome::Dead
        }
    })
}

/// Marks an open delivery `dead` when its job gave up without recording an outcome (e.g. a
/// database error or a missing `SECRETS_KEY` on the last attempt).
pub async fn give_up(db: &PgPool, tenant: Uuid, id: Uuid, why: &str) -> Result<bool, Error> {
    finish(db, tenant, id, "dead", None, None, Some(why)).await
}

async fn load_order(tx: &mut TenantTx, id: Uuid) -> Result<Option<vendors::Order>, Error> {
    let Some(o) = sqlx::query!(
        "SELECT number, email, phone, ship_to_country, currency, total_minor, tax_minor,
                shipping_minor
         FROM orders WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let lines = sqlx::query!(
        "SELECT sku, name, quantity, unit_gross_minor, net_minor FROM order_lines
         WHERE order_id = $1 ORDER BY position",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|l| vendors::Line {
        sku: l.sku,
        name: l.name,
        quantity: l.quantity,
        unit_gross_minor: l.unit_gross_minor,
        net_minor: l.net_minor,
    })
    .collect();
    Ok(Some(vendors::Order {
        number: o.number,
        email: o.email,
        phone: o.phone,
        country: o.ship_to_country,
        currency: o.currency,
        total_minor: o.total_minor,
        tax_minor: o.tax_minor,
        shipping_minor: o.shipping_minor,
        lines,
    }))
}

/// Checks the platform's configuration against the vendor without recording anything:
/// Meta reads the pixel with the token, GA4 validates a sample event on the validation server,
/// Google exchanges the refresh token and validates a sample conversion (`validateOnly`).
/// Seznam has no test endpoint: only the configuration is checked.
pub async fn test_connection(
    tx: &mut TenantTx,
    ads: &AdTracking,
    platform: Platform,
) -> Result<ConnectionTest, Error> {
    let tenant = tx.tenant_id();
    let row = sqlx::query!(
        "SELECT settings, credentials_ciphertext, test_mode FROM ad_platforms WHERE platform = $1",
        platform.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        return Ok(ConnectionTest {
            ok: false,
            response_code: None,
            message: "the platform is not configured".into(),
        });
    };
    let settings: Settings = serde_json::from_value(row.settings).unwrap_or_default();
    let creds = open_credentials(ads, tenant, platform, row.credentials_ciphertext.as_deref())?;
    if !complete(platform, false, &settings, creds.as_ref()) {
        return Ok(ConnectionTest {
            ok: false,
            response_code: None,
            message: "settings or credentials are incomplete".into(),
        });
    }
    let creds = creds.unwrap_or_default();
    let sample = vendors::Event {
        event_name: "purchase".into(),
        event_id: crate::id::new_id(),
        occurred_at: Utc::now(),
        url: "https://example.com/".into(),
        pseudonym: pseudonym(tenant, "connection-test"),
        user_agent: None,
        props: json!({}),
        order: Some(vendors::Order {
            number: 1,
            email: "connection-test@example.com".into(),
            phone: None,
            country: "CZ".into(),
            currency: "CZK".into(),
            total_minor: 100,
            tax_minor: 0,
            shipping_minor: 0,
            lines: vec![],
        }),
        test_mode: true,
    };
    let result: Result<i32, Failure> = match platform {
        Platform::Meta => {
            let pixel = settings.pixel_id.as_deref().unwrap_or_default();
            let token = creds.access_token.as_deref().unwrap_or_default();
            let url = format!(
                "{}?fields=id&access_token={}",
                ads.endpoints.meta_pixel(pixel),
                vendors::query_escape(token)
            );
            match ads.http.get(&url, LIMITS).await {
                Ok(_) => Ok(200),
                Err(platform::http::FetchError::Status(code)) => {
                    classify(code).map(|()| i32::from(code))
                }
                Err(e) => Err(Failure::Retryable(None, network_error(&e))),
            }
        }
        Platform::Sklik => {
            return Ok(ConnectionTest {
                ok: true,
                response_code: None,
                message:
                    "Seznam has no test endpoint: the SEM id is set, events are sent as they happen"
                        .into(),
            });
        }
        Platform::Ga4 | Platform::GoogleAds => {
            match vendors::build(&ads.endpoints, platform, &settings, &creds, &sample) {
                Ok(req) => match bearer(ads, platform, &creds).await {
                    Ok(token) => send(ads, platform, &req, token.as_deref()).await,
                    Err(f) => Err(f),
                },
                Err(e) => Err(Failure::Permanent(None, e.to_string())),
            }
        }
    };
    Ok(match result {
        Ok(code) => ConnectionTest {
            ok: true,
            response_code: Some(code),
            message: "the platform accepted the credentials".into(),
        },
        Err(Failure::Retryable(code, m) | Failure::Permanent(code, m)) => ConnectionTest {
            ok: false,
            response_code: code,
            message: m,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms_take_their_events() {
        assert_eq!(
            Platform::Meta.events(),
            [
                "page_view",
                "view_item",
                "add_to_cart",
                "begin_checkout",
                "purchase"
            ]
        );
        assert_eq!(Platform::Ga4.events().len(), 6);
        assert_eq!(Platform::GoogleAds.events(), ["purchase"]);
        assert_eq!(Platform::Sklik.events(), ["purchase"]);
    }

    #[test]
    fn settings_are_validated_per_platform() {
        let meta = Settings {
            pixel_id: Some(" 123456789 ".into()),
            ..Settings::default()
        };
        assert_eq!(
            check_settings(Platform::Meta, &meta)
                .unwrap()
                .pixel_id
                .as_deref(),
            Some("123456789")
        );
        assert!(
            check_settings(Platform::Ga4, &meta).is_err(),
            "not a GA4 field"
        );
        let google = Settings {
            customer_id: Some("123-456-7890".into()),
            conversion_action_id: Some("42".into()),
            ..Settings::default()
        };
        assert_eq!(
            check_settings(Platform::GoogleAds, &google)
                .unwrap()
                .customer_id
                .as_deref(),
            Some("1234567890")
        );
        for bad in [
            Settings {
                measurement_id: Some("UA-1234".into()),
                ..Settings::default()
            },
            Settings {
                measurement_id: Some("G-abc123".into()),
                ..Settings::default()
            },
        ] {
            assert!(check_settings(Platform::Ga4, &bad).is_err());
        }
        assert!(
            check_settings(
                Platform::Sklik,
                &Settings {
                    sem_id: Some("x y".into()),
                    ..Settings::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn credentials_merge_validate_and_hint() {
        let a = Credentials {
            client_id: Some("id".into()),
            client_secret: Some("secret".into()),
            refresh_token: Some("1//refresh-abcd".into()),
            ..Credentials::default()
        };
        assert!(check_credentials(Platform::GoogleAds, &a).is_ok());
        assert!(check_credentials(Platform::Meta, &a).is_err());
        assert!(
            check_credentials(
                Platform::Meta,
                &Credentials {
                    access_token: Some("has space".into()),
                    ..Credentials::default()
                }
            )
            .is_err()
        );
        let rotated = a.clone().merged(Credentials {
            refresh_token: Some("1//new-wxyz".into()),
            ..Credentials::default()
        });
        assert_eq!(rotated.client_id.as_deref(), Some("id"));
        assert_eq!(rotated.hint().as_deref(), Some("wxyz"));
        assert!(complete(
            Platform::GoogleAds,
            false,
            &Settings {
                customer_id: Some("1234567890".into()),
                conversion_action_id: Some("1".into()),
                ..Settings::default()
            },
            Some(&rotated)
        ));
        assert!(!complete(Platform::Meta, false, &Settings::default(), None));
        assert!(complete(
            Platform::Sklik,
            false,
            &Settings {
                sem_id: Some("s".into()),
                ..Settings::default()
            },
            None
        ));
    }

    #[test]
    fn answers_are_classified() {
        assert!(classify(200).is_ok());
        assert!(matches!(
            classify(429),
            Err(Failure::Retryable(Some(429), _))
        ));
        assert!(matches!(
            classify(503),
            Err(Failure::Retryable(Some(503), _))
        ));
        assert!(matches!(
            classify(400),
            Err(Failure::Permanent(Some(400), _))
        ));
        assert!(matches!(
            classify(401),
            Err(Failure::Permanent(Some(401), _))
        ));
    }

    #[test]
    fn order_event_ids_are_stable_and_distinct() {
        let o = Uuid::from_u128(1);
        assert_eq!(order_event_id(o, "purchase"), order_event_id(o, "purchase"));
        assert_ne!(order_event_id(o, "purchase"), order_event_id(o, "refund-1"));
    }
}
