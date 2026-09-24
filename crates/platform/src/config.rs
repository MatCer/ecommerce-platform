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

#[derive(Debug, Clone)]
pub struct MeiliConfig {
    /// `MEILI_URL`
    pub url: Url,
}

impl MeiliConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        Ok(Self {
            url: url(lookup, "MEILI_URL")?,
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
}

impl S3Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&process_env)
    }

    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        Ok(Self {
            endpoint: url(lookup, "S3_ENDPOINT")?,
            region: get(lookup, "S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            access_key_id: required(lookup, "S3_ACCESS_KEY_ID")?,
            secret_access_key: required(lookup, "S3_SECRET_ACCESS_KEY")?,
            bucket_public: required(lookup, "S3_BUCKET_PUBLIC")?,
            bucket_private: required(lookup, "S3_BUCKET_PRIVATE")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        let err =
            MeiliConfig::from_lookup(&env(&[("MEILI_URL", "file:///etc/passwd")])).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                name: "MEILI_URL",
                ..
            }
        ));
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
    }
}
