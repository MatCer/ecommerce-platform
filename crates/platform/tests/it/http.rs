//! SSRF-safe client (A21) against a local server: loopback is blocked unless allowlisted,
//! redirects are re-validated, bodies are capped.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::response::Redirect;
use axum::routing::get;
use platform::http::{FetchError, Limits, SafeClient};

async fn serve() -> u16 {
    let app = Router::new()
        .route("/small", get(|| async { "hello" }))
        .route("/big", get(|| async { "x".repeat(4096) }))
        .route(
            "/to/{target}",
            get(|Path(t): Path<String>| async move { Redirect::temporary(&t.replace('~', "/")) }),
        )
        .route(
            "/loop/{n}",
            get(|Path(n): Path<u32>| async move { Redirect::temporary(&format!("/loop/{}", n + 1)) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

const LIMITS: Limits = Limits {
    max_bytes: 1024,
    timeout: Duration::from_secs(5),
};

#[tokio::test]
async fn loopback_is_blocked_by_default() {
    let port = serve().await;
    let client = SafeClient::new(Vec::<String>::new()).unwrap();
    for url in [
        format!("http://127.0.0.1:{port}/small"),
        format!("http://localhost:{port}/small"),
    ] {
        let err = client.get(&url, LIMITS).await.unwrap_err();
        assert!(matches!(err, FetchError::Blocked(_)), "{url}: {err:?}");
    }
}

#[tokio::test]
async fn allowlisted_hosts_are_fetched_within_limits() {
    let port = serve().await;
    let client = SafeClient::new(["localhost".to_owned()]).unwrap();
    let ok = client
        .get(&format!("http://localhost:{port}/small"), LIMITS)
        .await
        .unwrap();
    assert_eq!(ok.bytes, b"hello");
    let big = client
        .get(&format!("http://localhost:{port}/big"), LIMITS)
        .await
        .unwrap_err();
    assert!(matches!(big, FetchError::TooLarge(1024)), "{big:?}");
}

#[tokio::test]
async fn redirects_are_revalidated_and_bounded() {
    let port = serve().await;
    let client = SafeClient::new(["localhost".to_owned()]).unwrap();
    // localhost is allowed, but the redirect goes to a loopback IP literal: blocked.
    let target = format!("http:~~127.0.0.1:{port}~small");
    let err = client
        .get(&format!("http://localhost:{port}/to/{target}"), LIMITS)
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Blocked(_)), "{err:?}");
    // A redirect to a non-http scheme.
    let err = client
        .get(
            &format!("http://localhost:{port}/to/file:~~~etc~passwd"),
            LIMITS,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            FetchError::InvalidUrl | FetchError::Request(_) | FetchError::Status(307)
        ),
        "{err:?}"
    );
    let err = client
        .get(&format!("http://localhost:{port}/loop/0"), LIMITS)
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Request(_)), "{err:?}");
    // Within the limit (3 redirects) it follows.
    let ok = client
        .get(&format!("http://localhost:{port}/to/~small"), LIMITS)
        .await
        .unwrap();
    assert_eq!(ok.bytes, b"hello");
}

#[tokio::test]
async fn bad_urls_are_refused_before_any_request() {
    let client = SafeClient::new(Vec::<String>::new()).unwrap();
    for url in [
        "ftp://example.com/x",
        "https://user:pw@example.com/",
        "not a url",
    ] {
        let err = client.get(url, LIMITS).await.unwrap_err();
        assert!(matches!(err, FetchError::InvalidUrl), "{url}: {err:?}");
    }
}
