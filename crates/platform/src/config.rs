//! Configuration from environment variables. Each consumer parses only the sections it needs,
//! so the worker does not require API-only settings.
//!
//! Every section has `from_env()` and `from_lookup()`; tests use the latter instead of mutating
//! the process environment.

use std::net::SocketAddr;
use std::str::FromStr;

use reqwest::Url;

/// Source of configuration values (the process environment in production).
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {name}: {reason}")]
    Invalid { name: &'static str, reason: String },
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Empty values count as unset, so `FOO=` in an env file does not produce a bogus value.
fn get(lookup: Lookup, name: &str) -> Option<String> {
    lookup(name).filter(|v| !v.trim().is_empty())
}

fn required(lookup: Lookup, name: &'static str) -> Result<String, ConfigError> {
    get(lookup, name).ok_or(ConfigError::Missing(name))
}

fn parsed<T>(lookup: Lookup, name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match get(lookup, name) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse()
            .map_err(|e: T::Err| ConfigError::Invalid {
                name,
                reason: e.to_string(),
            }),
    }
}

fn url(lookup: Lookup, name: &'static str) -> Result<Url, ConfigError> {
    let raw = required(lookup, name)?;
    let url = Url::parse(&raw).map_err(|e| ConfigError::Invalid {
        name,
        reason: e.to_string(),
    })?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(ConfigError::Invalid {
            name,
            reason: format!("unsupported scheme {other:?}"),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Dev,
    Prod,
}

impl FromStr for AppEnv {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dev" | "development" => Ok(Self::Dev),
            "prod" | "production" => Ok(Self::Prod),
            other => Err(format!("expected dev or prod, got {other:?}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// `APP_ENV`, defaults to `prod` so dev-only surfaces (Swagger UI) stay off unless asked for.
    pub env: AppEnv,
    /// `API_BIND`, defaults to `0.0.0.0:8000`.
    pub bind: SocketAddr,
}

impl ApiConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        Ok(Self {
            env: parsed(lookup, "APP_ENV", AppEnv::Prod)?,
            bind: parsed(lookup, "API_BIND", SocketAddr::from(([0, 0, 0, 0], 8000)))?,
        })
    }
}

/// Not `Debug`: the URL carries the password.
#[derive(Clone)]
pub struct DbConfig {
    /// `DATABASE_URL`
    pub url: String,
    /// `DATABASE_MAX_CONNECTIONS`, defaults to 10.
    pub max_connections: u32,
}

impl DbConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        Ok(Self {
            url: required(lookup, "DATABASE_URL")?,
            max_connections: parsed(lookup, "DATABASE_MAX_CONNECTIONS", 10)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// `WORKER_CONCURRENCY`: parallel job loops, defaults to 4 (spec §13).
    pub concurrency: usize,
}

impl WorkerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let concurrency = parsed(lookup, "WORKER_CONCURRENCY", 4)?;
        if !(1..=64).contains(&concurrency) {
            return Err(ConfigError::Invalid {
                name: "WORKER_CONCURRENCY",
                reason: "must be between 1 and 64".into(),
            });
        }
        Ok(Self { concurrency })
    }
}

/// Not Debug: contains the internal auth service secret.
#[derive(Clone)]
pub struct AuthServiceConfig {
    pub base_url: Url,
    pub token: String,
}
impl AuthServiceConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    /// For the API server: `None` when neither variable is set (the API still boots; staff
    /// invitations answer `503`), a validated config when both are, an error when only one is
    /// or the values are invalid (misconfiguration still fails fast).
    pub fn optional_from_env() -> Result<Option<Self>, ConfigError> {
        Self::optional_from_lookup(&process_env)
    }

    pub fn optional_from_lookup(lookup: Lookup) -> Result<Option<Self>, ConfigError> {
        if get(lookup, "AUTH_INTERNAL_URL").is_none()
            && get(lookup, "AUTH_INTERNAL_TOKEN").is_none()
        {
            return Ok(None);
        }
        Self::from_lookup(lookup).map(Some)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let token = required(lookup, "AUTH_INTERNAL_TOKEN")?;
        if token.chars().count() < 32 {
            return Err(ConfigError::Invalid {
                name: "AUTH_INTERNAL_TOKEN",
                reason: "must be at least 32 characters".into(),
            });
        }
        let mut base_url = url(lookup, "AUTH_INTERNAL_URL")?;
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Ok(Self { base_url, token })
    }
}

/// Staff authentication (spec A9): JWTs issued by the Better Auth service.
#[derive(Debug, Clone)]
pub struct StaffAuthConfig {
    /// `AUTH_JWKS_URL`, e.g. `http://auth:3000/api/auth/jwks`.
    pub jwks_url: Url,
    /// `AUTH_ISSUER`, defaults to `http://auth.localhost` (the `iss` claim, A9).
    pub issuer: String,
    /// `ADMIN_ORIGIN`: the only CORS origin for `/admin/v1`, e.g. `http://admin.localhost:8080`.
    pub admin_origin: String,
}

impl StaffAuthConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let admin_origin = url(lookup, "ADMIN_ORIGIN")?;
        Ok(Self {
            jwks_url: url(lookup, "AUTH_JWKS_URL")?,
            issuer: get(lookup, "AUTH_ISSUER").unwrap_or_else(|| "http://auth.localhost".into()),
            // Origin form: scheme://host[:port], no path or trailing slash.
            admin_origin: admin_origin.origin().ascii_serialization(),
        })
    }
}

/// Not `Debug`: holds a secret.
#[derive(Clone)]
pub struct ServiceTokenConfig {
    /// `INTERNAL_API_TOKEN`: bearer token for `/internal/v1` (edge), at least 32 characters.
    pub internal_api_token: String,
}

impl ServiceTokenConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let token = required(lookup, "INTERNAL_API_TOKEN")?;
        if token.len() < 32 {
            return Err(ConfigError::Invalid {
                name: "INTERNAL_API_TOKEN",
                reason: "must be at least 32 characters".into(),
            });
        }
        Ok(Self {
            internal_api_token: token,
        })
    }
}

/// Meilisearch endpoint and key. The API gets the search-only key, the worker the admin key
/// (spec A27); the master key stays with Meilisearch. Not `Debug`: holds the key.
#[derive(Clone)]
pub struct MeiliConfig {
    /// `MEILI_URL`
    pub url: Url,
    /// `MEILI_SEARCH_KEY` (API) or `MEILI_ADMIN_KEY` (worker)
    pub key: String,
}

impl MeiliConfig {
    /// API: `MEILI_URL` + `MEILI_SEARCH_KEY`.
    pub fn search_from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env, "MEILI_SEARCH_KEY")
    }

    /// Worker: `MEILI_URL` + `MEILI_ADMIN_KEY`.
    pub fn admin_from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env, "MEILI_ADMIN_KEY")
    }

    pub fn from_lookup(lookup: Lookup, key_var: &'static str) -> Result<Self, ConfigError> {
        Ok(Self {
            url: url(lookup, "MEILI_URL")?,
            key: required(lookup, key_var)?,
        })
    }
}

/// Not `Debug`: holds the secret key.
#[derive(Clone)]
pub struct S3Config {
    /// `S3_ENDPOINT`, e.g. `http://minio:9000` locally, the R2 endpoint in prod.
    pub endpoint: Url,
    /// `S3_REGION`, defaults to `us-east-1` (MinIO ignores it, R2 wants `auto`).
    pub region: String,
    /// `S3_ACCESS_KEY_ID`
    pub access_key_id: String,
    /// `S3_SECRET_ACCESS_KEY`
    pub secret_access_key: String,
    /// `S3_BUCKET_PUBLIC`, public-read, re-encoded media only (spec A21).
    pub bucket_public: String,
    /// `S3_BUCKET_PRIVATE`
    pub bucket_private: String,
    /// `S3_PUBLIC_ENDPOINT`: the endpoint as clients reach it, used for presigned URLs
    /// (e.g. `http://s3.localhost:8080` through Caddy). Defaults to `S3_ENDPOINT`.
    pub public_endpoint: Url,
    /// `MEDIA_BASE_URL`: base URL of public-bucket objects (a CDN domain in prod). Defaults to
    /// `<S3_PUBLIC_ENDPOINT>/<S3_BUCKET_PUBLIC>/`.
    pub media_base_url: Url,
}

impl S3Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let endpoint = url(lookup, "S3_ENDPOINT")?;
        let public_endpoint = match get(lookup, "S3_PUBLIC_ENDPOINT") {
            Some(_) => url(lookup, "S3_PUBLIC_ENDPOINT")?,
            None => endpoint.clone(),
        };
        let bucket_public = required(lookup, "S3_BUCKET_PUBLIC")?;
        let media_base_url = match get(lookup, "MEDIA_BASE_URL") {
            Some(_) => url(lookup, "MEDIA_BASE_URL")?,
            None => public_endpoint
                .join(&format!("{bucket_public}/"))
                .map_err(|e| ConfigError::Invalid {
                    name: "S3_BUCKET_PUBLIC",
                    reason: e.to_string(),
                })?,
        };
        Ok(Self {
            endpoint,
            region: get(lookup, "S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            access_key_id: required(lookup, "S3_ACCESS_KEY_ID")?,
            secret_access_key: required(lookup, "S3_SECRET_ACCESS_KEY")?,
            bucket_public,
            bucket_private: required(lookup, "S3_BUCKET_PRIVATE")?,
            public_endpoint,
            media_base_url,
        })
    }
}

/// Storefront URLs and the edge purge endpoint (spec §9.3). Not `Debug`: holds a secret.
#[derive(Clone)]
pub struct StorefrontConfig {
    /// `PUBLIC_STOREFRONT_SCHEME`: `https` (default) or `http` for local `*.localhost` shops.
    pub scheme: String,
    /// `PUBLIC_STOREFRONT_PORT`: a non-default port of the public shop URLs (local dev).
    pub port: Option<u16>,
    /// `EDGE_PURGE_URL`, e.g. `http://edge:8788/_edge/purge`; unset = no purges (tests).
    pub edge_purge_url: Option<Url>,
    /// `EDGE_PURGE_TOKEN`: the edge's own purge token (distinct service token, A7).
    pub edge_purge_token: String,
}

impl StorefrontConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let scheme = get(lookup, "PUBLIC_STOREFRONT_SCHEME").unwrap_or_else(|| "https".into());
        if scheme != "https" && scheme != "http" {
            return Err(ConfigError::Invalid {
                name: "PUBLIC_STOREFRONT_SCHEME",
                reason: "must be http or https".into(),
            });
        }
        let port = match get(lookup, "PUBLIC_STOREFRONT_PORT") {
            None => None,
            Some(_) => Some(parsed(lookup, "PUBLIC_STOREFRONT_PORT", 0u16)?),
        };
        let edge_purge_url = match get(lookup, "EDGE_PURGE_URL") {
            None => None,
            Some(_) => Some(url(lookup, "EDGE_PURGE_URL")?),
        };
        let edge_purge_token = get(lookup, "EDGE_PURGE_TOKEN").unwrap_or_default();
        if edge_purge_url.is_some() && edge_purge_token.len() < 16 {
            return Err(ConfigError::Invalid {
                name: "EDGE_PURGE_TOKEN",
                reason: "must be at least 16 characters when EDGE_PURGE_URL is set".into(),
            });
        }
        Ok(Self {
            scheme,
            port,
            edge_purge_url,
            edge_purge_token,
        })
    }
}

/// Checkout integrations (WP10). Not `Debug`: it holds the fake gateway's signing key.
#[derive(Clone, PartialEq, Eq)]
pub struct CheckoutConfig {
    /// `PAYMENTS_FAKE=1`: the fake payment gateway (local and e2e only). Refused with
    /// `APP_ENV=prod`: its pay page lets anyone mark an order paid.
    pub payments_fake: bool,
    /// `PAYMENTS_FAKE_SECRET`: key for the fake provider's event signatures (at least 16
    /// characters). Unset: a random key per process (one API instance, local dev).
    pub fake_secret: Option<String>,
    /// `PACKETA_WIDGET_URL`: the pickup-point widget library (Packeta's
    /// `https://widget.packeta.com/v6/www/js/library.js` or the local mock). Unset: pickup
    /// points cannot be chosen.
    pub packeta_widget_url: Option<Url>,
    /// `PACKETA_API_KEY`: the widget's public API key.
    pub packeta_api_key: Option<String>,
}

impl CheckoutConfig {
    pub fn from_env(env: AppEnv) -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env, env)
    }

    pub fn from_lookup(lookup: Lookup, env: AppEnv) -> Result<Self, ConfigError> {
        let payments_fake = match get(lookup, "PAYMENTS_FAKE").as_deref().map(str::trim) {
            None | Some("0" | "false") => false,
            Some("1" | "true") => true,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: "PAYMENTS_FAKE",
                    reason: format!("expected 1 or 0, got {other:?}"),
                });
            }
        };
        if payments_fake && env == AppEnv::Prod {
            return Err(ConfigError::Invalid {
                name: "PAYMENTS_FAKE",
                reason: "the fake payment gateway is refused with APP_ENV=prod".into(),
            });
        }
        let fake_secret = get(lookup, "PAYMENTS_FAKE_SECRET");
        if fake_secret.as_ref().is_some_and(|s| s.len() < 16) {
            return Err(ConfigError::Invalid {
                name: "PAYMENTS_FAKE_SECRET",
                reason: "must be at least 16 characters".into(),
            });
        }
        let packeta_widget_url = match get(lookup, "PACKETA_WIDGET_URL") {
            None => None,
            Some(_) => Some(url(lookup, "PACKETA_WIDGET_URL")?),
        };
        Ok(Self {
            payments_fake,
            fake_secret,
            packeta_widget_url,
            packeta_api_key: get(lookup, "PACKETA_API_KEY"),
        })
    }
}

/// How the platform talks to Stripe (WP11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripeMode {
    /// `sk_live_…`: real money.
    Live,
    /// `sk_test_…`: Stripe's test mode (the real Payment Element with test cards).
    Test,
    /// No key: API calls go to stripe-mock and the checkout offers a signed-event simulator
    /// (local and CI only, refused with `APP_ENV=prod`).
    Simulator,
}

/// Stripe Connect settings. Not `Debug`: it holds the secret key and the webhook secret.
#[derive(Clone, PartialEq, Eq)]
pub struct StripeConfig {
    pub mode: StripeMode,
    /// `https://api.stripe.com`, `STRIPE_API_URL` to override, or `STRIPE_MOCK_URL`.
    pub api_url: Url,
    pub secret_key: String,
    /// `STRIPE_PUBLISHABLE_KEY` (real modes only): the Payment Element's key.
    pub publishable_key: Option<String>,
    /// `STRIPE_WEBHOOK_SECRET` (`whsec_…`): the Connect webhook endpoint's signing secret.
    pub webhook_secret: String,
}

/// Payment providers (WP11). Not `Debug`: secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct PaymentsConfig {
    /// `None`: Stripe is not offered.
    pub stripe: Option<StripeConfig>,
    /// `FIO_API_URL`: the Fio banka API (`https://fioapi.fio.cz`, the local mock in compose).
    pub fio_api_url: Url,
}

impl PaymentsConfig {
    pub fn from_env(env: AppEnv) -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env, env)
    }

    pub fn from_lookup(lookup: Lookup, env: AppEnv) -> Result<Self, ConfigError> {
        let invalid = |name, reason: &str| ConfigError::Invalid {
            name,
            reason: reason.into(),
        };
        let fio_api_url = match get(lookup, "FIO_API_URL") {
            Some(_) => url(lookup, "FIO_API_URL")?,
            None => Url::parse("https://fioapi.fio.cz/")
                .map_err(|e| invalid("FIO_API_URL", &e.to_string()))?,
        };
        let webhook_secret = get(lookup, "STRIPE_WEBHOOK_SECRET");
        if webhook_secret.as_ref().is_some_and(|s| s.len() < 16) {
            return Err(invalid(
                "STRIPE_WEBHOOK_SECRET",
                "must be at least 16 characters",
            ));
        }
        let stripe = if let Some(key) = get(lookup, "STRIPE_SECRET_KEY") {
            let mode = match key.split('_').take(2).collect::<Vec<_>>()[..] {
                ["sk" | "rk", "live"] => StripeMode::Live,
                ["sk" | "rk", "test"] => StripeMode::Test,
                _ => {
                    return Err(invalid(
                        "STRIPE_SECRET_KEY",
                        "expected sk_live_… or sk_test_…",
                    ));
                }
            };
            let publishable_key = required(lookup, "STRIPE_PUBLISHABLE_KEY")?;
            if !publishable_key.starts_with("pk_") {
                return Err(invalid("STRIPE_PUBLISHABLE_KEY", "expected pk_…"));
            }
            Some(StripeConfig {
                mode,
                api_url: match get(lookup, "STRIPE_API_URL") {
                    Some(_) => url(lookup, "STRIPE_API_URL")?,
                    None => Url::parse("https://api.stripe.com/")
                        .map_err(|e| invalid("STRIPE_API_URL", &e.to_string()))?,
                },
                secret_key: key,
                publishable_key: Some(publishable_key),
                webhook_secret: webhook_secret
                    .ok_or(ConfigError::Missing("STRIPE_WEBHOOK_SECRET"))?,
            })
        } else if get(lookup, "STRIPE_MOCK_URL").is_some() {
            if env == AppEnv::Prod {
                return Err(invalid(
                    "STRIPE_MOCK_URL",
                    "the Stripe simulator is refused with APP_ENV=prod",
                ));
            }
            Some(StripeConfig {
                mode: StripeMode::Simulator,
                api_url: url(lookup, "STRIPE_MOCK_URL")?,
                // stripe-mock accepts any test key.
                secret_key: "sk_test_simulator".into(),
                publishable_key: None,
                webhook_secret: webhook_secret
                    .ok_or(ConfigError::Missing("STRIPE_WEBHOOK_SECRET"))?,
            })
        } else {
            None
        };
        Ok(Self {
            stripe,
            fio_api_url,
        })
    }
}

/// Operations and integrations shared by api and worker (WP14). Not `Debug`: holds the
/// secrets key.
#[derive(Clone)]
pub struct OpsConfig {
    /// `METRICS_BIND`: the internal Prometheus listener (`/metrics`), e.g. `0.0.0.0:9100`. It
    /// is a separate port that the public proxy never routes; unset = no metrics endpoint.
    pub metrics_bind: Option<SocketAddr>,
    /// `SECRETS_KEY`: 64 hex characters (AES-256 key) encrypting stored integration secrets
    /// (webhook signing secrets, Fio API tokens). Unset = webhook subscriptions are unavailable
    /// and Fio tokens cannot be stored or used.
    pub secrets_key: Option<[u8; 32]>,
    /// `STOREFRONT_RATE_PER_SECOND` (default 20) and `STOREFRONT_RATE_BURST` (default 120):
    /// Storefront API requests per storefront token + client IP (spec §8.1).
    pub storefront_rate_per_second: u32,
    pub storefront_rate_burst: u32,
}

impl OpsConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let metrics_bind = match get(lookup, "METRICS_BIND") {
            None => None,
            Some(_) => Some(parsed(
                lookup,
                "METRICS_BIND",
                SocketAddr::from(([0, 0, 0, 0], 0)),
            )?),
        };
        let secrets_key = match get(lookup, "SECRETS_KEY") {
            None => None,
            Some(raw) => {
                let bytes = hex::decode(raw.trim())
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b).ok());
                Some(bytes.ok_or(ConfigError::Invalid {
                    name: "SECRETS_KEY",
                    reason: "must be 64 hex characters (32 random bytes)".into(),
                })?)
            }
        };
        let storefront_rate_per_second = parsed(lookup, "STOREFRONT_RATE_PER_SECOND", 20u32)?;
        let storefront_rate_burst = parsed(lookup, "STOREFRONT_RATE_BURST", 120u32)?;
        if storefront_rate_per_second == 0 || storefront_rate_burst == 0 {
            return Err(ConfigError::Invalid {
                name: "STOREFRONT_RATE_PER_SECOND",
                reason: "rate and burst must be positive".into(),
            });
        }
        Ok(Self {
            metrics_bind,
            secrets_key,
            storefront_rate_per_second,
            storefront_rate_burst,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payments(vars: &[(&str, &str)], env: AppEnv) -> Result<PaymentsConfig, ConfigError> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let lookup = move |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        PaymentsConfig::from_lookup(&lookup, env)
    }

    #[test]
    fn payments_config_modes() {
        let none = payments(&[], AppEnv::Dev).unwrap();
        assert!(none.stripe.is_none());
        assert_eq!(none.fio_api_url.as_str(), "https://fioapi.fio.cz/");

        let sim = payments(
            &[
                ("STRIPE_MOCK_URL", "http://stripe-mock:12111"),
                ("STRIPE_WEBHOOK_SECRET", "whsec_local_0123456789"),
            ],
            AppEnv::Dev,
        )
        .unwrap()
        .stripe
        .unwrap();
        assert_eq!(sim.mode, StripeMode::Simulator);
        assert_eq!(sim.api_url.as_str(), "http://stripe-mock:12111/");
        assert!(
            payments(
                &[
                    ("STRIPE_MOCK_URL", "http://stripe-mock:12111"),
                    ("STRIPE_WEBHOOK_SECRET", "whsec_local_0123456789"),
                ],
                AppEnv::Prod,
            )
            .is_err(),
            "no simulator in prod"
        );

        let real = payments(
            &[
                ("STRIPE_SECRET_KEY", "sk_live_abc"),
                ("STRIPE_PUBLISHABLE_KEY", "pk_live_abc"),
                ("STRIPE_WEBHOOK_SECRET", "whsec_0123456789abcdef"),
                ("STRIPE_MOCK_URL", "http://ignored"),
            ],
            AppEnv::Prod,
        )
        .unwrap()
        .stripe
        .unwrap();
        assert_eq!(real.mode, StripeMode::Live);
        assert_eq!(real.api_url.as_str(), "https://api.stripe.com/");
        assert!(
            payments(&[("STRIPE_SECRET_KEY", "sk_test_abc")], AppEnv::Dev).is_err(),
            "a real key needs the publishable key and the webhook secret"
        );
        assert!(payments(&[("STRIPE_SECRET_KEY", "pk_test_abc")], AppEnv::Dev).is_err());
    }
    use std::collections::HashMap;

    #[test]
    fn ops_config_validates_the_secrets_key() {
        assert!(OpsConfig::from_lookup(&env(&[("SECRETS_KEY", "abcd")])).is_err());
        let key = "11".repeat(32);
        let c = OpsConfig::from_lookup(&env(&[("SECRETS_KEY", &key)])).unwrap();
        assert_eq!(c.secrets_key, Some([0x11; 32]));
        assert!(c.metrics_bind.is_none());
        assert_eq!(
            (c.storefront_rate_per_second, c.storefront_rate_burst),
            (20, 120)
        );
    }

    #[test]
    fn checkout_config_refuses_the_fake_gateway_in_prod() {
        let fake = HashMap::from([("PAYMENTS_FAKE", "1")]);
        let lookup = |k: &str| fake.get(k).map(|v| (*v).to_owned());
        assert!(
            CheckoutConfig::from_lookup(&lookup, AppEnv::Dev)
                .unwrap()
                .payments_fake
        );
        assert!(CheckoutConfig::from_lookup(&lookup, AppEnv::Prod).is_err());
        let none = |_: &str| None;
        let c = CheckoutConfig::from_lookup(&none, AppEnv::Prod).unwrap();
        assert!(!c.payments_fake && c.packeta_widget_url.is_none());
        let bad = |k: &str| (k == "PAYMENTS_FAKE").then(|| "yes".to_owned());
        assert!(CheckoutConfig::from_lookup(&bad, AppEnv::Dev).is_err());
    }

    #[test]
    fn storefront_defaults_to_https_without_purges() {
        let cfg = StorefrontConfig::from_lookup(&env(&[])).expect("defaults");
        assert_eq!((cfg.scheme.as_str(), cfg.port), ("https", None));
        assert!(cfg.edge_purge_url.is_none());
        let cfg = StorefrontConfig::from_lookup(&env(&[
            ("PUBLIC_STOREFRONT_SCHEME", "http"),
            ("PUBLIC_STOREFRONT_PORT", "8080"),
        ]))
        .expect("valid");
        assert_eq!((cfg.scheme.as_str(), cfg.port), ("http", Some(8080)));
        assert!(
            StorefrontConfig::from_lookup(&env(&[("PUBLIC_STOREFRONT_SCHEME", "ftp")])).is_err()
        );
        assert!(
            StorefrontConfig::from_lookup(&env(&[(
                "EDGE_PURGE_URL",
                "http://edge:8788/_edge/purge"
            )]))
            .is_err(),
            "purge URL without a token"
        );
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn api_defaults_to_prod_on_port_8000() {
        let cfg = ApiConfig::from_lookup(&env(&[])).expect("defaults");
        assert_eq!(cfg.env, AppEnv::Prod);
        assert_eq!(cfg.bind.port(), 8000);
    }

    #[test]
    fn api_rejects_unknown_env() {
        let err = ApiConfig::from_lookup(&env(&[("APP_ENV", "staging")])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                name: "APP_ENV",
                ..
            }
        ));
    }

    #[test]
    fn db_requires_url_and_treats_empty_as_missing() {
        let err = DbConfig::from_lookup(&env(&[("DATABASE_URL", "")])).err();
        assert_eq!(err, Some(ConfigError::Missing("DATABASE_URL")));
    }

    #[test]
    fn db_rejects_non_numeric_pool_size() {
        let err = DbConfig::from_lookup(&env(&[
            ("DATABASE_URL", "postgres://x"),
            ("DATABASE_MAX_CONNECTIONS", "lots"),
        ]))
        .err();
        assert!(matches!(
            err,
            Some(ConfigError::Invalid {
                name: "DATABASE_MAX_CONNECTIONS",
                ..
            })
        ));
    }

    #[test]
    fn staff_auth_normalizes_origin_and_defaults_issuer() {
        let cfg = StaffAuthConfig::from_lookup(&env(&[
            ("AUTH_JWKS_URL", "http://auth:3000/api/auth/jwks"),
            ("ADMIN_ORIGIN", "http://admin.localhost:8180/"),
        ]))
        .expect("valid");
        assert_eq!(cfg.admin_origin, "http://admin.localhost:8180");
        assert_eq!(cfg.issuer, "http://auth.localhost");
    }

    #[test]
    fn service_token_must_be_long() {
        let err = ServiceTokenConfig::from_lookup(&env(&[("INTERNAL_API_TOKEN", "short")])).err();
        assert!(matches!(
            err,
            Some(ConfigError::Invalid {
                name: "INTERNAL_API_TOKEN",
                ..
            })
        ));
    }

    #[test]
    fn worker_concurrency_is_bounded() {
        assert_eq!(WorkerConfig::from_lookup(&env(&[])).unwrap().concurrency, 4);
        assert!(WorkerConfig::from_lookup(&env(&[("WORKER_CONCURRENCY", "0")])).is_err());
    }

    #[test]
    fn meili_rejects_non_http_url() {
        let err = MeiliConfig::from_lookup(
            &env(&[
                ("MEILI_URL", "file:///etc/passwd"),
                ("MEILI_SEARCH_KEY", "k"),
            ]),
            "MEILI_SEARCH_KEY",
        )
        .err();
        assert!(matches!(
            err,
            Some(ConfigError::Invalid {
                name: "MEILI_URL",
                ..
            })
        ));
        let missing = MeiliConfig::from_lookup(
            &env(&[("MEILI_URL", "http://meili:7700")]),
            "MEILI_ADMIN_KEY",
        )
        .err();
        assert_eq!(missing, Some(ConfigError::Missing("MEILI_ADMIN_KEY")));
    }

    #[test]
    fn s3_defaults_region() {
        let cfg = S3Config::from_lookup(&env(&[
            ("S3_ENDPOINT", "http://minio:9000"),
            ("S3_ACCESS_KEY_ID", "k"),
            ("S3_SECRET_ACCESS_KEY", "s"),
            ("S3_BUCKET_PUBLIC", "public"),
            ("S3_BUCKET_PRIVATE", "private"),
        ]))
        .expect("valid");
        assert_eq!(cfg.region, "us-east-1");
        assert_eq!(cfg.public_endpoint.as_str(), "http://minio:9000/");
        assert_eq!(cfg.media_base_url.as_str(), "http://minio:9000/public/");
    }

    #[test]
    fn s3_public_endpoint_and_media_url() {
        let cfg = S3Config::from_lookup(&env(&[
            ("S3_ENDPOINT", "http://minio:9000"),
            ("S3_PUBLIC_ENDPOINT", "http://s3.localhost:8080"),
            ("S3_ACCESS_KEY_ID", "k"),
            ("S3_SECRET_ACCESS_KEY", "s"),
            ("S3_BUCKET_PUBLIC", "public"),
            ("S3_BUCKET_PRIVATE", "private"),
        ]))
        .expect("valid");
        assert_eq!(
            cfg.media_base_url.as_str(),
            "http://s3.localhost:8080/public/"
        );
    }
}

#[cfg(test)]
mod auth_service_config_tests {
    use super::*;
    #[test]
    fn internal_auth_configuration_is_required_and_secret_is_validated() {
        assert!(matches!(
            AuthServiceConfig::from_lookup(&|_| None),
            Err(ConfigError::Missing("AUTH_INTERNAL_TOKEN"))
        ));
        let lookup = |key: &str| match key {
            "AUTH_INTERNAL_URL" => Some("http://auth:3000/base".into()),
            "AUTH_INTERNAL_TOKEN" => Some("x".repeat(32)),
            _ => None,
        };
        let cfg = AuthServiceConfig::from_lookup(&lookup).expect("valid configuration");
        assert_eq!(cfg.base_url.as_str(), "http://auth:3000/base/");
        assert!(matches!(
            AuthServiceConfig::from_lookup(&|key| if key == "AUTH_INTERNAL_TOKEN" {
                Some("short".into())
            } else {
                lookup(key)
            }),
            Err(ConfigError::Invalid {
                name: "AUTH_INTERNAL_TOKEN",
                ..
            })
        ));
        assert!(matches!(
            AuthServiceConfig::from_lookup(&|key| if key == "AUTH_INTERNAL_URL" {
                None
            } else {
                lookup(key)
            }),
            Err(ConfigError::Missing("AUTH_INTERNAL_URL"))
        ));
    }

    #[test]
    fn internal_auth_is_optional_for_the_server_but_never_half_configured() {
        assert!(matches!(
            AuthServiceConfig::optional_from_lookup(&|_| None),
            Ok(None)
        ));
        assert!(matches!(
            AuthServiceConfig::optional_from_lookup(
                &|key| (key == "AUTH_INTERNAL_URL").then(|| "http://auth:3000/".into())
            ),
            Err(ConfigError::Missing("AUTH_INTERNAL_TOKEN"))
        ));
        let full = |key: &str| match key {
            "AUTH_INTERNAL_URL" => Some("http://auth:3000/".into()),
            "AUTH_INTERNAL_TOKEN" => Some("x".repeat(32)),
            _ => None,
        };
        assert!(matches!(
            AuthServiceConfig::optional_from_lookup(&full),
            Ok(Some(_))
        ));
    }
}
