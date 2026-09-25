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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

/// Which model provider serves the AI helpers.
#[derive(Clone, PartialEq, Eq)]
pub enum AiProvider {
    /// The Anthropic Messages API with this key.
    Anthropic { api_key: String },
    /// Deterministic fixtures (tests, local stacks without a key; never with `APP_ENV=prod`).
    Fake,
    /// No key in production: AI endpoints answer `503 ai_unavailable`.
    Disabled,
}

/// AI gateway (spec D21, §12.1). Not `Debug`: holds the API key.
#[derive(Clone)]
pub struct AiConfig {
    /// `ANTHROPIC_API_KEY` set: Anthropic. Unset: the fake provider, or `Disabled` with
    /// `APP_ENV=prod` (fixture text must never reach a real shop).
    pub provider: AiProvider,
    /// `ANTHROPIC_BASE_URL`, default `https://api.anthropic.com/`.
    pub base_url: Url,
    /// `AI_HELPER_MODEL`, default `claude-sonnet-5` (admin helpers, bulk plans).
    pub helper_model: String,
    /// `AI_THEME_MODEL`, default `claude-opus-5-5` (AI theme editing, WP24).
    pub theme_model: String,
    /// `AI_TIMEOUT_SECS` per attempt, default 90.
    pub timeout: std::time::Duration,
    /// `AI_MAX_RETRIES` on 408/429/5xx/529 and network errors, default 2.
    pub max_retries: u32,
    /// `AI_PRICES`: `model=input:output;...` USD per MTok over the built-in list prices.
    pub prices: crate::ai::PriceTable,
    /// `AI_PLAN_QUOTAS`: `plan=tokens;...` monthly tokens per tenant plan, default
    /// `standard=2000000`. Plans not listed get the `standard` quota.
    pub plan_quotas: std::collections::BTreeMap<String, i64>,
}

impl AiConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let env: AppEnv = parsed(lookup, "APP_ENV", AppEnv::Prod)?;
        let provider = match get(lookup, "ANTHROPIC_API_KEY") {
            Some(api_key) => AiProvider::Anthropic {
                api_key: api_key.trim().to_owned(),
            },
            None if env == AppEnv::Prod => AiProvider::Disabled,
            None => AiProvider::Fake,
        };
        let base_url = match get(lookup, "ANTHROPIC_BASE_URL") {
            None => Url::parse("https://api.anthropic.com/").map_err(|e| ConfigError::Invalid {
                name: "ANTHROPIC_BASE_URL",
                reason: e.to_string(),
            })?,
            Some(_) => {
                let mut u = url(lookup, "ANTHROPIC_BASE_URL")?;
                if !u.path().ends_with('/') {
                    u.set_path(&format!("{}/", u.path()));
                }
                u
            }
        };
        let prices = match get(lookup, "AI_PRICES") {
            None => crate::ai::PriceTable::default(),
            Some(spec) => {
                crate::ai::PriceTable::parse(&spec).map_err(|reason| ConfigError::Invalid {
                    name: "AI_PRICES",
                    reason,
                })?
            }
        };
        let mut plan_quotas =
            std::collections::BTreeMap::from([("standard".to_owned(), 2_000_000)]);
        for entry in get(lookup, "AI_PLAN_QUOTAS")
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            let invalid = || ConfigError::Invalid {
                name: "AI_PLAN_QUOTAS",
                reason: format!("{entry:?}: expected plan=tokens"),
            };
            let (plan, tokens) = entry.split_once('=').ok_or_else(invalid)?;
            let tokens: i64 = tokens.trim().parse().map_err(|_| invalid())?;
            if tokens < 0 {
                return Err(invalid());
            }
            plan_quotas.insert(plan.trim().to_owned(), tokens);
        }
        let model = |name: &'static str, default: &str| {
            let m = get(lookup, name).unwrap_or_else(|| default.into());
            let ok = (1..=100).contains(&m.len())
                && m.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b':')
                });
            ok.then_some(m).ok_or(ConfigError::Invalid {
                name,
                reason: "expected a model id".into(),
            })
        };
        Ok(Self {
            provider,
            base_url,
            helper_model: model("AI_HELPER_MODEL", "claude-sonnet-5")?,
            theme_model: model("AI_THEME_MODEL", "claude-opus-5-5")?,
            timeout: std::time::Duration::from_secs(parsed(lookup, "AI_TIMEOUT_SECS", 90u64)?),
            max_retries: parsed(lookup, "AI_MAX_RETRIES", 2u32)?,
            prices,
            plan_quotas,
        })
    }
}

#[cfg(test)]
mod ai_config_tests {
    use super::*;

    #[test]
    fn provider_follows_key_and_environment() {
        let dev = |k: &str| (k == "APP_ENV").then(|| "dev".to_owned());
        let cfg = AiConfig::from_lookup(&dev).unwrap();
        assert!(cfg.provider == AiProvider::Fake);
        assert_eq!(cfg.helper_model, "claude-sonnet-5");
        assert_eq!(cfg.theme_model, "claude-opus-5-5");
        assert_eq!(cfg.plan_quotas["standard"], 2_000_000);
        assert_eq!(cfg.base_url.as_str(), "https://api.anthropic.com/");
        // Production without a key never falls back to fixtures.
        assert!(AiConfig::from_lookup(&|_| None).unwrap().provider == AiProvider::Disabled);
        let keyed = |k: &str| match k {
            "ANTHROPIC_API_KEY" => Some("sk-test".into()),
            "AI_PLAN_QUOTAS" => Some("pro=9000000; free=0".into()),
            "ANTHROPIC_BASE_URL" => Some("http://127.0.0.1:9/base".into()),
            _ => None,
        };
        let cfg = AiConfig::from_lookup(&keyed).unwrap();
        assert!(matches!(cfg.provider, AiProvider::Anthropic { .. }));
        assert_eq!(cfg.plan_quotas["pro"], 9_000_000);
        assert_eq!(cfg.plan_quotas["free"], 0);
        assert_eq!(cfg.base_url.as_str(), "http://127.0.0.1:9/base/");
        let bad = |k: &str| (k == "AI_PLAN_QUOTAS").then(|| "pro=lots".to_owned());
        assert!(AiConfig::from_lookup(&bad).is_err());
        let bad_model = |k: &str| (k == "AI_HELPER_MODEL").then(|| "a b".to_owned());
        assert!(AiConfig::from_lookup(&bad_model).is_err());
    }
}
