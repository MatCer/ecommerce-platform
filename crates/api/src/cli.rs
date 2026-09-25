//! Superadmin commands (`api admin ...`, spec §5, A29). Run inside the api container, which
//! has the runtime database URL and the auth service's internal token:
//! `make admin args="create-tenant --slug demo --name Demo --owner-email owner@example.com"`.

use std::time::Duration;

use anyhow::{Context, bail};
use clap::Subcommand;
use commerce::tenancy::{self, NewTenant};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;

#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create a tenant with a default CZ market, the `<slug>.localhost` domain and an owner
    /// (created in the auth service if new, then invited by magic link).
    CreateTenant {
        #[arg(long)]
        slug: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        owner_email: String,
    },
    /// Add a domain to a tenant's market. Non-`.localhost` domains stay unverified until
    /// `verify-domain` finds the TXT record.
    AddDomain {
        /// Tenant slug.
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        host: String,
        /// Market code; the default market when omitted.
        #[arg(long)]
        market: Option<String>,
        /// Make it the market's primary domain.
        #[arg(long)]
        primary: bool,
    },
    /// Check the domain's verification TXT record (DNS stub: the mocks service) and mark it
    /// verified when it matches.
    VerifyDomain {
        #[arg(long)]
        host: String,
    },
}

/// Settings only the CLI needs (read when a command runs).
struct CliEnv {
    auth: platform::config::AuthServiceConfig,
    dns_txt_url: Url,
    admin_url: String,
}

fn env_url(name: &str) -> anyhow::Result<Url> {
    let raw = std::env::var(name).with_context(|| format!("{name} is not set"))?;
    Url::parse(&raw).with_context(|| format!("{name} is not a URL"))
}

impl CliEnv {
    fn load() -> anyhow::Result<Self> {
        Ok(Self {
            auth: platform::config::AuthServiceConfig::from_env()?,
            dns_txt_url: env_url("DNS_TXT_URL")?,
            admin_url: std::env::var("ADMIN_ORIGIN").context("ADMIN_ORIGIN is not set")?,
        })
    }
}

pub async fn run(db: &PgPool, cmd: AdminCommand) -> anyhow::Result<()> {
    let env = CliEnv::load()?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    match cmd {
        AdminCommand::CreateTenant {
            slug,
            name,
            owner_email,
        } => create_tenant(db, &env, &slug, &name, &owner_email).await,
        AdminCommand::AddDomain {
            tenant,
            host,
            market,
            primary,
        } => {
            let d = tenancy::add_domain(db, &tenant, &host, market.as_deref(), primary).await?;
            print_json(&json!({
                "hostname": d.hostname,
                "verified": d.verified,
                "txt_record": (!d.verified).then(|| json!({ "name": d.txt_name(), "value": d.txt_value() })),
            }))
        }
        AdminCommand::VerifyDomain { host } => {
            let d = tenancy::domain(db, &host).await?;
            let records = txt_records(&http, &env.dns_txt_url, &d.txt_name()).await?;
            let verified = tenancy::verify_domain(db, &host, &records).await?;
            print_json(&json!({ "hostname": d.hostname, "verified": verified }))?;
            if !verified {
                bail!(
                    "TXT record {} does not contain {}",
                    d.txt_name(),
                    d.txt_value()
                );
            }
            Ok(())
        }
    }
}

async fn create_tenant(
    db: &PgPool,
    env: &CliEnv,
    slug: &str,
    name: &str,
    owner_email: &str,
) -> anyhow::Result<()> {
    // Fail on a bad slug before touching the auth service.
    tenancy::validate_slug(slug)?;
    // 1. Find or create the owner in the auth service (idempotent), without emailing yet.
    let auth =
        crate::auth_service::AuthService::new(env.auth.base_url.clone(), env.auth.token.clone())?;
    let user_id = auth.ensure_user(owner_email, name).await?;
    // 2. Tenant, market, domain, membership, audit entry and event in one transaction.
    let created = tenancy::create_tenant(
        db,
        &NewTenant {
            slug,
            name,
            owner_user_id: &user_id,
            owner_email,
        },
    )
    .await?;
    // 3. Only now send the magic link invitation.
    auth.invite(owner_email, &env.admin_url)
        .await
        .context("tenant created, but sending the invitation failed; rerun the invite")?;
    print_json(&json!({
        "tenant_id": created.tenant_id,
        "market_id": created.market_id,
        "hostname": created.hostname,
        "owner_user_id": user_id,
        "invited": owner_email,
    }))
}

#[derive(Deserialize)]
struct TxtAnswer {
    records: Vec<String>,
}

/// ponytail: HTTP DNS stub (apps/mocks); swap for a real resolver (hickory) with real domains.
async fn txt_records(http: &reqwest::Client, url: &Url, name: &str) -> anyhow::Result<Vec<String>> {
    let mut url = url.clone();
    url.query_pairs_mut().append_pair("name", name);
    let answer: TxtAnswer = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(answer.records)
}

fn print_json(value: &serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
