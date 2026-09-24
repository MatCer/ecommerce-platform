use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use platform::config::{ApiConfig, AppEnv, DbConfig, MeiliConfig, S3Config};
use platform::storage::Storage;
use sqlx::postgres::PgPoolOptions;

const USAGE: &str = "usage: api [serve | openapi | migrate | healthcheck]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve().await,
        // Prints the OpenAPI document to stdout; used by `make openapi`. No logging here.
        Some("openapi") => {
            println!("{}", api::openapi().to_pretty_json()?);
            Ok(())
        }
        Some("migrate") => migrate().await,
        Some("healthcheck") => healthcheck().await,
        Some(other) => bail!("unknown command {other:?}\n{USAGE}"),
    }
}

fn init_tracing() -> anyhow::Result<()> {
    platform::telemetry::init().map_err(|e| anyhow!(e))
}

async fn serve() -> anyhow::Result<()> {
    init_tracing()?;
    let cfg = ApiConfig::from_env()?;
    let db = platform::db::pool(&DbConfig::from_env()?)?;
    let state = api::AppState {
        db: db.clone(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?,
        meili_url: MeiliConfig::from_env()?.url,
        storage: Storage::s3(&S3Config::from_env()?)?,
    };
    let app = api::app(state, cfg.env == AppEnv::Dev);

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("bind {}", cfg.bind))?;
    tracing::info!(addr = %cfg.bind, env = ?cfg.env, "api listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(platform::shutdown::signal())
        .await?;
    db.close().await;
    tracing::info!("api stopped");
    Ok(())
}

/// Applies `migrations/` (embedded at build time). Run with the owner role's `DATABASE_URL`.
async fn migrate() -> anyhow::Result<()> {
    init_tracing()?;
    let db = DbConfig::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&db.url)
        .await
        .context("connect for migrations")?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// Container health probe: the runtime image has no shell or curl.
async fn healthcheck() -> anyhow::Result<()> {
    let port = ApiConfig::from_env()?.bind.port();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
