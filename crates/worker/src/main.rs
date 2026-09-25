use std::time::Duration;

use anyhow::anyhow;
use platform::config::{AuthServiceConfig, DbConfig, MeiliConfig, S3Config, WorkerConfig};
use platform::mail::{MailConfig, Mailer};
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

    // Without MAIL_* the worker still runs every other job; mail jobs wait (retry) until it is
    // configured. A partial MAIL_* setup refuses to start.
    let mailer = MailConfig::optional_from_env()?
        .map(|c| Mailer::new(&c))
        .transpose()?;
    if mailer.is_none() {
        tracing::warn!("MAIL_* is not configured: emails stay queued until it is");
    }
    // Staff invitations need the auth service (sign-in links); other jobs run without it.
    let auth = AuthServiceConfig::optional_from_env()?
        .map(|c| platform::auth_service::AuthService::new(c.base_url, c.token))
        .transpose()?;

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
            handlers::all(storage, meili, mailer, auth),
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
