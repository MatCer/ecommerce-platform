//! Thin Meilisearch HTTP client: the dozen endpoints search needs, on the workspace `reqwest`.
//! Not `meilisearch-sdk`: it brings its own HTTP stack and trails server features we rely on
//! (granular filterable attributes, per-query `distinct`, multi-search).
//!
//! Writes are asynchronous in Meilisearch: they return a task uid, [`Meili::wait`] polls it.
//! Tasks of one index are applied in enqueue order, which the indexing jobs rely on.

use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum MeiliError {
    /// Unreachable, timed out or an unreadable response.
    #[error("meilisearch unreachable: {0}")]
    Http(#[from] reqwest::Error),
    #[error("meilisearch {status}: {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("meilisearch task {uid} {status}: {error}")]
    Task {
        uid: u64,
        status: String,
        error: String,
    },
    #[error("meilisearch task {0} still running after the wait limit")]
    Timeout(u64),
}

impl MeiliError {
    /// The Meilisearch error code (`index_not_found`, …), if the server answered.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { code, .. } => Some(code),
            _ => None,
        }
    }
}

impl From<MeiliError> for platform::Error {
    fn from(e: MeiliError) -> Self {
        Self::Unavailable(format!("search: {e}"))
    }
}

/// A Meilisearch task handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Task {
    #[serde(rename = "taskUid")]
    pub uid: u64,
}

/// Client bound to one API key: the search-only key in the API, the admin key in the worker
/// (spec A27). Not `Debug`: holds the key.
#[derive(Clone)]
pub struct Meili {
    http: reqwest::Client,
    url: Url,
    key: String,
    timeout: Duration,
}

impl Meili {
    /// `timeout` bounds every request (the storefront uses a short one and degrades).
    pub fn new(http: reqwest::Client, url: Url, key: String, timeout: Duration) -> Self {
        Self {
            http,
            url,
            key,
            timeout,
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, MeiliError> {
        let mut url = self.url.clone();
        let (path, query) = path
            .split_once('?')
            .map_or((path, None), |(p, q)| (p, Some(q)));
        url.set_path(path);
        url.set_query(query);
        let mut req = self
            .http
            .request(method, url)
            .bearer_auth(&self.key)
            .timeout(self.timeout);
        if let Some(body) = body {
            req = req.json(body);
        }
        let res = req.send().await?;
        let status = res.status();
        if status.is_success() {
            return Ok(if status == StatusCode::NO_CONTENT {
                Value::Null
            } else {
                res.json().await?
            });
        }
        let err: Value = res.json().await.unwrap_or(Value::Null);
        let text = |k: &str| {
            err.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Err(MeiliError::Api {
            status: status.as_u16(),
            code: text("code"),
            message: text("message"),
        })
    }

    async fn task(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Task, MeiliError> {
        let v = self.call(method, path, body).await?;
        serde_json::from_value(v).map_err(|e| MeiliError::Api {
            status: 200,
            code: "unexpected_response".into(),
            message: e.to_string(),
        })
    }

    /// `false` when the index does not exist.
    pub async fn index_exists(&self, uid: &str) -> Result<bool, MeiliError> {
        match self
            .call(Method::GET, &format!("/indexes/{uid}"), None)
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.code() == Some("index_not_found") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn create_index(&self, uid: &str) -> Result<Task, MeiliError> {
        self.task(
            Method::POST,
            "/indexes",
            Some(&json!({ "uid": uid, "primaryKey": "id" })),
        )
        .await
    }

    pub async fn delete_index(&self, uid: &str) -> Result<Task, MeiliError> {
        self.task(Method::DELETE, &format!("/indexes/{uid}"), None)
            .await
    }

    pub async fn update_settings(&self, uid: &str, settings: &Value) -> Result<Task, MeiliError> {
        self.task(
            Method::PATCH,
            &format!("/indexes/{uid}/settings"),
            Some(settings),
        )
        .await
    }

    /// Adds or replaces whole documents.
    pub async fn add_documents(&self, uid: &str, docs: &[Value]) -> Result<Task, MeiliError> {
        self.task(
            Method::POST,
            &format!("/indexes/{uid}/documents"),
            Some(&Value::Array(docs.to_vec())),
        )
        .await
    }

    pub async fn delete_by_filter(&self, uid: &str, filter: &str) -> Result<Task, MeiliError> {
        self.task(
            Method::POST,
            &format!("/indexes/{uid}/documents/delete"),
            Some(&json!({ "filter": filter })),
        )
        .await
    }

    /// Documents matching `filter` (only `fields`), for bookkeeping queries in the worker.
    pub async fn fetch_documents(
        &self,
        uid: &str,
        filter: &str,
        fields: &[&str],
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Value>, MeiliError> {
        let v = self
            .call(
                Method::POST,
                &format!("/indexes/{uid}/documents/fetch"),
                Some(&json!({ "filter": filter, "fields": fields, "offset": offset, "limit": limit })),
            )
            .await?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Atomically swaps the contents of each `(a, b)` pair.
    pub async fn swap(&self, pairs: &[(String, String)]) -> Result<Task, MeiliError> {
        let body: Vec<Value> = pairs
            .iter()
            .map(|(a, b)| json!({ "indexes": [a, b] }))
            .collect();
        self.task(Method::POST, "/swap-indexes", Some(&Value::Array(body)))
            .await
    }

    /// `POST /multi-search`; returns the `results` array in query order.
    pub async fn multi_search(&self, queries: &[Value]) -> Result<Vec<Value>, MeiliError> {
        let v = self
            .call(
                Method::POST,
                "/multi-search",
                Some(&json!({ "queries": queries })),
            )
            .await?;
        Ok(v.get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Polls a task until it succeeded (Ok), failed (Err) or `limit` elapsed.
    pub async fn wait(&self, task: Task, limit: Duration) -> Result<(), MeiliError> {
        let deadline = tokio::time::Instant::now() + limit;
        let mut delay = Duration::from_millis(10);
        loop {
            let v = self
                .call(Method::GET, &format!("/tasks/{}", task.uid), None)
                .await?;
            match v.get("status").and_then(Value::as_str).unwrap_or_default() {
                "succeeded" => return Ok(()),
                status @ ("failed" | "canceled") => {
                    return Err(MeiliError::Task {
                        uid: task.uid,
                        status: status.to_owned(),
                        error: v
                            .pointer("/error/code")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    });
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MeiliError::Timeout(task.uid));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_millis(500));
        }
    }

    /// Waits until no task of `uid` is enqueued or processing (tests, operator tooling).
    pub async fn wait_idle(&self, uid: &str, limit: Duration) -> Result<(), MeiliError> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let v = self
                .call(
                    Method::GET,
                    &format!("/tasks?indexUids={uid}&statuses=enqueued,processing&limit=1"),
                    None,
                )
                .await?;
            if v.get("results")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MeiliError::Timeout(0));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A Meilisearch filter string literal: double-quoted, with `\` and `"` escaped. Every value
/// that reaches a filter goes through here (no filter injection).
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_quotes_and_backslashes() {
        assert_eq!(quote("red"), r#""red""#);
        assert_eq!(
            quote(r#"a" OR id EXISTS OR "b"#),
            r#""a\" OR id EXISTS OR \"b""#
        );
        assert_eq!(quote(r"x\"), r#""x\\""#);
    }
}
