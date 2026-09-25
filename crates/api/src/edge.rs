//! Purges of the edge's resolver and HTML caches (`POST /_edge/purge`, spec §9.3.1) after
//! changes the edge must see before its TTLs run out (theme publish, token rotation).
//!
//! Best effort: a failed purge is logged, and the edge's TTLs (60 s resolver, page-model
//! `max_age`) bound how long it serves the old state.

use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone)]
pub struct EdgePurge {
    http: reqwest::Client,
    url: Option<Url>,
    token: String,
}

impl EdgePurge {
    pub fn new(url: Option<Url>, token: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        Self { http, url, token }
    }

    /// No edge to purge (tests, tools).
    pub fn disabled() -> Self {
        Self::new(None, String::new())
    }

    pub async fn tenant(&self, tenant_id: Uuid) {
        self.send(json!({ "tenant_id": tenant_id })).await;
    }

    pub async fn all(&self) {
        self.send(json!({ "all": true })).await;
    }

    async fn send(&self, body: Value) {
        let Some(url) = &self.url else { return };
        let res = self
            .http
            .post(url.clone())
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        if let Err(e) = res {
            tracing::warn!(error = %e, "edge purge failed; caches expire on their TTL");
        }
    }
}
