//! Storefront API rate limits (spec §8.1): a GCRA bucket per storefront token + client IP.
//! The API is reached only through the edge (A4), which sets `X-Storefront-Token` itself and
//! forwards the client address in `X-Client-Ip` (the last `X-Forwarded-For` hop, appended by
//! the proxy in front of it); calls without it share one bucket per token. Over the limit:
//! `429 rate_limited` as problem+json with `Retry-After`.
//!
//! ponytail: in-process buckets, so N API replicas allow N times the rate; move to a shared
//! store (or the CDN's rate limiting in prod) when the API scales out.

use std::num::NonZeroU32;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use platform::Error;

use crate::AppState;
use crate::storefront::TOKEN_HEADER;
use crate::storefront::customer::CLIENT_IP_HEADER;

pub struct StorefrontLimiter {
    limiter: DefaultKeyedRateLimiter<(String, String)>,
    clock: DefaultClock,
}

impl StorefrontLimiter {
    pub fn new(per_second: u32, burst: u32) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(per_second).unwrap_or(NonZeroU32::MIN))
            .allow_burst(NonZeroU32::new(burst).unwrap_or(NonZeroU32::MIN));
        Self {
            limiter: RateLimiter::keyed(quota),
            clock: DefaultClock::default(),
        }
    }

    /// `Err(wait)` when the caller is over the limit.
    pub fn check(&self, token: &str, ip: &str) -> Result<(), Duration> {
        // Bounded key sizes: both come from the edge, but never trust lengths.
        let key = (
            token.chars().take(128).collect(),
            ip.chars().take(64).collect(),
        );
        self.limiter
            .check_key(&key)
            .map_err(|not_until| not_until.wait_time_from(self.clock.now()))
    }

    /// Forgets idle buckets (call periodically; bounds memory).
    pub fn prune(&self) {
        self.limiter.retain_recent();
        self.limiter.shrink_to_fit();
    }
}

/// Middleware: applies to `/storefront/*` only.
pub async fn storefront(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if !req.uri().path().starts_with("/storefront/") {
        return next.run(req).await;
    }
    let verdict = {
        let header = |name: &str| {
            req.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
        };
        s.rate_limit
            .check(header(TOKEN_HEADER), header(CLIENT_IP_HEADER))
    };
    match verdict {
        Ok(()) => next.run(req).await,
        Err(wait) => {
            let mut res = Error::TooManyRequests {
                code: "rate_limited",
            }
            .into_response();
            let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
            res.headers_mut()
                .insert("retry-after", HeaderValue::from(secs.max(1)));
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_per_token_and_ip() {
        let l = StorefrontLimiter::new(1, 2);
        assert!(l.check("t", "1.1.1.1").is_ok());
        assert!(l.check("t", "1.1.1.1").is_ok());
        let wait = l.check("t", "1.1.1.1").unwrap_err();
        assert!(wait > Duration::ZERO && wait <= Duration::from_secs(1));
        assert!(l.check("t", "2.2.2.2").is_ok(), "another IP");
        assert!(l.check("u", "1.1.1.1").is_ok(), "another token");
        l.prune();
    }
}
