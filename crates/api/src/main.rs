use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use axum::http::HeaderValue;
use clap::{Parser, Subcommand};
use platform::config::{
    ApiConfig, AppEnv, DbConfig, MeiliConfig, S3Config, ServiceTokenConfig, StaffAuthConfig,
};
use platform::storage::Storage;
use sqlx::postgres::PgPoolOptions;

#[derive(Parser)]
#[command(
    name = "api",
    about = "Commerce platform API server and operator commands"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve HTTP (default).
    Serve,
    /// Print the OpenAPI document (used by `make openapi`).
    Openapi,
    /// Apply migrations; run with the owner role's DATABASE_URL.
    Migrate,
    /// Container health probe.
    Healthcheck,
    /// Superadmin operations.
    #[command(subcommand)]
    Admin(api::cli::AdminCommand),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        // No logging here: stdout is the document.
        Command::Openapi => {
            println!("{}", api::openapi().to_pretty_json()?);
            Ok(())
        }
        Command::Migrate => migrate().await,
        Command::Healthcheck => healthcheck().await,
        Command::Admin(cmd) => {
            let db = platform::db::pool(&DbConfig::from_env()?)?;
            let result = api::cli::run(&db, cmd).await;
            db.close().await;
            result
        }
    }
}

fn init_tracing() -> anyhow::Result<()> {
    platform::telemetry::init().map_err(|e| anyhow!(e))
}

async fn serve() -> anyhow::Result<()> {
    init_tracing()?;
    let cfg = ApiConfig::from_env()?;
    let auth = StaffAuthConfig::from_env()?;
    let db = platform::db::pool(&DbConfig::from_env()?)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let auth_service = platform::config::AuthServiceConfig::optional_from_env()?
        .map(|c| api::auth_service::AuthService::new(c.base_url, c.token))
        .transpose()?;
    if auth_service.is_none() {
        tracing::warn!(
            "AUTH_INTERNAL_URL/AUTH_INTERNAL_TOKEN not set: staff invitations answer 503"
        );
    }
    let state = api::AppState {
        auth_service,
        db: db.clone(),
        http: http.clone(),
        meili: {
            let m = MeiliConfig::search_from_env()?;
            // Short: a slow search degrades to 503 instead of holding storefront requests.
            commerce::search::Meili::new(http.clone(), m.url, m.key, Duration::from_secs(2))
        },
        storage: Storage::s3(&S3Config::from_env()?)?,
        staff_auth: Arc::new(api::auth::StaffAuth::new(http, auth.jwks_url, &auth.issuer)),
        internal_token: api::auth::ServiceToken::new(
            &ServiceTokenConfig::from_env()?.internal_api_token,
        ),
        admin_origin: HeaderValue::from_str(&auth.admin_origin).context("ADMIN_ORIGIN")?,
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
