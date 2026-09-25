//! The SSRF-safe HTTP client (spec §14, A21) for merchant-supplied URLs (webhooks now, imports
//! later). It resolves DNS itself and connects only to public unicast addresses, pinning the
//! connection to the addresses it checked (no second lookup an attacker could rebind). Every
//! redirect is re-validated (at most 3), nothing credential-like is added (no cookie store, no
//! proxy, no default headers), bodies are capped at 20 MB (no transparent decompression is
//! enabled) and the whole exchange times out after 10 s.
//!
//! `allow_hosts` names hosts exempt from the address check (local mocks on the compose
//! network); configuration refuses it in production (`SAFE_HTTP_ALLOW_HOSTS`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, LOCATION};
use reqwest::{Method, StatusCode, Url};

pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_BODY: usize = 20 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum SafeError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("destination not allowed: {0}")]
    Blocked(String),
    #[error("could not resolve {0}")]
    Dns(String),
    #[error("timed out")]
    Timeout,
    #[error("response body over {MAX_BODY} bytes")]
    TooLarge,
    #[error("more than {MAX_REDIRECTS} redirects")]
    TooManyRedirects,
    #[error("request failed: {0}")]
    Http(String),
}

#[derive(Debug)]
pub struct SafeResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub url: Url,
}

#[derive(Debug, Clone)]
pub struct SafeClient {
    allow_hosts: Arc<Vec<String>>,
    timeout: Duration,
    max_body: usize,
}

impl SafeClient {
    pub fn new(allow_hosts: Vec<String>) -> Self {
        Self {
            allow_hosts: Arc::new(allow_hosts),
            timeout: TIMEOUT,
            max_body: MAX_BODY,
        }
    }

    /// Checks a URL's form without resolving it (scheme, no credentials, a host).
    pub fn check_url(raw: &str) -> Result<Url, SafeError> {
        let url = Url::parse(raw).map_err(|e| SafeError::InvalidUrl(e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(SafeError::InvalidUrl("only http and https".into()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(SafeError::InvalidUrl("credentials in the URL".into()));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(SafeError::InvalidUrl("no host".into()));
        }
        Ok(url)
    }

    /// Sends one request, following up to [`MAX_REDIRECTS`] validated redirects.
    pub async fn send(
        &self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: Option<Vec<u8>>,
    ) -> Result<SafeResponse, SafeError> {
        let deadline = Instant::now() + self.timeout;
        tokio::time::timeout(
            self.timeout,
            self.exchange(method, url, headers, body, deadline),
        )
        .await
        .map_err(|_| SafeError::Timeout)?
    }

    async fn exchange(
        &self,
        mut method: Method,
        url: &str,
        headers: HeaderMap,
        mut body: Option<Vec<u8>>,
        deadline: Instant,
    ) -> Result<SafeResponse, SafeError> {
        let mut url = Self::check_url(url)?;
        for hop in 0..=MAX_REDIRECTS {
            let addrs = self.resolve(&url).await?;
            let host = url.host_str().unwrap_or_default().to_owned();
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(deadline.saturating_duration_since(Instant::now()))
                .resolve_to_addrs(&host, &addrs)
                .build()
                .map_err(|e| SafeError::Http(e.to_string()))?;
            let mut req = client
                .request(method.clone(), url.clone())
                .headers(headers.clone());
            if let Some(b) = &body {
                req = req.body(b.clone());
            }
            let mut res = req.send().await.map_err(|e| {
                if e.is_timeout() {
                    SafeError::Timeout
                } else {
                    SafeError::Http(without_url(&e))
                }
            })?;
            let status = res.status();
            if status.is_redirection()
                && let Some(location) = res.headers().get(LOCATION)
            {
                if hop == MAX_REDIRECTS {
                    return Err(SafeError::TooManyRedirects);
                }
                let next = location
                    .to_str()
                    .ok()
                    .and_then(|l| url.join(l).ok())
                    .ok_or_else(|| SafeError::InvalidUrl("bad redirect location".into()))?;
                url = Self::check_url(next.as_str())?;
                // RFC 9110 §15.4: 303 (and, as browsers do, 301/302 after POST) become GET.
                if status == StatusCode::SEE_OTHER
                    || (matches!(status.as_u16(), 301 | 302) && method == Method::POST)
                {
                    method = Method::GET;
                    body = None;
                }
                continue;
            }
            let mut out = Vec::new();
            while let Some(chunk) = res
                .chunk()
                .await
                .map_err(|e| SafeError::Http(without_url(&e)))?
            {
                if out.len() + chunk.len() > self.max_body {
                    return Err(SafeError::TooLarge);
                }
                out.extend_from_slice(&chunk);
            }
            return Ok(SafeResponse {
                status,
                headers: res.headers().clone(),
                body: out,
                url,
            });
        }
        Err(SafeError::TooManyRedirects)
    }

    /// The addresses to connect to: all must be public unless the host is allowlisted.
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>, SafeError> {
        let host = url.host_str().unwrap_or_default();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| SafeError::InvalidUrl("no port".into()))?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let addrs: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((bare, port))
                .await
                .map_err(|_| SafeError::Dns(bare.to_owned()))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(SafeError::Dns(bare.to_owned()));
        }
        let allowed = self
            .allow_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(bare));
        // One private answer blocks the whole host: a mixed answer is a rebinding attempt.
        if !allowed && let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
            return Err(SafeError::Blocked(format!(
                "{bare} resolves to {}",
                bad.ip()
            )));
        }
        Ok(addrs)
    }
}

/// reqwest errors embed the URL; webhook URLs may carry tokens in their query.
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s.replace(u.as_str(), "<url>");
    }
    s
}

/// Public unicast only (IANA special-purpose registries for v4 and v6).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0                                   // "this network"
        || a == 10                             // private
        || a == 127                            // loopback
        || (a == 100 && (64..128).contains(&b)) // CGNAT
        || (a == 169 && b == 254)              // link-local (cloud metadata)
        || (a == 172 && (16..32).contains(&b)) // private
        || (a == 192 && b == 0 && c == 0)      // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2)      // TEST-NET-1
        || (a == 192 && b == 88 && c == 99)    // 6to4 relay anycast
        || (a == 192 && b == 168)              // private
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || (a == 198 && b == 51 && c == 100)   // TEST-NET-2
        || (a == 203 && b == 0 && c == 113)    // TEST-NET-3
        || a >= 224) // multicast, reserved, broadcast
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // Only global unicast (2000::/3) is routable on the internet.
    if s[0] & 0xe000 != 0x2000 {
        return false;
    }
    let embedded_v4 = |hi: u16, lo: u16| {
        let [a, b] = hi.to_be_bytes();
        let [c, d] = lo.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    match s[0] {
        // 2001::/23 IETF protocol assignments (Teredo, benchmarking, ORCHID, ...) and
        // 2001:db8::/32 documentation.
        0x2001 if s[1] < 0x0200 || s[1] == 0x0db8 => false,
        // 6to4: the tunnel endpoint is the embedded IPv4 address.
        0x2002 => is_public_v4(embedded_v4(s[1], s[2])),
        // 3fff::/20 documentation.
        0x3fff if s[1] < 0x1000 => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_special_purpose_addresses() {
        for private in [
            "0.0.0.0",
            "10.1.2.3",
            "127.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2002:0a00:0001::1",
            "64:ff9b::a00:1",
            "3fff::1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "::ffff:8.8.8.8",
            "2606:4700:4700::1111",
            "2a00:1450:4001::1",
            "2002:0808:0808::1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn refuses_bad_urls() {
        for bad in [
            "ftp://example.com/",
            "http://user:pw@example.com/",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(SafeClient::check_url(bad).is_err(), "{bad}");
        }
        assert!(SafeClient::check_url("https://example.com/hook?x=1").is_ok());
    }

    #[tokio::test]
    async fn blocks_private_destinations_before_connecting() {
        let client = SafeClient::new(vec![]);
        for url in [
            "http://127.0.0.1:1/",
            "http://[::1]:1/",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:1/",
        ] {
            let err = client
                .send(Method::GET, url, HeaderMap::new(), None)
                .await
                .unwrap_err();
            assert!(matches!(err, SafeError::Blocked(_)), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn follows_redirects_and_revalidates_each_hop() {
        use axum::Router;
        use axum::http::{HeaderValue, StatusCode as S};
        use axum::routing::{any, get};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let to = move |path: &'static str, status: S| {
            move || async move {
                let mut res = axum::response::Response::new(axum::body::Body::empty());
                *res.status_mut() = status;
                let loc = path.replace("PORT", &port.to_string());
                res.headers_mut()
                    .insert("location", HeaderValue::from_str(&loc).unwrap());
                res
            }
        };
        let app = Router::new()
            .route(
                "/ok",
                any(|m: axum::http::Method| async move { m.to_string() }),
            )
            .route("/hop", any(to("http://localhost:PORT/ok", S::FOUND)))
            .route("/see-other", any(to("/ok", S::SEE_OTHER)))
            .route("/keep", any(to("/ok", S::TEMPORARY_REDIRECT)))
            .route("/private", get(to("http://127.0.0.1:PORT/ok", S::FOUND)))
            .route("/loop", get(to("/loop", S::FOUND)))
            .route("/big", get(|| async { vec![b'x'; MAX_BODY + 1] }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = SafeClient::new(vec!["localhost".into()]);
        let base = format!("http://localhost:{port}");
        let send = |m: Method, path: &str| {
            let url = format!("{base}{path}");
            let client = client.clone();
            async move {
                client
                    .send(m, &url, HeaderMap::new(), Some(b"{}".to_vec()))
                    .await
            }
        };
        let ok = send(Method::GET, "/hop").await.unwrap();
        assert_eq!(ok.status, StatusCode::OK);
        assert_eq!(ok.body, b"GET");
        assert_eq!(send(Method::POST, "/see-other").await.unwrap().body, b"GET");
        assert_eq!(send(Method::POST, "/keep").await.unwrap().body, b"POST");
        let private = send(Method::GET, "/private").await;
        assert!(matches!(private, Err(SafeError::Blocked(_))), "{private:?}");
        let looped = send(Method::GET, "/loop").await;
        assert!(
            matches!(looped, Err(SafeError::TooManyRedirects)),
            "{looped:?}"
        );
        let big = send(Method::GET, "/big").await;
        assert!(matches!(big, Err(SafeError::TooLarge)), "{big:?}");
    }
}
