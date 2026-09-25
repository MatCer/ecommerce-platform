use std::time::Duration;

use anyhow::anyhow;
use platform::config::{
    AppEnv, AuthServiceConfig, DbConfig, MeiliConfig, OpsConfig, S3Config, StorefrontConfig,
    WorkerConfig,
};
use platform::mail::{MailConfig, Mailer};
use platform::storage::Storage;
use worker::runner::RunnerConfig;
use worker::{cron, handlers, metrics, outbox, runner};

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

    // Webhook deliveries need the secrets key; without it they wait (retry) until it is set.
    let ops = OpsConfig::from_env()?;
    let env: AppEnv = std::env::var("APP_ENV")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(AppEnv::Prod);
    let fetch = platform::http::SafeClient::from_env()?;
    let webhooks = ops.secrets_key.map(|key| commerce::webhooks::Webhooks {
        secrets: platform::crypto::SecretBox::new(&key),
        http: fetch.clone(),
        require_https: env == AppEnv::Prod,
    });
    if webhooks.is_none() {
        tracing::warn!("SECRETS_KEY is not configured: webhook deliveries stay queued until it is");
    }
    // Event partitions exist before the first event of a new month even if the nightly job
    // has not run yet (e.g. after downtime). In the background: startup (and SIGTERM
    // handling) must not wait for a database that is down.
    let partitions_db = db.clone();
    tokio::spawn(async move {
        if let Err(e) = sqlx::query("SELECT platform.ensure_event_partitions(2)")
            .execute(&partitions_db)
            .await
        {
            tracing::warn!(error = %e, "ensuring event partitions failed");
        }
    });
    // Edge purges and public URLs (export feeds); the SSRF-safe client for imports and
    // webhooks (A21).
    let sf = StorefrontConfig::from_env()?;
    let extra = handlers::Extra {
        edge: platform::edge::EdgePurge::new(sf.edge_purge_url, sf.edge_purge_token),
        fetch,
        urls: commerce::storefront::PublicUrls {
            scheme: sf.scheme,
            port: sf.port,
        },
        webhooks,
        fio: fio_poller(env, &ops)?,
        fulfillment: Some(fulfillment(&ops)?),
    };

    let (stop, shutdown) = tokio::sync::watch::channel(false);
    if let Some(bind) = ops.metrics_bind {
        let handle = platform::metrics::install().map_err(|e| anyhow!(e))?;
        tokio::spawn(metrics::refresh(db.clone(), shutdown.clone()));
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = platform::metrics::serve(bind, handle, shutdown).await {
                tracing::error!(error = %e, "metrics listener failed");
            }
        });
    }
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
            handlers::all(storage, meili, mailer, auth, extra),
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

/// Carriers (tracking), the ČNB client and the Typst renderer (WP12).
fn fulfillment(ops: &platform::config::OpsConfig) -> anyhow::Result<handlers::Fulfillment> {
    let c = platform::config::FulfillmentConfig::from_env()?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    Ok(handlers::Fulfillment {
        carriers: commerce::carriers::Carriers::new(
            c.packeta_api_url.to_string(),
            c.packeta_validate_url.to_string(),
            c.ppl_api_url.to_string(),
            ops.secrets_key
                .map(|k| std::sync::Arc::new(platform::crypto::SecretBox::new(&k))),
        )?,
        rates: commerce::invoicing::Rates {
            http,
            url: c.cnb_rates_url.to_string(),
        },
        typst: commerce::documents::Typst {
            bin: c.typst_bin.into(),
        },
    })
}

/// The Fio API poller when `SECRETS_KEY` is set (stored tokens are encrypted with it).
fn fio_poller(
    env: platform::config::AppEnv,
    ops: &platform::config::OpsConfig,
) -> anyhow::Result<Option<handlers::Fio>> {
    let p = platform::config::PaymentsConfig::from_env(env)?;
    let Some(key) = ops.secrets_key else {
        tracing::warn!("SECRETS_KEY not set: Fio API polling is off");
        return Ok(None);
    };
    Ok(Some(handlers::Fio {
        secrets: std::sync::Arc::new(platform::crypto::SecretBox::new(&key)),
        base_url: p.fio_api_url.to_string(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?,
    }))
}
