//! Readiness checks (spec §15): database, Meilisearch and object storage.
//! Search is out of core readiness (spec A27, A30): a Meilisearch failure makes the overall
//! status `degraded` (still ready, HTTP 200), never `fail`.
//! Responses expose only ok/fail per dependency; the cause goes to the logs, since `/readyz`
//! is publicly reachable and must not reveal internal hostnames or errors.

use std::fmt::Display;
use std::time::Duration;

use reqwest::Url;
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use crate::storage::Storage;

const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    /// Only for the overall status: core dependencies are fine, search is not.
    Degraded,
    Fail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Checks {
    pub database: CheckStatus,
    pub meilisearch: CheckStatus,
    pub storage: CheckStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Readiness {
    /// `ok` when every dependency is `ok`; `degraded` when only Meilisearch fails (search
    /// answers 503, the shop keeps working, spec A27/A30); `fail` when a core dependency
    /// (database, storage) fails.
    pub status: CheckStatus,
    pub checks: Checks,
}

impl Readiness {
    pub fn is_ready(&self) -> bool {
        self.status != CheckStatus::Fail
    }
}

/// Runs all checks concurrently, each bounded by a 2 s timeout.
pub async fn readiness(
    db: &PgPool,
    http: &reqwest::Client,
    meili_url: &Url,
    storage: &Storage,
) -> Readiness {
    let (database, meilisearch, storage) = tokio::join!(
        check("database", crate::db::ping(db)),
        check("meilisearch", meili_health(http, meili_url)),
        check("storage", storage.ping()),
    );
    let core_ok = database == CheckStatus::Ok && storage == CheckStatus::Ok;
    Readiness {
        status: match (core_ok, meilisearch) {
            (false, _) => CheckStatus::Fail,
            (true, CheckStatus::Ok) => CheckStatus::Ok,
            (true, _) => CheckStatus::Degraded,
        },
        checks: Checks {
            database,
            meilisearch,
            storage,
        },
    }
}

async fn meili_health(http: &reqwest::Client, base: &Url) -> Result<(), reqwest::Error> {
    let mut url = base.clone();
    url.set_path("/health");
    http.get(url).send().await?.error_for_status()?;
    Ok(())
}

async fn check<E: Display>(
    name: &'static str,
    fut: impl Future<Output = Result<(), E>>,
) -> CheckStatus {
    match tokio::time::timeout(CHECK_TIMEOUT, fut).await {
        Ok(Ok(())) => CheckStatus::Ok,
        Ok(Err(e)) => {
            tracing::warn!(check = name, error = %e, "readiness check failed");
            CheckStatus::Fail
        }
        Err(_) => {
            tracing::warn!(check = name, "readiness check timed out");
            CheckStatus::Fail
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;
    use std::sync::Arc;

    #[tokio::test]
    async fn reports_each_dependency_and_fails_without_database() {
        // Nothing listens on port 1: database and Meilisearch fail, in-memory storage passes.
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(500))
            .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
            .unwrap();
        let meili = Url::parse("http://127.0.0.1:1").unwrap();
        let cfg = crate::config::S3Config::from_lookup(&|k| {
            k.starts_with("S3_")
                .then(|| "http://s3.test".to_owned())
                .filter(|_| k != "S3_REGION" && k != "S3_PUBLIC_ENDPOINT")
        })
        .unwrap();
        let storage = Storage {
            public: Arc::new(InMemory::new()),
            private: Arc::new(InMemory::new()),
            ..Storage::s3(&cfg).unwrap()
        };

        let r = readiness(&db, &reqwest::Client::new(), &meili, &storage).await;

        assert_eq!(r.status, CheckStatus::Fail);
        assert_eq!(r.checks.database, CheckStatus::Fail);
        assert_eq!(r.checks.meilisearch, CheckStatus::Fail);
        assert_eq!(r.checks.storage, CheckStatus::Ok);
        assert!(!r.is_ready());
    }
}
