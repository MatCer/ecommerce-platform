//! Staff authentication for `/admin/v1` (spec §5.3, A8, A9) and the service token for
//! `/internal/v1` (spec §8.4).
//!
//! Staff JWTs come from the Better Auth service (EdDSA, `iss`, `aud=admin-api`, 5 min). The
//! JWKS is cached for 10 minutes; an unknown `kid` forces a refresh, but fetches (including
//! failed ones) happen at most every 30 s, so forged kids or an auth outage cannot hammer the
//! auth service or stall requests. Membership in the tenant named by
//! `X-Tenant-Id` is re-checked on every request through `platform.staff_membership`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use commerce::tenancy::{self, Role};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use platform::Error;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::AppState;

/// The `aud` of admin API tokens (A9).
pub const AUDIENCE: &str = "admin-api";
/// Sensitive operations need a login at most this old (A9).
pub const FRESH_AUTH: Duration = Duration::from_secs(15 * 60);
pub const TENANT_HEADER: &str = "x-tenant-id";

const JWKS_TTL: Duration = Duration::from_secs(10 * 60);
const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(30);

fn invalid_token() -> Error {
    Error::Unauthorized {
        code: "invalid_token",
    }
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, DecodingKey>,
    /// Last successful fetch.
    fetched_at: Option<Instant>,
    /// Last fetch attempt, successful or not.
    last_attempt: Option<Instant>,
    last_failed: bool,
}

/// Verifies staff JWTs against the auth service's JWKS.
pub struct StaffAuth {
    http: reqwest::Client,
    jwks_url: Url,
    validation: Validation,
    ttl: Duration,
    retry_interval: Duration,
    cache: RwLock<KeyCache>,
    /// Single flight: one JWKS fetch at a time.
    refresh: Mutex<()>,
}

impl StaffAuth {
    pub fn new(http: reqwest::Client, jwks_url: Url, issuer: &str) -> Self {
        Self::with_timing(http, jwks_url, issuer, JWKS_TTL, MIN_FETCH_INTERVAL)
    }

    /// Custom cache timings (tests): `ttl` of a good key set, minimum `retry_interval`
    /// between fetches.
    pub fn with_timing(
        http: reqwest::Client,
        jwks_url: Url,
        issuer: &str,
        ttl: Duration,
        retry_interval: Duration,
    ) -> Self {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[issuer]);
        validation.set_audience(&[AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;
        Self {
            http,
            jwks_url,
            validation,
            ttl,
            retry_interval,
            cache: RwLock::new(KeyCache::default()),
            refresh: Mutex::new(()),
        }
    }

    /// Validates signature, algorithm, `iss`, `aud`, `exp` and the verified-email claim.
    pub async fn verify(&self, token: &str) -> Result<Claims, Error> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| invalid_token())?;
        if header.alg != Algorithm::EdDSA {
            return Err(invalid_token());
        }
        let kid = header.kid.ok_or_else(invalid_token)?;
        let key = self.key(&kid).await?;
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &self.validation)
            .map_err(|_| invalid_token())?
            .claims;
        if !claims.email_verified {
            // Better Auth only issues tokens to verified users; defense in depth (A9).
            return Err(Error::Forbidden {
                code: "email_not_verified",
            });
        }
        Ok(claims)
    }

    async fn key(&self, kid: &str) -> Result<DecodingKey, Error> {
        {
            let cache = self.cache.read().await;
            if is_fresh(cache.fetched_at, self.ttl)
                && let Some(key) = cache.keys.get(kid)
            {
                return Ok(key.clone());
            }
        }

        let _single_flight = self.refresh.lock().await;
        // Every fetch, successful or not, counts against the throttle: neither unknown kids
        // nor an auth outage can make each request wait on a JWKS fetch.
        let fetch = {
            let mut cache = self.cache.write().await;
            let wanted = !is_fresh(cache.fetched_at, self.ttl) || !cache.keys.contains_key(kid);
            let allowed = !is_fresh(cache.last_attempt, self.retry_interval);
            if wanted && allowed {
                cache.last_attempt = Some(Instant::now());
            }
            wanted && allowed
        };
        if fetch {
            // No cache lock held here: requests with known, fresh keys are not blocked.
            let fetched = self.fetch().await;
            let mut cache = self.cache.write().await;
            match fetched {
                Ok(keys) => {
                    cache.keys = keys;
                    cache.fetched_at = Some(Instant::now());
                    cache.last_failed = false;
                }
                Err(e) => {
                    // Keep serving known keys while the auth service is unreachable.
                    tracing::warn!(error = %e, "JWKS refresh failed");
                    cache.last_failed = true;
                }
            }
        }
        let cache = self.cache.read().await;
        match cache.keys.get(kid) {
            Some(key) => Ok(key.clone()),
            None if cache.last_failed => Err(Error::Unavailable("JWKS unavailable".into())),
            None => Err(invalid_token()),
        }
    }

    async fn fetch(&self) -> Result<HashMap<String, DecodingKey>, String> {
        let set: JwkSet = self
            .http
            .get(self.jwks_url.clone())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(set
            .keys
            .iter()
            .filter_map(|jwk| {
                let kid = jwk.common.key_id.clone()?;
                DecodingKey::from_jwk(jwk).ok().map(|key| (kid, key))
            })
            .collect())
    }
}

fn is_fresh(at: Option<Instant>, max_age: Duration) -> bool {
    at.is_some_and(|t| t.elapsed() < max_age)
}

/// The claims the auth service puts into staff JWTs (apps/auth `definePayload`).
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    /// Better Auth user id.
    pub sub: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    /// Unix seconds of the login that created the session.
    #[serde(default)]
    pub auth_time: i64,
}

fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// An authenticated staff user (any tenant).
#[derive(Debug, Clone)]
pub struct StaffUser {
    pub user_id: String,
    pub email: String,
    pub auth_time: i64,
}

impl FromRequestParts<AppState> for StaffUser {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Error> {
        let token = bearer(parts).ok_or(Error::Unauthorized {
            code: "missing_token",
        })?;
        let claims = state.staff_auth.verify(token).await?;
        Ok(Self {
            user_id: claims.sub,
            email: claims.email,
            auth_time: claims.auth_time,
        })
    }
}

/// A staff user acting in the tenant named by `X-Tenant-Id`, with a verified membership.
#[derive(Debug, Clone)]
pub struct TenantStaff {
    pub user: StaffUser,
    pub tenant_id: Uuid,
    pub role: Role,
}

impl TenantStaff {
    /// 403 `insufficient_role` unless the member's role is at least `min`.
    pub fn require(&self, min: Role) -> Result<(), Error> {
        if self.role >= min {
            Ok(())
        } else {
            Err(Error::Forbidden {
                code: "insufficient_role",
            })
        }
    }

    /// 401 `reauth_required` unless the login is at most 15 minutes old (A9). For staff
    /// management, payment/tax settings, exports and theme publishing.
    pub fn require_fresh_auth(&self) -> Result<(), Error> {
        require_fresh(self.user.auth_time, now_unix())
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn require_fresh(auth_time: i64, now: i64) -> Result<(), Error> {
    let max_age = i64::try_from(FRESH_AUTH.as_secs()).unwrap_or(i64::MAX);
    if auth_time > 0 && now.saturating_sub(auth_time) <= max_age {
        Ok(())
    } else {
        Err(Error::Unauthorized {
            code: "reauth_required",
        })
    }
}

impl FromRequestParts<AppState> for TenantStaff {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Error> {
        let user = StaffUser::from_request_parts(parts, state).await?;
        let tenant_id = parts
            .headers
            .get(TENANT_HEADER)
            .ok_or_else(|| Error::BadRequest {
                code: "tenant_required",
                detail: "X-Tenant-Id header is required".into(),
            })?
            .to_str()
            .ok()
            .and_then(|v| Uuid::parse_str(v.trim()).ok())
            .ok_or_else(|| Error::BadRequest {
                code: "invalid_tenant_id",
                detail: "X-Tenant-Id must be a UUID".into(),
            })?;
        let role = tenancy::membership(&state.db, &user.user_id, tenant_id)
            .await?
            .ok_or(Error::Forbidden {
                code: "not_a_member",
            })?;
        Ok(Self {
            user,
            tenant_id,
            role,
        })
    }
}

/// Hash of the internal service token; comparing digests keeps the comparison independent of
/// how many leading characters of a guess are right.
#[derive(Clone)]
pub struct ServiceToken([u8; 32]);

impl ServiceToken {
    pub fn new(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// A caller holding the internal service token (edge, checkout, theme builder).
pub struct Service;

impl FromRequestParts<AppState> for Service {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Error> {
        let token = bearer(parts).ok_or(Error::Unauthorized {
            code: "missing_token",
        })?;
        if ServiceToken::new(token).0 == state.internal_token.0 {
            Ok(Self)
        } else {
            Err(invalid_token())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_auth_window() {
        let now = 1_800_000_000;
        assert!(require_fresh(now - 60, now).is_ok());
        assert!(require_fresh(now - 15 * 60, now).is_ok());
        assert_eq!(
            require_fresh(now - 15 * 60 - 1, now).unwrap_err().code(),
            "reauth_required"
        );
        assert!(require_fresh(0, now).is_err());
    }
}
