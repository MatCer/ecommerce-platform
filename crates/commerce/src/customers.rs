//! Customer accounts (spec §5.4, A4, A5). Accounts live on the checkout origin only; the
//! edge holds the `sid` cookie and passes the session token to the API.
//!
//! - Sessions: 256-bit tokens, SHA-256 at rest, 30 days sliding; a password change revokes
//!   every other session of the customer.
//! - Magic links: single use, 15 minutes, consumed atomically, rate-limited per email and IP.
//!   The link proves control of the address: the customer row is created (or marked verified)
//!   when it is consumed, and `customer.email_verified` is published so guest orders can be
//!   linked (WP10). Requests always answer the same way, known address or not.
//! - Passwords: argon2id with the OWASP parameters (m=19 MiB, t=2, p=1). Setting or changing
//!   one needs the current password or a magic-link sign-in within the last 10 minutes (A5).
//! - Redirects after sign-in: relative paths on the checkout origin only.

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use chrono::{DateTime, Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use platform::queue;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::capability;
use crate::cart::{self, Scope};
use crate::consent;
use crate::markets::invalid;
use crate::notifications::{self, Brand, Email, Template};
use crate::staff::normalize_email;
use crate::storefront::Context;

pub const SESSION_DAYS: i64 = 30;
pub const MAGIC_LINK_MINUTES: i64 = 15;
/// A5: a password may be set without the current one this long after a magic-link sign-in.
pub const REAUTH_MINUTES: i64 = 10;
pub const MIN_PASSWORD_CHARS: usize = 10;
pub const MAX_PASSWORD_CHARS: usize = 128;
pub const MAX_ADDRESSES: i64 = 20;
/// Outbox event after an address is proven (hook for guest-order linking, WP10). Payload:
/// `{customer_id}`.
pub const EMAIL_VERIFIED_EVENT: &str = "customer.email_verified";
/// Where sign-in lands by default.
pub const DEFAULT_REDIRECT: &str = "/account";

/// Rate limits over a sliding 15-minute window.
const WINDOW_MINUTES: i64 = 15;
const MAGIC_LINKS_PER_EMAIL: i64 = 3;
const MAGIC_LINKS_PER_IP: i64 = 10;
const FAILED_LOGINS_PER_EMAIL: i64 = 10;
const FAILED_LOGINS_PER_IP: i64 = 30;

// ---------------------------------------------------------------------------------------
// Passwords

/// argon2id v19, m=19456 KiB, t=2, p=1: the crate defaults are exactly OWASP's recommendation.
fn argon() -> Argon2<'static> {
    Argon2::default()
}

fn check_password_policy(password: &str) -> Result<(), Error> {
    let n = password.chars().count();
    if !(MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&n) {
        return Err(invalid(
            "weak_password",
            format!("the password needs {MIN_PASSWORD_CHARS}-{MAX_PASSWORD_CHARS} characters"),
        ));
    }
    if password.trim().is_empty() {
        return Err(invalid(
            "weak_password",
            "the password cannot be only spaces",
        ));
    }
    Ok(())
}

/// Hashes on a blocking thread (argon2 takes tens of milliseconds of CPU).
async fn hash_password(password: String) -> Result<String, Error> {
    tokio::task::spawn_blocking(move || {
        argon()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| Error::Internal(format!("password hashing failed: {e}")))
    })
    .await
    .map_err(|e| Error::Internal(e.to_string()))?
}

/// A real hash of a random password: verifying unknown accounts against it costs the same as
/// verifying a real one, so response times do not reveal which emails have accounts.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    argon()
        .hash_password(capability::mint().token.as_bytes())
        .map(|h| h.to_string())
        .unwrap_or_default()
});

async fn verify_password(password: String, hash: Option<String>) -> Result<bool, Error> {
    tokio::task::spawn_blocking(move || {
        let known = hash.is_some();
        let stored = hash.unwrap_or_else(|| DUMMY_HASH.clone());
        let ok = PasswordHash::new(&stored).is_ok_and(|parsed| {
            argon()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        });
        known && ok
    })
    .await
    .map_err(|e| Error::Internal(e.to_string()))
}

// ---------------------------------------------------------------------------------------
// Redirects

/// A relative path on the checkout origin (A5), else [`DEFAULT_REDIRECT`]. Rejects scheme-
/// relative (`//evil`), backslash tricks (`/\evil`), control characters and anything absolute.
pub fn safe_redirect(input: Option<&str>) -> String {
    let Some(path) = input.map(str::trim) else {
        return DEFAULT_REDIRECT.into();
    };
    let ok = path.len() <= 500
        && path.starts_with('/')
        && !path.starts_with("//")
        && !path.starts_with("/\\")
        && !path.contains('\\')
        && !path.chars().any(|c| c.is_control() || c.is_whitespace());
    if ok {
        path.to_owned()
    } else {
        DEFAULT_REDIRECT.into()
    }
}

// ---------------------------------------------------------------------------------------
// Views and inputs

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CustomerView {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub phone: Option<String>,
    pub locale: String,
    pub email_verified: bool,
    pub has_password: bool,
    /// A password can be set without the current one right now (A5: signed in with an email
    /// link within the last 10 minutes).
    pub recently_verified: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MagicLinkRequest {
    pub email: String,
    /// Relative path on the checkout origin to land on after sign-in (default `/account`).
    #[serde(default)]
    pub redirect: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MagicLinkToken {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordLogin {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub redirect: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordChange {
    /// Required unless the session signed in with an email link within the last 10 minutes.
    #[serde(default)]
    pub current_password: Option<String>,
    pub new_password: String,
}

/// A successful sign-in. The token goes to the edge (cookie), never to the browser's JS.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub session_token: String,
    pub customer: CustomerView,
    pub redirect: String,
}

/// An authenticated session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub customer_id: Uuid,
    pub token_hash: Vec<u8>,
    /// When this session last proved control of the email address (magic link).
    pub email_verified_at: Option<DateTime<Utc>>,
}

impl Session {
    fn recently_verified(&self, now: DateTime<Utc>) -> bool {
        self.email_verified_at
            .is_some_and(|at| now - at <= Duration::minutes(REAUTH_MINUTES))
    }
}

// ---------------------------------------------------------------------------------------
// Sessions

async fn create_session(
    tx: &mut TenantTx,
    customer_id: Uuid,
    email_verified: bool,
) -> Result<String, Error> {
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO customer_sessions (tenant_id, token_hash, customer_id, email_verified_at, expires_at)
         VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END, now() + make_interval(days => $5))",
        tx.tenant_id(),
        minted.hash,
        customer_id,
        email_verified,
        i32::try_from(SESSION_DAYS).unwrap_or(30)
    )
    .execute(&mut **tx)
    .await?;
    Ok(minted.token)
}

/// The session behind `token`, its expiry slid forward (30 days from now). `None` when unknown
/// or expired (including tokens of another tenant: RLS hides them).
pub async fn authenticate(tx: &mut TenantTx, token: &str) -> Result<Option<Session>, Error> {
    if !capability::well_formed(token) {
        return Ok(None);
    }
    let hash = capability::hash(token);
    let row = sqlx::query!(
        "UPDATE customer_sessions
         SET last_seen_at = now(), expires_at = now() + make_interval(days => $2)
         WHERE token_hash = $1 AND expires_at > now()
         RETURNING customer_id, email_verified_at",
        hash,
        i32::try_from(SESSION_DAYS).unwrap_or(30)
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|r| Session {
        customer_id: r.customer_id,
        token_hash: hash,
        email_verified_at: r.email_verified_at,
    }))
}

/// Like [`authenticate`], but `401 not_signed_in` without a valid session.
pub async fn require(tx: &mut TenantTx, token: Option<&str>) -> Result<Session, Error> {
    match token {
        Some(t) => authenticate(tx, t).await?,
        None => None,
    }
    .ok_or(Error::Unauthorized {
        code: "not_signed_in",
    })
}

pub async fn logout(tx: &mut TenantTx, token: &str) -> Result<(), Error> {
    if capability::well_formed(token) {
        sqlx::query!(
            "DELETE FROM customer_sessions WHERE token_hash = $1",
            capability::hash(token)
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn me(tx: &mut TenantTx, session: &Session) -> Result<CustomerView, Error> {
    let c = sqlx::query!(
        "SELECT id, email, name, phone, locale, email_verified_at, password_hash IS NOT NULL AS \"has_password!\"
         FROM customers WHERE id = $1",
        session.customer_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::Unauthorized {
        code: "not_signed_in",
    })?;
    Ok(CustomerView {
        id: c.id,
        email: c.email,
        name: c.name,
        phone: c.phone,
        locale: c.locale,
        email_verified: c.email_verified_at.is_some(),
        has_password: c.has_password,
        recently_verified: session.recently_verified(Utc::now()),
    })
}

// ---------------------------------------------------------------------------------------
// Rate limits

/// Recent attempts of `kind` for the email and the IP. Takes transaction-scoped advisory
/// locks on both first (email, then IP: one order, no deadlocks), so concurrent requests are
/// counted one after another and the caller's `record_attempt` in the same transaction is
/// visible to the next one: parallel requests cannot share one remaining allowance.
async fn attempts(
    tx: &mut TenantTx,
    kind: &str,
    email: &str,
    ip_hash: Option<&[u8]>,
) -> Result<(i64, i64), Error> {
    let tenant = tx.tenant_id();
    let mut keys = vec![format!("customer_auth:{tenant}:{kind}:email:{email}")];
    if let Some(ip) = ip_hash {
        keys.push(format!(
            "customer_auth:{tenant}:{kind}:ip:{}",
            hex::encode(ip)
        ));
    }
    for key in keys {
        sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", key)
            .fetch_one(&mut **tx)
            .await?;
    }
    let r = sqlx::query!(
        "SELECT count(*) FILTER (WHERE email = $2) AS \"email!\",
                count(*) FILTER (WHERE ip_hash = $3) AS \"ip!\"
         FROM customer_auth_attempts
         WHERE kind = $1 AND at > now() - make_interval(mins => $4)
           AND (email = $2 OR ip_hash = $3)",
        kind,
        email,
        ip_hash,
        i32::try_from(WINDOW_MINUTES).unwrap_or(15)
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok((r.email, r.ip))
}

async fn record_attempt(
    tx: &mut TenantTx,
    kind: &str,
    email: &str,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO customer_auth_attempts (tenant_id, kind, email, ip_hash) VALUES ($1, $2, $3, $4)",
        tx.tenant_id(),
        kind,
        email,
        ip_hash
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn too_many() -> Error {
    Error::TooManyRequests {
        code: "too_many_attempts",
    }
}

// ---------------------------------------------------------------------------------------
// Magic links

/// Emails a sign-in link (always the same answer, known address or not). `429` when the
/// address or the IP asked too often.
pub async fn request_magic_link(
    tx: &mut TenantTx,
    ctx: &Context,
    input: &MagicLinkRequest,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    let email = normalize_email(&input.email)?;
    let redirect = safe_redirect(input.redirect.as_deref());
    let (by_email, by_ip) = attempts(tx, "magic_link", &email, ip_hash).await?;
    if by_email >= MAGIC_LINKS_PER_EMAIL || by_ip >= MAGIC_LINKS_PER_IP {
        return Err(too_many());
    }
    record_attempt(tx, "magic_link", &email, ip_hash).await?;
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO customer_magic_links (tenant_id, token_hash, market_id, email, locale, redirect, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(mins => $7))",
        tx.tenant_id(),
        minted.hash,
        ctx.market.id,
        email,
        ctx.locale,
        redirect,
        i32::try_from(MAGIC_LINK_MINUTES).unwrap_or(15)
    )
    .execute(&mut **tx)
    .await?;
    let locale = sqlx::query_scalar!("SELECT locale FROM customers WHERE email = $1", email)
        .fetch_optional(&mut **tx)
        .await?
        .unwrap_or_else(|| ctx.locale.clone());
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::MagicLink,
            stream: Stream::Transactional,
            to: &email,
            locale: &locale,
            vars: json!({
                "url": ctx.checkout_url(&format!("/account/verify?token={}", minted.token)),
                "minutes": MAGIC_LINK_MINUTES,
            }),
            idempotency_key: format!("magic_link:{}", hex::encode(&minted.hash)),
            sensitive: true,
        },
    )
    .await?;
    Ok(())
}

/// Consumes a magic link (atomically, once, on the market it was requested for) and signs
/// the customer in, creating the account on first use. `400 invalid_magic_link` for unknown,
/// used or expired links.
pub async fn consume_magic_link(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    cart_token: Option<&str>,
    consent_subject: Option<&str>,
) -> Result<SignedIn, Error> {
    let invalid_link = || Error::BadRequest {
        code: "invalid_magic_link",
        detail: "the sign-in link expired or was already used".into(),
    };
    if !capability::well_formed(token) {
        return Err(invalid_link());
    }
    let link = sqlx::query!(
        "UPDATE customer_magic_links SET used_at = now()
         WHERE token_hash = $1 AND market_id = $2 AND used_at IS NULL AND expires_at > now()
         RETURNING email, locale, redirect",
        capability::hash(token),
        ctx.market.id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(invalid_link)?;
    sqlx::query!(
        "INSERT INTO customers (tenant_id, email, locale) VALUES ($1, $2, $3)
         ON CONFLICT ON CONSTRAINT customers_email_unique DO NOTHING",
        tx.tenant_id(),
        link.email,
        link.locale
    )
    .execute(&mut **tx)
    .await?;
    let c = sqlx::query!(
        "SELECT id, email_verified_at FROM customers WHERE email = $1 FOR UPDATE",
        link.email
    )
    .fetch_one(&mut **tx)
    .await?;
    if c.email_verified_at.is_none() {
        sqlx::query!(
            "UPDATE customers SET email_verified_at = now(), updated_at = now() WHERE id = $1",
            c.id
        )
        .execute(&mut **tx)
        .await?;
    }
    // A5: the link proved control of the address, so guest orders placed with it (also those
    // placed since an earlier verification) may join the account (`orders.link_guest`).
    queue::publish(
        &mut **tx,
        EMAIL_VERIFIED_EVENT,
        &json!({ "customer_id": c.id }),
    )
    .await?;
    let session_token = create_session(tx, c.id, true).await?;
    after_sign_in(tx, ctx, c.id, cart_token, consent_subject).await?;
    let session = authenticate(tx, &session_token)
        .await?
        .ok_or_else(|| Error::Internal("new session not found".into()))?;
    Ok(SignedIn {
        customer: me(tx, &session).await?,
        session_token,
        redirect: safe_redirect(Some(&link.redirect)),
    })
}

/// Cart merge (A4) and consent linking after any sign-in.
async fn after_sign_in(
    tx: &mut TenantTx,
    ctx: &Context,
    customer_id: Uuid,
    cart_token: Option<&str>,
    consent_subject: Option<&str>,
) -> Result<(), Error> {
    if let Some(token) = cart_token {
        match cart::find(tx, ctx, token, Some(Scope::Checkout)).await {
            Ok(c) => cart::attach_to_customer(tx, &c, customer_id).await?,
            // An expired or foreign cart capability just means there is nothing to attach.
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    if let Some(anon) = consent_subject {
        consent::link_anonymous(tx, anon, customer_id).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Password sign-in

/// Password sign-in. `Ok(None)` = wrong email or password (the failed attempt is recorded, so
/// the caller must commit before answering `401`). `429` after too many failures.
pub async fn login(
    tx: &mut TenantTx,
    ctx: &Context,
    input: &PasswordLogin,
    cart_token: Option<&str>,
    consent_subject: Option<&str>,
    ip_hash: Option<&[u8]>,
) -> Result<Option<SignedIn>, Error> {
    let Ok(email) = normalize_email(&input.email) else {
        return Ok(None);
    };
    let (by_email, by_ip) = attempts(tx, "login_failed", &email, ip_hash).await?;
    if by_email >= FAILED_LOGINS_PER_EMAIL || by_ip >= FAILED_LOGINS_PER_IP {
        return Err(too_many());
    }
    // Locked until commit: a concurrent password change (which locks the same row, then
    // revokes other sessions) cannot interleave, so an old password can never mint a session
    // that outlives the change.
    let found = sqlx::query!(
        "SELECT id, password_hash FROM customers WHERE email = $1 FOR UPDATE",
        email
    )
    .fetch_optional(&mut **tx)
    .await?;
    let (id, hash) = match found {
        Some(c) => (Some(c.id), c.password_hash),
        None => (None, None),
    };
    let password = input
        .password
        .chars()
        .take(MAX_PASSWORD_CHARS + 1)
        .collect();
    let ok = verify_password(password, hash).await?;
    let Some(customer_id) = id.filter(|_| ok) else {
        record_attempt(tx, "login_failed", &email, ip_hash).await?;
        return Ok(None);
    };
    let session_token = create_session(tx, customer_id, false).await?;
    after_sign_in(tx, ctx, customer_id, cart_token, consent_subject).await?;
    let session = authenticate(tx, &session_token)
        .await?
        .ok_or_else(|| Error::Internal("new session not found".into()))?;
    Ok(Some(SignedIn {
        customer: me(tx, &session).await?,
        session_token,
        redirect: safe_redirect(input.redirect.as_deref()),
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordOutcome {
    Changed,
    /// The current password was wrong (recorded as a failed attempt; commit, then refuse).
    WrongCurrentPassword,
}

/// Sets or changes the password (A5), revokes every other session and emails a notice.
/// `403 reauth_required` without the current password and without a recent email-link sign-in.
pub async fn set_password(
    tx: &mut TenantTx,
    ctx: &Context,
    session: &Session,
    input: &PasswordChange,
    ip_hash: Option<&[u8]>,
) -> Result<PasswordOutcome, Error> {
    check_password_policy(&input.new_password)?;
    let c = sqlx::query!(
        "SELECT email, locale, password_hash FROM customers WHERE id = $1 FOR UPDATE",
        session.customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if !session.recently_verified(Utc::now()) {
        let Some(current) = &input.current_password else {
            return Err(Error::Forbidden {
                code: "reauth_required",
            });
        };
        let (by_email, by_ip) = attempts(tx, "login_failed", &c.email, ip_hash).await?;
        if by_email >= FAILED_LOGINS_PER_EMAIL || by_ip >= FAILED_LOGINS_PER_IP {
            return Err(too_many());
        }
        if c.password_hash.is_none() {
            // Nothing to compare with: only a fresh email-link sign-in can set the first one.
            return Err(Error::Forbidden {
                code: "reauth_required",
            });
        }
        let current = current.chars().take(MAX_PASSWORD_CHARS + 1).collect();
        if !verify_password(current, c.password_hash.clone()).await? {
            record_attempt(tx, "login_failed", &c.email, ip_hash).await?;
            return Ok(PasswordOutcome::WrongCurrentPassword);
        }
    }
    let hash = hash_password(input.new_password.clone()).await?;
    let changed_at = sqlx::query_scalar!(
        "UPDATE customers SET password_hash = $2, password_changed_at = now(), updated_at = now()
         WHERE id = $1 RETURNING password_changed_at AS \"at!\"",
        session.customer_id,
        hash
    )
    .fetch_one(&mut **tx)
    .await?;
    // A5: a password change signs out every other device.
    sqlx::query!(
        "DELETE FROM customer_sessions WHERE customer_id = $1 AND token_hash <> $2",
        session.customer_id,
        session.token_hash
    )
    .execute(&mut **tx)
    .await?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::PasswordChanged,
            stream: Stream::Transactional,
            to: &c.email,
            locale: &c.locale,
            vars: json!({ "url": ctx.checkout_url(DEFAULT_REDIRECT) }),
            idempotency_key: format!(
                "password_changed:{}:{}",
                session.customer_id,
                changed_at.timestamp_micros()
            ),
            sensitive: false,
        },
    )
    .await?;
    Ok(PasswordOutcome::Changed)
}

// ---------------------------------------------------------------------------------------
// Addresses

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Address {
    pub id: Uuid,
    pub name: String,
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2 (`CZ`).
    pub country: String,
    pub phone: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddressInput {
    pub name: String,
    #[serde(default)]
    pub company: Option<String>,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    pub country: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

pub(crate) struct CleanAddress {
    pub(crate) name: String,
    pub(crate) company: Option<String>,
    pub(crate) street: String,
    pub(crate) city: String,
    pub(crate) postal_code: String,
    pub(crate) country: String,
    pub(crate) phone: Option<String>,
}

pub(crate) fn clean(a: &AddressInput) -> Result<CleanAddress, Error> {
    let field = |v: &str, name: &'static str, max: usize| -> Result<String, Error> {
        let v = v.trim();
        if v.is_empty() || v.chars().count() > max || v.chars().any(char::is_control) {
            return Err(invalid(
                "invalid_address",
                format!("{name} must have 1-{max} characters"),
            ));
        }
        Ok(v.to_owned())
    };
    let optional = |v: &Option<String>, name: &'static str, max: usize| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| field(v, name, max))
            .transpose()
    };
    let country = a.country.trim().to_ascii_uppercase();
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(invalid(
            "invalid_address",
            "country must be a two-letter code",
        ));
    }
    Ok(CleanAddress {
        name: field(&a.name, "name", 200)?,
        company: optional(&a.company, "company", 200)?,
        street: field(&a.street, "street", 200)?,
        city: field(&a.city, "city", 100)?,
        postal_code: field(&a.postal_code, "postal_code", 20)?,
        country,
        phone: optional(&a.phone, "phone", 40)?,
    })
}

pub async fn addresses(tx: &mut TenantTx, customer_id: Uuid) -> Result<Vec<Address>, Error> {
    Ok(sqlx::query_as!(
        Address,
        "SELECT id, name, company, street, city, postal_code, country, phone, is_default
         FROM customer_addresses WHERE customer_id = $1
         ORDER BY is_default DESC, created_at",
        customer_id
    )
    .fetch_all(&mut **tx)
    .await?)
}

async fn clear_default(tx: &mut TenantTx, customer_id: Uuid) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE customer_addresses SET is_default = false WHERE customer_id = $1 AND is_default",
        customer_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn add_address(
    tx: &mut TenantTx,
    customer_id: Uuid,
    input: &AddressInput,
) -> Result<Address, Error> {
    let a = clean(input)?;
    // Serializes concurrent additions for the same customer (count limit, single default).
    sqlx::query!(
        "SELECT id FROM customers WHERE id = $1 FOR UPDATE",
        customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let count = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM customer_addresses WHERE customer_id = $1",
        customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if count >= MAX_ADDRESSES {
        return Err(invalid(
            "too_many_addresses",
            format!("at most {MAX_ADDRESSES} addresses"),
        ));
    }
    let is_default = input.is_default || count == 0;
    if is_default {
        clear_default(tx, customer_id).await?;
    }
    Ok(sqlx::query_as!(
        Address,
        "INSERT INTO customer_addresses (tenant_id, customer_id, name, company, street, city,
                                         postal_code, country, phone, is_default)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id, name, company, street, city, postal_code, country, phone, is_default",
        tx.tenant_id(),
        customer_id,
        a.name,
        a.company,
        a.street,
        a.city,
        a.postal_code,
        a.country,
        a.phone,
        is_default
    )
    .fetch_one(&mut **tx)
    .await?)
}

pub async fn update_address(
    tx: &mut TenantTx,
    customer_id: Uuid,
    id: Uuid,
    input: &AddressInput,
) -> Result<Address, Error> {
    let a = clean(input)?;
    sqlx::query!(
        "SELECT id FROM customers WHERE id = $1 FOR UPDATE",
        customer_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if input.is_default {
        clear_default(tx, customer_id).await?;
    }
    sqlx::query_as!(
        Address,
        "UPDATE customer_addresses
         SET name = $3, company = $4, street = $5, city = $6, postal_code = $7, country = $8,
             phone = $9, is_default = is_default OR $10, updated_at = now()
         WHERE id = $1 AND customer_id = $2
         RETURNING id, name, company, street, city, postal_code, country, phone, is_default",
        id,
        customer_id,
        a.name,
        a.company,
        a.street,
        a.city,
        a.postal_code,
        a.country,
        a.phone,
        input.is_default
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

pub async fn delete_address(tx: &mut TenantTx, customer_id: Uuid, id: Uuid) -> Result<(), Error> {
    let deleted = sqlx::query!(
        "DELETE FROM customer_addresses WHERE id = $1 AND customer_id = $2",
        id,
        customer_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(Error::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stay_on_the_checkout_origin() {
        assert_eq!(safe_redirect(None), "/account");
        assert_eq!(
            safe_redirect(Some("/account/addresses")),
            "/account/addresses"
        );
        assert_eq!(safe_redirect(Some("/?step=2")), "/?step=2");
        for bad in [
            "https://evil.example/",
            "//evil.example",
            "/\\evil.example",
            "/a\\b",
            "javascript:alert(1)",
            "account",
            "/a b",
            "/a\nb",
            "",
        ] {
            assert_eq!(safe_redirect(Some(bad)), "/account", "{bad:?}");
        }
        assert_eq!(
            safe_redirect(Some(&format!("/{}", "a".repeat(600)))),
            "/account"
        );
    }

    #[test]
    fn password_policy() {
        assert!(check_password_policy("correct horse").is_ok());
        assert!(check_password_policy("short").is_err());
        assert!(check_password_policy(&"x".repeat(129)).is_err());
        assert!(check_password_policy("          ").is_err());
        // Counted in characters, not bytes.
        assert!(check_password_policy("žluťoučký1").is_ok());
    }

    #[tokio::test]
    async fn argon2id_owasp_hash_and_verify() {
        let h = hash_password("correct horse battery".into()).await.unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert!(
            verify_password("correct horse battery".into(), Some(h.clone()))
                .await
                .unwrap()
        );
        assert!(
            !verify_password("wrong horse battery".into(), Some(h))
                .await
                .unwrap()
        );
        // Unknown accounts never verify, even with the dummy hash's own password.
        assert!(!verify_password("anything".into(), None).await.unwrap());
    }

    #[test]
    fn addresses_are_trimmed_and_validated() {
        let input = AddressInput {
            name: "  Jana Nováková ".into(),
            company: Some("  ".into()),
            street: "Dlouhá 1".into(),
            city: "Praha".into(),
            postal_code: "110 00".into(),
            country: "cz".into(),
            phone: None,
            is_default: false,
        };
        let a = clean(&input).unwrap();
        assert_eq!(
            (a.name.as_str(), a.company, a.country.as_str()),
            ("Jana Nováková", None, "CZ")
        );
        assert!(
            clean(&AddressInput {
                country: "CZE".into(),
                ..input.clone()
            })
            .is_err()
        );
        assert!(
            clean(&AddressInput {
                city: String::new(),
                ..input.clone()
            })
            .is_err()
        );
        assert!(
            clean(&AddressInput {
                name: "a\u{0}b".into(),
                ..input
            })
            .is_err()
        );
    }
}
