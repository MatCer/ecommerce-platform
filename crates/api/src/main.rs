use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use axum::http::HeaderValue;
use clap::{Parser, Subcommand};
use platform::config::{
    ApiConfig, AppEnv, CheckoutConfig, DbConfig, FulfillmentConfig, MeiliConfig, OpsConfig,
    PaymentsConfig, S3Config, ServiceTokenConfig, StaffAuthConfig, StorefrontConfig,
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
    Openapi {
        /// Only the Storefront API (source of the storefront SDK types).
        #[arg(long)]
        storefront: bool,
    },
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
        Command::Openapi { storefront } => {
            let doc = if storefront {
                api::openapi_storefront()
            } else {
                api::openapi()
            };
            println!("{}", doc.to_pretty_json()?);
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

/// Payment gateways and the pickup-point widget from `CheckoutConfig` and `PaymentsConfig`.
fn checkout_settings(
    c: &CheckoutConfig,
    p: &PaymentsConfig,
    ops: &OpsConfig,
) -> anyhow::Result<commerce::checkout::Settings> {
    let fake = c.payments_fake.then(|| {
        tracing::warn!("PAYMENTS_FAKE=1: the fake payment gateway is enabled (local/e2e only)");
        let secret = c
            .fake_secret
            .clone()
            .map(String::into_bytes)
            .unwrap_or_else(|| commerce::capability::mint().token.into_bytes());
        commerce::payments::FakeGateway::new(secret)
    });
    let packeta = match (&c.packeta_widget_url, &c.packeta_api_key) {
        (Some(url), Some(key)) => Some(commerce::checkout::PacketaWidget {
            script_url: url.to_string(),
            api_key: key.clone(),
        }),
        _ => {
            tracing::warn!("PACKETA_WIDGET_URL/PACKETA_API_KEY not set: no pickup-point widget");
            None
        }
    };
    let stripe = match &p.stripe {
        Some(cfg) => {
            if cfg.mode == platform::config::StripeMode::Simulator {
                tracing::warn!(
                    "no STRIPE_SECRET_KEY: Stripe runs against stripe-mock with the test simulator"
                );
            }
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?;
            Some(commerce::payments::stripe::Stripe::new(cfg, http))
        }
        None => {
            tracing::warn!("Stripe is not configured (STRIPE_SECRET_KEY or STRIPE_MOCK_URL)");
            None
        }
    };
    let secrets = ops
        .secrets_key
        .map(|k| Arc::new(platform::crypto::SecretBox::new(&k)));
    if secrets.is_none() {
        tracing::warn!("SECRETS_KEY not set: Fio API tokens cannot be stored");
    }
    Ok(commerce::checkout::Settings {
        payments: commerce::payments::Payments {
            fake,
            stripe,
            secrets,
        },
        packeta,
    })
}

/// AI helpers from `AiConfig` (Anthropic with a key, else the fake provider; off in prod).
fn ai_helpers() -> anyhow::Result<commerce::ai::Ai> {
    let ai = commerce::ai::Ai::from_config(&platform::config::AiConfig::from_env()?)?;
    match ai.provider() {
        "fake" => tracing::warn!("ANTHROPIC_API_KEY not set: AI helpers use the fake provider"),
        "disabled" => tracing::warn!("ANTHROPIC_API_KEY not set: AI helpers are disabled"),
        _ => tracing::info!(model = %ai.helper_model, "AI helpers use the Anthropic API"),
    }
    Ok(ai)
}

/// `MAIL_EVENTS_SECRET` (at least 32 characters): the HTTP Basic password SNS sends with SES
/// bounce/complaint notifications. Unset: the endpoint is off.
fn mail_events_secret() -> anyhow::Result<Option<api::auth::ServiceToken>> {
    match std::env::var("MAIL_EVENTS_SECRET") {
        Ok(v) if v.trim().is_empty() => Ok(None),
        Ok(v) if v.len() < 32 => anyhow::bail!("MAIL_EVENTS_SECRET must be at least 32 characters"),
        Ok(v) => Ok(Some(api::auth::ServiceToken::new(&v))),
        Err(_) => {
            tracing::warn!("MAIL_EVENTS_SECRET not set: bounce/complaint ingestion is off");
            Ok(None)
        }
    }
}

fn init_tracing() -> anyhow::Result<()> {
    platform::telemetry::init().map_err(|e| anyhow!(e))
}

/// Webhook secrets + the SSRF-safe client (A21); `None` without `SECRETS_KEY`.
fn webhooks(ops: &OpsConfig, env: AppEnv) -> anyhow::Result<Option<commerce::webhooks::Webhooks>> {
    let Some(key) = ops.secrets_key else {
        tracing::warn!("SECRETS_KEY not set: webhook subscriptions answer 503");
        return Ok(None);
    };
    Ok(Some(commerce::webhooks::Webhooks {
        secrets: platform::crypto::SecretBox::new(&key),
        http: platform::http::SafeClient::from_env()?,
        require_https: env == AppEnv::Prod,
    }))
}

async fn serve() -> anyhow::Result<()> {
    init_tracing()?;
    let cfg = ApiConfig::from_env()?;
    let ops = OpsConfig::from_env()?;
    let auth = StaffAuthConfig::from_env()?;
    let sf = StorefrontConfig::from_env()?;
    let theme_cfg = platform::config::ThemeConfig::from_env()?;
    if theme_cfg.secret.is_none() || theme_cfg.builder_token.is_none() {
        tracing::warn!(
            "THEME_SECRET/THEME_BUILDER_TOKEN not set: theme previews and builds answer 503"
        );
    }
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
    let checkout = Arc::new(checkout_settings(
        &CheckoutConfig::from_env(cfg.env)?,
        &PaymentsConfig::from_env(cfg.env)?,
        &ops,
    )?);
    let fulfillment = FulfillmentConfig::from_env()?;
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
        public_urls: commerce::storefront::PublicUrls {
            scheme: sf.scheme.clone(),
            port: sf.port,
        },
        edge: api::edge::EdgePurge::new(sf.edge_purge_url, sf.edge_purge_token),
        webhooks: webhooks(&ops, cfg.env)?,
        ads: match ops.secrets_key {
            Some(key) => Some(commerce::adtracking::AdTracking::new(
                platform::crypto::SecretBox::new(&key),
                platform::http::SafeClient::from_env()?,
                commerce::adtracking::AdTracking::endpoints_from_env(),
                commerce::storefront::PublicUrls {
                    scheme: sf.scheme,
                    port: sf.port,
                },
            )),
            None => None,
        },
        rate_limit: Arc::new(api::rate_limit::StorefrontLimiter::new(
            ops.storefront_rate_per_second,
            ops.storefront_rate_burst,
        )),
        carriers: Some(commerce::carriers::Carriers::new(
            fulfillment.packeta_api_url.to_string(),
            fulfillment.packeta_validate_url.to_string(),
            fulfillment.ppl_api_url.to_string(),
            checkout.payments.secrets.clone(),
        )?),
        checkout,
        ai: ai_helpers()?,
        mail_events: mail_events_secret()?,
        themes: theme_cfg
            .secret
            .as_deref()
            .map(|s| commerce::themes::ThemeKeys::new(s.as_bytes())),
        builder_token: theme_cfg
            .builder_token
            .as_deref()
            .map(api::auth::ServiceToken::new),
    };
    let limiter = state.rate_limit.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            limiter.prune();
        }
    });
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    if let Some(bind) = ops.metrics_bind {
        let handle = platform::metrics::install().map_err(|e| anyhow!(e))?;
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = platform::metrics::serve(bind, handle, shutdown).await {
                tracing::error!(error = %e, "metrics listener failed");
            }
        });
    }
    let app = api::app(state, cfg.env == AppEnv::Dev);

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("bind {}", cfg.bind))?;
    tracing::info!(addr = %cfg.bind, env = ?cfg.env, "api listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            platform::shutdown::signal().await;
            let _ = stop.send(true);
        })
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
