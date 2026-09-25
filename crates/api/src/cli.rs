//! Superadmin commands (`api admin ...`, spec §5, A29). Run inside the api container, which
//! has the runtime database URL and the auth service's internal token:
//! `make admin args="create-tenant --slug demo --name Demo --owner-email owner@example.com"`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use clap::Subcommand;
use commerce::tenancy::{self, CreatedTenant, NewTenant};
use commerce::themes::{self, ArtifactKind};
use platform::config::{S3Config, StorefrontConfig};
use platform::storage::Storage;
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
    /// Create or complete the demo shop (tenant `demo`, CZ + SK markets, 60 products, a sale,
    /// a coupon). Idempotent.
    SeedDemo {
        #[arg(long, default_value = "owner@lnen.example")]
        owner_email: String,
    },
    /// Upload packed artifacts (`theme-kit pack` output under `--root`) to the private bucket,
    /// register them, publish the theme as the default for every tenant following it and set
    /// the checkout artifact (spec A22, A30). Then purges the edge.
    PublishArtifacts {
        #[arg(long)]
        root: PathBuf,
        /// Theme artifact id to publish as the default theme.
        #[arg(long)]
        theme: Option<String>,
        /// Checkout artifact id.
        #[arg(long)]
        checkout: Option<String>,
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
    if let AdminCommand::PublishArtifacts {
        root,
        theme,
        checkout,
    } = &cmd
    {
        return publish_artifacts(db, root, theme.as_deref(), checkout.as_deref()).await;
    }
    let env = CliEnv::load()?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    match cmd {
        AdminCommand::CreateTenant {
            slug,
            name,
            owner_email,
        } => {
            let created = create_tenant(db, &env, &slug, &name, &owner_email).await?;
            print_json(&json!({
                "tenant_id": created.tenant_id,
                "market_id": created.market_id,
                "hostname": created.hostname,
                "invited": owner_email,
            }))
        }
        AdminCommand::SeedDemo { owner_email } => seed_demo(db, &env, &owner_email).await,
        AdminCommand::PublishArtifacts { .. } => Ok(()),
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
) -> anyhow::Result<CreatedTenant> {
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
    Ok(created)
}

async fn seed_demo(db: &PgPool, env: &CliEnv, owner_email: &str) -> anyhow::Result<()> {
    let existing = sqlx::query_scalar!(
        "SELECT id FROM platform.tenants WHERE slug = $1",
        crate::seed::TENANT
    )
    .fetch_optional(db)
    .await?;
    let tenant_id = match existing {
        Some(id) => id,
        None => {
            create_tenant(
                db,
                env,
                crate::seed::TENANT,
                "Lnen & Co.",
                owner_email,
            )
            .await?
            .tenant_id
        }
    };
    let storage = Storage::s3(&S3Config::from_env()?)?;
    let summary = crate::seed::Seeder {
        db,
        storage: &storage,
    }
    .run(tenant_id)
    .await?;
    edge_purge()?.tenant(tenant_id).await;
    print_json(&json!({
        "tenant_id": summary.tenant_id,
        "products_created": summary.products_created,
        "images_created": summary.images_created,
        "shops": ["demo.localhost", "demo-sk.localhost"],
        "owner": owner_email,
    }))
}

fn edge_purge() -> anyhow::Result<crate::edge::EdgePurge> {
    let cfg = StorefrontConfig::from_env()?;
    Ok(crate::edge::EdgePurge::new(
        cfg.edge_purge_url,
        cfg.edge_purge_token,
    ))
}

/// Every regular file under `dir` as `(relative posix path, bytes)`; symlinks are refused.
fn read_tree(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir.join(rel))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("non-UTF-8 file name in the artifact"))?;
        let path = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            read_tree(dir, &path, out)?;
        } else if kind.is_file() {
            out.push((path.clone(), std::fs::read(dir.join(&path))?));
        } else {
            bail!("artifact contains a symlink or special file: {path}");
        }
    }
    Ok(())
}

async fn publish_artifacts(
    db: &PgPool,
    root: &Path,
    theme: Option<&str>,
    checkout: Option<&str>,
) -> anyhow::Result<()> {
    let storage = Storage::s3(&S3Config::from_env()?)?;
    let mut out = serde_json::Map::new();
    for (id, expected) in [
        (theme, ArtifactKind::Theme),
        (checkout, ArtifactKind::Checkout),
    ] {
        let Some(id) = id else { continue };
        if !themes::artifact_id_valid(id) {
            bail!("invalid artifact id {id:?}");
        }
        let dir = root.join(id);
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("manifest.json"))
                .with_context(|| format!("read {}", dir.join("manifest.json").display()))?,
        )?;
        if manifest["id"] != id {
            bail!("manifest id does not match the directory {id}");
        }
        let kind = manifest["kind"]
            .as_str()
            .and_then(ArtifactKind::parse)
            .filter(|k| *k == expected)
            .ok_or_else(|| anyhow!("artifact {id} is not a {} artifact", expected.as_str()))?;
        let mut files = Vec::new();
        read_tree(&dir, "", &mut files)?;
        let tokens = manifest.get("tokens").filter(|t| !t.is_null());
        themes::register_artifact(db, &storage, id, kind, tokens, files).await?;
        match kind {
            ArtifactKind::Theme => {
                let tenants =
                    themes::publish_default(db, commerce::audit::PLATFORM_ACTOR, id).await?;
                out.insert(
                    "theme".into(),
                    json!({ "id": id, "tenants_updated": tenants.len() }),
                );
            }
            ArtifactKind::Checkout => {
                themes::set_channel(db, themes::CHECKOUT, id).await?;
                out.insert("checkout".into(), json!({ "id": id }));
            }
        }
    }
    edge_purge()?.all().await;
    print_json(&serde_json::Value::Object(out))
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
