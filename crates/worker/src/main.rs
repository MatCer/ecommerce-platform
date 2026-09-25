use std::time::Duration;

use anyhow::anyhow;
use platform::config::{DbConfig, MeiliConfig, S3Config, WorkerConfig};
use platform::storage::Storage;
use worker::runner::RunnerConfig;
use worker::{cron, handlers, outbox, runner};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    platform::telemetry::init().map_err(|e| anyhow!(e))?;
    let cfg = WorkerConfig::from_env()?;
    let db = platform::db::pool(&DbConfig::from_env()?)?;
    let storage = Storage::s3(&S3Config::from_env()?)?;
    let meili = {
        let m = MeiliConfig::admin_from_env()?;
        let http = reqwest::Client::builder().build()?;
        commerce::search::Meili::new(http, m.url, m.key, Duration::from_secs(30))
    };

    let (stop, shutdown) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        platform::shutdown::signal().await;
        let _ = stop.send(true);
    });

    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "worker".into());
    let owner = format!("{host}:{}", std::process::id());
    tracing::info!(%owner, concurrency = cfg.concurrency, "worker started");

    tokio::join!(
        runner::run(
            db.clone(),
            handlers::all(storage, meili),
            RunnerConfig::new(owner, cfg.concurrency),
            shutdown.clone(),
        ),
        outbox::run(db.clone(), Duration::from_millis(500), shutdown.clone()),
        cron::run(db.clone(), Duration::from_secs(30), shutdown),
    );

    db.close().await;
    tracing::info!("worker stopped");
    Ok(())
}
