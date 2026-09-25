//! Shared harness for the API integration tests: a local JWKS server signing like the auth
//! service, app state on the runtime role, and a small request builder.
#![allow(dead_code, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api::auth::StaffAuth;
use api::{AppState, app};
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::RwLock;
use tower::ServiceExt;
use uuid::Uuid;

pub const KEY_A: &[u8] = include_bytes!("../fixtures/ed25519-a.pem");
pub const KEY_B: &[u8] = include_bytes!("../fixtures/ed25519-b.pem");
pub const X_A: &str = "jRjc3p_3v4VZPmn7aKgo7SMpp-_rC2rNC5hUa2inKqA";
pub const X_B: &str = "21DQfDet_r7Z-araVddfSoCqc9Af3EhRfOgBskuJub0";
pub const ISSUER: &str = "http://auth.localhost";
pub const ADMIN_ORIGIN: &str = "http://admin.localhost:8180";
pub const SERVICE_TOKEN: &str = "internal-test-token-0123456789abcdef";

pub fn jwk(kid: &str, x: &str) -> Value {
    json!({ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": kid, "x": x })
}

/// A JWKS endpoint like Better Auth's `/api/auth/jwks`, with a hit counter.
pub struct Jwks {
    pub url: String,
    pub keys: Arc<RwLock<Value>>,
    pub hits: Arc<AtomicUsize>,
    pub auth_requests: Arc<RwLock<Vec<(String, String, Value)>>>,
    /// 0 succeeds, 1 fails ensure-user, 2 fails invite.
    pub auth_failure: Arc<AtomicUsize>,
}

pub async fn jwks_server(keys: Value) -> Jwks {
    let keys = Arc::new(RwLock::new(keys));
    let hits = Arc::new(AtomicUsize::new(0));
    let (k, h) = (keys.clone(), hits.clone());
    let router = axum::Router::new().route(
        "/jwks",
        axum::routing::get(move || {
            let (k, h) = (k.clone(), h.clone());
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                let keys = k.read().await.clone();
                // `null` keys simulate an auth service outage.
                if keys.is_null() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                axum::Json(json!({ "keys": keys })).into_response()
            }
        }),
    );
    let auth_requests = Arc::new(RwLock::new(Vec::new()));
    let auth_failure = Arc::new(AtomicUsize::new(0));
    let mut router = router;
    for (path, stage) in [("/internal/users", 1), ("/internal/users/invite", 2)] {
        let requests = auth_requests.clone();
        let failure = auth_failure.clone();
        router = router.route(
            path,
            axum::routing::post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let requests = requests.clone();
                    let failure = failure.clone();
                    async move {
                        let token = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        requests
                            .write()
                            .await
                            .push((path.to_owned(), token, body.clone()));
                        if failure.load(Ordering::SeqCst) == stage {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        axum::Json(
                            json!({"id": format!("auth:{}", body["email"].as_str().unwrap())}),
                        )
                        .into_response()
                    }
                },
            ),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/jwks", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Jwks {
        url,
        keys,
        hits,
        auth_requests,
        auth_failure,
    }
}

pub fn state(db: PgPool, jwks: &Jwks, forced_interval: Duration) -> AppState {
    AppState {
        db,
        auth_service: Some(
            api::auth_service::AuthService::new(jwks.url.parse().unwrap(), SERVICE_TOKEN.into())
                .unwrap(),
        ),
        http: reqwest::Client::new(),
        meili: commerce::search::Meili::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1".parse().unwrap(),
            "unused".into(),
            Duration::from_secs(1),
        ),
        storage: testkit::memory_storage(),
        staff_auth: Arc::new(StaffAuth::with_timing(
            reqwest::Client::new(),
            jwks.url.parse().unwrap(),
            ISSUER,
            Duration::from_secs(600),
            forced_interval,
        )),
        internal_token: api::auth::ServiceToken::new(SERVICE_TOKEN),
        admin_origin: HeaderValue::from_static(ADMIN_ORIGIN),
        public_urls: commerce::storefront::PublicUrls {
            scheme: "http".into(),
            port: Some(8080),
        },
        edge: api::edge::EdgePurge::disabled(),
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn claims(sub: &str) -> Value {
    json!({
        "sub": sub,
        "email": format!("{sub}@example.test"),
        "email_verified": true,
        "auth_time": now() - 60,
        "iss": ISSUER,
        "aud": "admin-api",
        "iat": now(),
        "exp": now() + 300,
    })
}

pub fn sign_with(kid: &str, pem: &[u8], claims: &Value) -> String {
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(kid.into());
    jsonwebtoken::encode(&header, claims, &EncodingKey::from_ed_pem(pem).unwrap()).unwrap()
}

pub fn sign(claims: &Value) -> String {
    sign_with("a", KEY_A, claims)
}

pub struct Call<'a> {
    method: &'a str,
    uri: &'a str,
    token: Option<&'a str>,
    tenant: Option<Uuid>,
    body: Option<Value>,
    idempotency_key: Option<&'a str>,
    headers: Vec<(&'a str, String)>,
}

impl<'a> Call<'a> {
    pub fn get(uri: &'a str) -> Self {
        Self {
            method: "GET",
            uri,
            token: None,
            tenant: None,
            body: None,
            idempotency_key: None,
            headers: Vec::new(),
        }
    }

    pub fn patch(uri: &'a str, body: Value) -> Self {
        Self {
            method: "PATCH",
            body: Some(body),
            ..Self::get(uri)
        }
    }

    pub fn header(mut self, name: &'a str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn post(uri: &'a str, body: Value) -> Self {
        Self {
            method: "POST",
            body: Some(body),
            ..Self::get(uri)
        }
    }

    pub fn patch(uri: &'a str, body: Value) -> Self {
        Self {
            method: "PATCH",
            body: Some(body),
            ..Self::get(uri)
        }
    }

    pub fn put(uri: &'a str, body: Value) -> Self {
        Self {
            method: "PUT",
            body: Some(body),
            ..Self::get(uri)
        }
    }

    pub fn delete(uri: &'a str) -> Self {
        Self {
            method: "DELETE",
            ..Self::get(uri)
        }
    }

    pub fn token(self, token: &'a str) -> Self {
        Self {
            token: Some(token),
            ..self
        }
    }

    pub fn tenant(self, tenant: Uuid) -> Self {
        Self {
            tenant: Some(tenant),
            ..self
        }
    }

    pub fn key(self, key: &'a str) -> Self {
        Self {
            idempotency_key: Some(key),
            ..self
        }
    }

    pub async fn send(self, state: &AppState) -> (StatusCode, Value, Response) {
        let (status, text, res) = self.send_text(state).await;
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, json, res)
    }

    /// Like [`Call::send`], with the body as text.
    pub async fn send_text(self, state: &AppState) -> (StatusCode, String, Response) {
        let mut req = Request::builder().method(self.method).uri(self.uri);
        if let Some(t) = self.token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(t) = self.tenant {
            req = req.header("x-tenant-id", t.to_string());
        }
        if let Some(k) = self.idempotency_key {
            req = req.header("idempotency-key", k);
        }
        for (name, value) in &self.headers {
            req = req.header(*name, value.as_str());
        }
        let body = match self.body {
            Some(b) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = app(state.clone(), false)
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let (parts, body) = res.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (status, text, Response::from_parts(parts, Body::empty()))
    }
}
