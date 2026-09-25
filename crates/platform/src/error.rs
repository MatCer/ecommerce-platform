//! API error type rendered as RFC 9457 `application/problem+json` (spec §8.1).
//!
//! `code` is the stable, machine-readable identifier clients branch on; `title` and `detail`
//! are for humans and may change. Internal errors are logged server-side and never leak detail.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("resource not found")]
    NotFound,
    #[error("method not allowed")]
    MethodNotAllowed,
    #[error("request body too large")]
    PayloadTooLarge,
    /// Malformed request outside the body (headers, query). `code` is stable snake_case.
    #[error("{detail}")]
    BadRequest { code: &'static str, detail: String },
    /// Missing or invalid credentials (`invalid_token`, `reauth_required`, ...).
    #[error("unauthorized: {code}")]
    Unauthorized { code: &'static str },
    /// Authenticated but not allowed (`not_a_member`, `insufficient_role`, ...).
    #[error("forbidden: {code}")]
    Forbidden { code: &'static str },
    /// State conflict (`idempotency_conflict`, `already_exists`, ...).
    #[error("{detail}")]
    Conflict { code: &'static str, detail: String },
    /// Input failed validation. `code` is a stable snake_case identifier.
    #[error("{detail}")]
    Validation { code: &'static str, detail: String },
    /// A rate limit was hit (`too_many_attempts`, ...).
    #[error("too many requests: {code}")]
    TooManyRequests { code: &'static str },
    #[error("service unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Storage(#[from] object_store::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BadRequest { .. } => StatusCode::BAD_REQUEST,
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database(_) | Self::Storage(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::PayloadTooLarge => "payload_too_large",
            Self::BadRequest { code, .. }
            | Self::Unauthorized { code }
            | Self::Forbidden { code }
            | Self::TooManyRequests { code }
            | Self::Conflict { code, .. }
            | Self::Validation { code, .. } => code,
            Self::Unavailable(_) => "service_unavailable",
            Self::Database(_) | Self::Storage(_) | Self::Internal(_) => "internal_error",
        }
    }

    /// Detail safe to show to clients. Server-side failures expose nothing.
    fn public_detail(&self) -> Option<String> {
        match self {
            Self::NotFound
            | Self::MethodNotAllowed
            | Self::PayloadTooLarge
            | Self::Unauthorized { .. }
            | Self::Forbidden { .. }
            | Self::TooManyRequests { .. } => None,
            Self::BadRequest { detail, .. }
            | Self::Conflict { detail, .. }
            | Self::Validation { detail, .. } => Some(detail.clone()),
            Self::Unavailable(_) | Self::Database(_) | Self::Storage(_) | Self::Internal(_) => None,
        }
    }
}

/// RFC 9457 problem details body.
#[derive(Debug, Serialize, ToSchema)]
pub struct Problem {
    /// Always `about:blank`; `code` carries the specific error kind.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Stable machine-readable error code, e.g. `not_found`.
    pub code: String,
}

impl Problem {
    pub fn new(status: StatusCode, code: impl Into<String>, detail: Option<String>) -> Self {
        Self {
            kind: "about:blank",
            title: status.canonical_reason().unwrap_or("Error").to_owned(),
            status: status.as_u16(),
            detail,
            code: code.into(),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut res = (status, axum::Json(self)).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
        );
        res
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, code = self.code(), "request failed");
        }
        let mut res = Problem::new(status, self.code(), self.public_detail()).into_response();
        if status == StatusCode::UNAUTHORIZED {
            // RFC 6750 §3: name the expected scheme.
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use serde_json::Value;

    async fn render(err: Error) -> (StatusCode, String, Value) {
        let res = err.into_response();
        let status = res.status();
        let ct = res.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, ct, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn not_found_renders_problem_json() {
        let (status, ct, body) = render(Error::NotFound).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(ct, PROBLEM_CONTENT_TYPE);
        assert_eq!(body["type"], "about:blank");
        assert_eq!(body["title"], "Not Found");
        assert_eq!(body["status"], 404);
        assert_eq!(body["code"], "not_found");
        assert!(body.get("detail").is_none());
    }

    #[tokio::test]
    async fn validation_keeps_code_and_detail() {
        let err = Error::Validation {
            code: "invalid_slug",
            detail: "slug must be lowercase".into(),
        };
        let (status, _, body) = render(err).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "invalid_slug");
        assert_eq!(body["detail"], "slug must be lowercase");
    }

    #[tokio::test]
    async fn unauthorized_names_bearer_scheme() {
        let (status, _, body) = render(Error::Unauthorized {
            code: "reauth_required",
        })
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "reauth_required");
        let res = Error::Unauthorized {
            code: "invalid_token",
        }
        .into_response();
        assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }

    #[tokio::test]
    async fn internal_errors_do_not_leak_detail() {
        let (status, _, body) = render(Error::Internal("db password is hunter2".into())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["code"], "internal_error");
        assert!(!body.to_string().contains("hunter2"));
    }
}
