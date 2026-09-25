//! The one HTTP client for merchant-supplied URLs (spec §14, A21): feed and image imports,
//! later webhooks.
//!
//! - It resolves DNS itself and connects only to public unicast addresses (v4 + v6). reqwest
//!   connects to exactly the addresses the resolver returned, so a name cannot be re-bound to
//!   a private address between the check and the connection.
//! - IP-literal hosts are checked the same way; every redirect (max 3) is re-validated.
//! - Only `http`/`https`, no userinfo, no proxies, no cookies, no decompression (the body is
//!   counted as sent), a byte cap and a total timeout.
//! - `allow_hosts` (dev only, `SAFE_FETCH_ALLOW_HOSTS`) names hosts that may resolve to
//!   private addresses, e.g. the `mocks` service serving fixtures.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Url, redirect};

pub const MAX_REDIRECTS: usize = 3;

/// Caps of one fetch.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: u64,
    pub timeout: Duration,
}

impl Limits {
    /// A21 defaults (images): 20 MB, 10 s.
    pub const IMAGE: Self = Self {
        max_bytes: 20 * 1024 * 1024,
        timeout: Duration::from_secs(10),
    };
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("only http and https URLs without credentials are allowed")]
    InvalidUrl,
    #[error("the address of {0} is not a public internet address")]
    Blocked(String),
    #[error("the response is larger than {0} bytes")]
    TooLarge(u64),
    #[error("the server answered {0}")]
    Status(u16),
    #[error("request failed: {0}")]
    Request(String),
}

/// A downloaded body with its declared content type.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    /// The URL after redirects.
    pub url: Url,
}

#[derive(Clone)]
pub struct SafeClient {
    http: reqwest::Client,
    allow_hosts: Arc<BTreeSet<String>>,
}

/// A public unicast address: not loopback, private, link-local, shared (CGNAT), multicast,
/// documentation, benchmarking or reserved; IPv6 must be global unicast (2000::/3), and
/// IPv4-mapped/compatible/NAT64 addresses are judged by their IPv4 part.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_v4(v4);
            }
            // ::a.b.c.d (deprecated compatible) and 64:ff9b::/96 (NAT64)
            if (s[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified())
                || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0]
            {
                let [a, b] = s[6].to_be_bytes();
                let [c, d] = s[7].to_be_bytes();
                return public_v4(Ipv4Addr::new(a, b, c, d));
            }
            let global_unicast = (s[0] & 0xe000) == 0x2000;
            let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
            // 2001::/23 holds Teredo, benchmarking, ORCHID and other special ranges.
            let special = s[0] == 0x2001 && s[1] < 0x0200;
            // 2002::/16 (6to4) embeds an IPv4 address.
            let six_to_four = s[0] == 0x2002;
            global_unicast && !documentation && !special && !six_to_four && !is_v6_local(v6)
        }
    }
}

fn is_v6_local(v6: Ipv6Addr) -> bool {
    v6.is_loopback() || v6.is_unspecified() || v6.is_multicast()
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared address space (CGNAT)
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || a >= 240) // reserved + broadcast
}

/// Resolves with the system resolver and drops every non-public address (fails when none is
/// left), except for allowlisted hosts.
struct PublicResolver {
    allow_hosts: Arc<BTreeSet<String>>,
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let allowed = self.allow_hosts.contains(&host);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| allowed || is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(Box::new(FetchError::Blocked(host)) as _);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

impl SafeClient {
    pub fn new(allow_hosts: impl IntoIterator<Item = String>) -> Result<Self, FetchError> {
        let allow_hosts: Arc<BTreeSet<String>> = Arc::new(
            allow_hosts
                .into_iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
        );
        let redirect_hosts = allow_hosts.clone();
        let http = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(PublicResolver {
                allow_hosts: allow_hosts.clone(),
            }))
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    attempt.error(FetchError::Request("too many redirects".into()))
                } else if let Err(e) = check_url(attempt.url(), &redirect_hosts) {
                    attempt.error(e)
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("commerce-platform-fetch/1")
            .build()
            .map_err(|e| FetchError::Request(e.to_string()))?;
        Ok(Self { http, allow_hosts })
    }

    /// `SAFE_FETCH_ALLOW_HOSTS` (comma-separated), honored only with `APP_ENV=dev`: the local
    /// stack serves fixture feeds and images from the `mocks` service on a private address.
    pub fn from_env() -> Result<Self, FetchError> {
        let hosts = std::env::var("SAFE_FETCH_ALLOW_HOSTS").unwrap_or_default();
        let dev = std::env::var("APP_ENV").is_ok_and(|e| e == "dev");
        if !dev && !hosts.trim().is_empty() {
            tracing::warn!("SAFE_FETCH_ALLOW_HOSTS is ignored outside APP_ENV=dev");
            return Self::new(Vec::<String>::new());
        }
        Self::new(hosts.split(',').map(str::to_owned).collect::<Vec<_>>())
    }

    /// GETs `url` within `limits`. Non-2xx answers are errors.
    pub async fn get(&self, url: &str, limits: Limits) -> Result<Fetched, FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
        check_url(&url, &self.allow_hosts)?;
        let mut res = self
            .http
            .get(url)
            .timeout(limits.timeout)
            .send()
            .await
            .map_err(request_error)?;
        if !res.status().is_success() {
            return Err(FetchError::Status(res.status().as_u16()));
        }
        if res.content_length().is_some_and(|l| l > limits.max_bytes) {
            return Err(FetchError::TooLarge(limits.max_bytes));
        }
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let final_url = res.url().clone();
        let mut bytes = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(request_error)? {
            if (bytes.len() + chunk.len()) as u64 > limits.max_bytes {
                return Err(FetchError::TooLarge(limits.max_bytes));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Fetched {
            bytes,
            content_type,
            url: final_url,
        })
    }
}

/// A blocked address surfaces from the resolver or redirect policy wrapped in reqwest errors;
/// report it as such instead of a generic failure.
fn request_error(e: reqwest::Error) -> FetchError {
    let mut source: Option<&dyn std::error::Error> = Some(&e);
    while let Some(s) = source {
        if let Some(f) = s.downcast_ref::<FetchError>() {
            return match f {
                FetchError::Blocked(h) => FetchError::Blocked(h.clone()),
                FetchError::InvalidUrl => FetchError::InvalidUrl,
                other => FetchError::Request(other.to_string()),
            };
        }
        source = s.source();
    }
    if e.is_timeout() {
        return FetchError::Request("timed out".into());
    }
    // Without the URL: it may carry merchant tokens in its query string.
    FetchError::Request(e.without_url().to_string())
}

/// Scheme, credentials and IP-literal hosts (names are checked by the resolver).
fn check_url(url: &Url, allow_hosts: &BTreeSet<String>) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::InvalidUrl);
    }
    let host = url.host_str().ok_or(FetchError::InvalidUrl)?;
    // The URL parser normalizes IP literals (`0x7f000001` -> `127.0.0.1`, `[::1]`).
    let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    else {
        return Ok(());
    };
    let literal = ip.to_string();
    if allow_hosts.contains(&literal) || is_public(ip) {
        Ok(())
    } else {
        Err(FetchError::Blocked(literal))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn public_addresses() {
        for ok in [
            "8.8.8.8",
            "1.1.1.1",
            "2a00:1450:4001:82a::200e",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(ok.parse().unwrap()), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "240.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2001::1",
        ] {
            assert!(!is_public(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn urls() {
        let none = BTreeSet::new();
        let check = |u: &str| check_url(&Url::parse(u).unwrap(), &none);
        assert!(check("https://shop.example/feed.xml").is_ok());
        assert!(check("http://8.8.8.8/x").is_ok());
        for bad in [
            "ftp://shop.example/feed.xml",
            "file:///etc/passwd",
            "https://user:pw@shop.example/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
            "http://0x7f000001/",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }
}
