//! Tenants, domains and staff membership (spec §5.1, §5.3, A8, A29).

use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit, capability, id, themes, unique_violation};

/// Staff roles, weakest first (spec §5.3). `staff` has no settings, payment config, staff
/// management or exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Staff,
    Admin,
    Owner,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "staff" => Some(Self::Staff),
            "admin" => Some(Self::Admin),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

/// Subdomains the platform itself uses under `*.localhost` (spec §3.2); a tenant slug must not
/// shadow them.
const RESERVED_SLUGS: &[&str] = &[
    "admin", "api", "auth", "mail", "s3", "checkout", "www", "edge", "internal", "static",
];

pub fn validate_slug(slug: &str) -> Result<(), Error> {
    let bytes = slug.as_bytes();
    let ok = (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.starts_with("preview-");
    if !ok {
        return Err(invalid(
            "invalid_slug",
            "slug must be 1-63 lowercase letters, digits or inner hyphens",
        ));
    }
    if RESERVED_SLUGS.contains(&slug) {
        return Err(invalid("reserved_slug", "slug is reserved by the platform"));
    }
    Ok(())
}

/// Lowercases, drops a `:port` and a trailing dot, and checks DNS hostname syntax.
pub fn normalize_host(host: &str) -> Result<String, Error> {
    let host = host.trim().to_ascii_lowercase();
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            name.to_owned()
        }
        _ => host,
    };
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    let labels_ok = host.split('.').all(|l| {
        (1..=63).contains(&l.len())
            && l.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    });
    if host.len() > 253 || !host.contains('.') || !labels_ok {
        return Err(invalid("invalid_hostname", "not a valid hostname"));
    }
    Ok(host)
}

fn invalid(code: &'static str, detail: &str) -> Error {
    Error::Validation {
        code,
        detail: detail.into(),
    }
}

/// Local development hostnames resolve to loopback, so there is nothing to verify.
fn is_local(host: &str) -> bool {
    host.ends_with(".localhost")
}

pub struct NewTenant<'a> {
    pub slug: &'a str,
    pub name: &'a str,
    pub owner_user_id: &'a str,
    pub owner_email: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedTenant {
    pub tenant_id: Uuid,
    pub market_id: Uuid,
    pub hostname: String,
}

/// Creates a tenant with its default CZ market, the `<slug>.localhost` domain and the owner's
/// staff membership, in one transaction. Superadmin only (CLI).
pub async fn create_tenant(db: &PgPool, t: &NewTenant<'_>) -> Result<CreatedTenant, Error> {
    validate_slug(t.slug)?;
    let name = t.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("invalid_name", "name must be 1-200 characters"));
    }
    let tenant_id = id::new_id();
    let hostname = format!("{}.localhost", t.slug);

    let mut tx = tenant_tx(db, tenant_id).await?;
    sqlx::query!(
        "INSERT INTO platform.tenants (id, slug, name) VALUES ($1, $2, $3)",
        tenant_id,
        t.slug,
        name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if unique_violation(&e) {
            Error::Conflict {
                code: "already_exists",
                detail: format!("tenant {} already exists", t.slug),
            }
        } else {
            e.into()
        }
    })?;
    let market_id = sqlx::query_scalar!(
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale, locales, is_default)
         VALUES ($1, 'cz', 'Česko', '{CZ}', 'CZK', 'cs', '{cs}', true)
         RETURNING id",
        tenant_id
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
         VALUES ($1, $2, $3, true, now())",
        hostname,
        tenant_id,
        market_id
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if unique_violation(&e) {
            Error::Conflict {
                code: "already_exists",
                detail: format!("domain {hostname} already exists"),
            }
        } else {
            e.into()
        }
    })?;
    sqlx::query!(
        "INSERT INTO staff_members (tenant_id, user_id, email, role) VALUES ($1, $2, $3, 'owner')",
        tenant_id,
        t.owner_user_id,
        t.owner_email
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO platform.storefront_tokens (token, tenant_id) VALUES ($1, $2)",
        new_storefront_token(),
        tenant_id
    )
    .execute(&mut *tx)
    .await?;
    themes::assign_default(&mut tx, audit::PLATFORM_ACTOR).await?;
    audit::record(
        &mut tx,
        audit::PLATFORM_ACTOR,
        "tenant.created",
        "tenant",
        Some(&tenant_id.to_string()),
        &json!({ "slug": t.slug, "name": name, "owner": t.owner_user_id }),
    )
    .await?;
    platform::queue::publish(
        &mut *tx,
        "tenant.created",
        &json!({ "tenant_id": tenant_id, "slug": t.slug }),
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedTenant {
        tenant_id,
        market_id,
        hostname,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct Domain {
    pub hostname: String,
    pub verification_token: String,
    pub verified: bool,
}

impl Domain {
    /// Where the TXT record must be published (A29 stub, see [`verify_domain`]).
    pub fn txt_name(&self) -> String {
        format!("_commerce-verification.{}", self.hostname)
    }

    pub fn txt_value(&self) -> String {
        format!("commerce-verification={}", self.verification_token)
    }
}

/// Adds a domain for a market (the default market when `market_code` is `None`).
/// `*.localhost` domains are verified immediately; others wait for [`verify_domain`].
pub async fn add_domain(
    db: &PgPool,
    tenant_slug: &str,
    host: &str,
    market_code: Option<&str>,
    primary: bool,
) -> Result<Domain, Error> {
    let host = normalize_host(host)?;
    let tenant_id = sqlx::query_scalar!(
        "SELECT id FROM platform.tenants WHERE slug = $1",
        tenant_slug
    )
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound)?;

    let mut tx = tenant_tx(db, tenant_id).await?;
    let market_id = sqlx::query_scalar!(
        "SELECT id FROM markets WHERE ($1::text IS NULL AND is_default) OR code = $1",
        market_code
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::NotFound)?;
    if primary {
        sqlx::query!(
            "UPDATE platform.domains SET is_primary = false WHERE market_id = $1",
            market_id
        )
        .execute(&mut *tx)
        .await?;
    }
    let local = is_local(&host);
    let token = sqlx::query_scalar!(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() END)
         RETURNING verification_token",
        host,
        tenant_id,
        market_id,
        primary,
        local
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if unique_violation(&e) {
            Error::Conflict {
                code: "already_exists",
                detail: format!("domain {host} is already registered"),
            }
        } else {
            e.into()
        }
    })?;
    audit::record(
        &mut tx,
        audit::PLATFORM_ACTOR,
        "domain.added",
        "domain",
        Some(&host),
        &json!({ "market_id": market_id, "primary": primary }),
    )
    .await?;
    tx.commit().await?;
    Ok(Domain {
        hostname: host,
        verification_token: token,
        verified: local,
    })
}

/// Looks up a domain awaiting verification.
pub async fn domain(db: &PgPool, host: &str) -> Result<Domain, Error> {
    let host = normalize_host(host)?;
    sqlx::query_as!(
        Domain,
        r#"SELECT hostname, verification_token, verified_at IS NOT NULL AS "verified!"
           FROM platform.domains WHERE hostname = $1"#,
        host
    )
    .fetch_optional(db)
    .await?
    .ok_or(Error::NotFound)
}

/// Marks the domain verified when one of its TXT records carries the expected token
/// (local stub for A29; `txt_records` come from the DNS lookup, mocked locally).
pub async fn verify_domain(db: &PgPool, host: &str, txt_records: &[String]) -> Result<bool, Error> {
    let domain = domain(db, host).await?;
    if domain.verified {
        return Ok(true);
    }
    if !txt_records.iter().any(|r| r.trim() == domain.txt_value()) {
        return Ok(false);
    }
    sqlx::query!(
        "UPDATE platform.domains SET verified_at = now() WHERE hostname = $1 AND verified_at IS NULL",
        domain.hostname
    )
    .execute(db)
    .await?;
    Ok(true)
}

/// What the edge needs to serve a hostname (spec §5.1, `GET /internal/v1/resolve`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Resolved {
    pub hostname: String,
    pub tenant_id: Uuid,
    pub tenant_slug: String,
    pub market_id: Uuid,
    pub market_code: String,
    pub currency: String,
    pub default_locale: String,
    pub locales: Vec<String>,
    pub country_codes: Vec<String>,
    /// The tenant's public storefront token (§5.5); the edge injects it, never the browser.
    pub storefront_token: String,
    /// Active theme artifact; `None` until a theme is published for the tenant.
    pub theme_artifact: Option<String>,
    /// Earlier artifacts whose `/_astro/*` assets stay served (A22).
    pub retained_artifacts: Vec<String>,
    /// The platform checkout artifact (same for every tenant, §9.4).
    pub checkout_artifact: Option<String>,
}

/// Resolves a verified domain of an active tenant; `None` for anything else.
pub async fn resolve_host(db: &PgPool, host: &str) -> Result<Option<Resolved>, Error> {
    let Ok(host) = normalize_host(host) else {
        return Ok(None);
    };
    let Some(d) = sqlx::query!(
        r#"SELECT d.tenant_id, d.market_id, t.slug, s.token AS "token?"
         FROM platform.domains d JOIN platform.tenants t ON t.id = d.tenant_id
         LEFT JOIN platform.storefront_tokens s ON s.tenant_id = t.id AND s.expires_at IS NULL
         WHERE d.hostname = $1 AND d.verified_at IS NOT NULL AND t.status = 'active'"#,
        host
    )
    .fetch_optional(db)
    .await?
    else {
        return Ok(None);
    };
    let token = d.token.ok_or_else(|| {
        Error::Internal(format!("tenant {} has no storefront token", d.tenant_id))
    })?;
    let checkout_artifact = themes::channel(db, themes::CHECKOUT).await?;
    let mut tx = tenant_tx(db, d.tenant_id).await?;
    let m = sqlx::query!(
        "SELECT code, currency, default_locale, locales, country_codes FROM markets WHERE id = $1",
        d.market_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let theme = themes::active(&mut tx).await?;
    tx.commit().await?;
    Ok(Some(Resolved {
        hostname: host,
        tenant_id: d.tenant_id,
        tenant_slug: d.slug,
        market_id: d.market_id,
        market_code: m.code,
        currency: m.currency,
        default_locale: m.default_locale,
        locales: m.locales,
        country_codes: m.country_codes,
        storefront_token: token,
        theme_artifact: theme.artifact_id,
        retained_artifacts: theme.retained,
        checkout_artifact,
    }))
}

// ---------------------------------------------------------------------------------------
// Storefront tokens (§5.5)

/// How long a rotated-out token keeps working (longer than the edge's 60 s resolver cache).
pub const TOKEN_GRACE_SECS: i32 = 300;

fn new_storefront_token() -> String {
    format!("sf_{}", capability::mint().token)
}

/// The tenant a storefront token belongs to: the current token or one still in its grace
/// period, of an active tenant.
pub async fn storefront_token_tenant(db: &PgPool, token: &str) -> Result<Option<Uuid>, Error> {
    let shaped = token.len() == 67
        && token
            .strip_prefix("sf_")
            .is_some_and(|t| t.bytes().all(|b| b.is_ascii_hexdigit()));
    if !shaped {
        return Ok(None);
    }
    Ok(sqlx::query_scalar!(
        "SELECT s.tenant_id FROM platform.storefront_tokens s
         JOIN platform.tenants t ON t.id = s.tenant_id
         WHERE s.token = $1 AND (s.expires_at IS NULL OR s.expires_at > now())
           AND t.status = 'active'",
        token
    )
    .fetch_optional(db)
    .await?)
}

/// The current storefront token of the tenant in `tx`.
pub async fn storefront_token(tx: &mut TenantTx) -> Result<String, Error> {
    sqlx::query_scalar!(
        "SELECT token FROM platform.storefront_tokens WHERE tenant_id = $1 AND expires_at IS NULL",
        tx.tenant_id()
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

/// Issues a new storefront token. The previous one keeps working for [`TOKEN_GRACE_SECS`].
/// Audited (without the token values).
pub async fn rotate_storefront_token(tx: &mut TenantTx, actor: &str) -> Result<String, Error> {
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "UPDATE platform.storefront_tokens SET expires_at = now() + make_interval(secs => $2)
         WHERE tenant_id = $1 AND expires_at IS NULL",
        tenant_id,
        f64::from(TOKEN_GRACE_SECS)
    )
    .execute(&mut **tx)
    .await?;
    let token = new_storefront_token();
    sqlx::query!(
        "INSERT INTO platform.storefront_tokens (token, tenant_id) VALUES ($1, $2)",
        token,
        tenant_id
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "storefront_token.rotated",
        "storefront_token",
        None,
        &json!({ "grace_seconds": TOKEN_GRACE_SECS }),
    )
    .await?;
    Ok(token)
}

/// The caller's role in an active tenant (A8 bootstrap), or `None`.
pub async fn membership(
    db: &PgPool,
    user_id: &str,
    tenant_id: Uuid,
) -> Result<Option<Role>, Error> {
    let role = sqlx::query_scalar!(
        "SELECT platform.staff_membership($1, $2)",
        user_id,
        tenant_id
    )
    .fetch_one(db)
    .await?;
    Ok(role.as_deref().and_then(Role::parse))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Membership {
    pub tenant_id: Uuid,
    pub slug: String,
    pub name: String,
    pub role: Role,
}

/// Every active tenant the user belongs to.
pub async fn memberships(db: &PgPool, user_id: &str) -> Result<Vec<Membership>, Error> {
    let rows = sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", slug AS "slug!", name AS "name!", role AS "role!"
           FROM platform.staff_tenants($1)"#,
        user_id
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Membership {
                tenant_id: r.tenant_id,
                slug: r.slug,
                name: r.name,
                role: Role::parse(&r.role)?,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert!(validate_slug("demo").is_ok());
        assert!(validate_slug("demo-sk2").is_ok());
        for bad in [
            "",
            "Demo",
            "-demo",
            "demo-",
            "de_mo",
            "demo.cz",
            "preview-x",
            "api",
            "auth",
        ] {
            assert!(validate_slug(bad).is_err(), "{bad}");
        }
        assert!(validate_slug(&"a".repeat(64)).is_err());
    }

    #[test]
    fn hosts() {
        assert_eq!(
            normalize_host("Demo.LocalHost:8180").unwrap(),
            "demo.localhost"
        );
        assert_eq!(
            normalize_host("shop.example.cz.").unwrap(),
            "shop.example.cz"
        );
        for bad in [
            "localhost",
            "",
            "a..b",
            "-a.cz",
            "a_b.cz",
            "a.cz/x",
            "a.cz:",
            "exa mple.cz",
        ] {
            assert!(normalize_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Owner > Role::Admin && Role::Admin > Role::Staff);
        assert_eq!(Role::parse("admin"), Some(Role::Admin));
        assert_eq!(Role::parse("root"), None);
    }
}
