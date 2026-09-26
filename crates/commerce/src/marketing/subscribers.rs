//! Newsletter subscribers (spec §11.5, A20): double opt-in with stored evidence, unsubscribe
//! (one-click links, the preference page, staff, consent withdrawal), customer linking and the
//! admin list/export.
//!
//! Subscribing never reveals whether an address is known: the answer is always "accepted"
//! and a confirmation mail goes out only when one is due (new, re-subscribing, or a pending
//! request older than the resend cooldown). The confirmation page reads the token with GET
//! and confirms only on POST, so link scanners cannot subscribe anyone.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::capability;
use crate::consent::{self, ConsentPurpose, Subject};
use crate::markets::invalid;
use crate::notifications::{self, Brand, Email, Template};
use crate::storefront::Context;

/// How long a confirmation link works.
pub const CONFIRM_HOURS: i64 = 48;
/// A pending address gets another confirmation mail only after this long.
pub const RESEND_COOLDOWN_MINUTES: i64 = 10;
/// Sign-up requests one (hashed) IP may make per hour before `429`.
pub const MAX_REQUESTS_PER_IP_HOUR: i64 = 20;
/// Rows an export returns at most.
pub const MAX_EXPORT: i64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Subscribed,
    Unsubscribed,
    Bounced,
    Complained,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Subscribed => "subscribed",
            Self::Unsubscribed => "unsubscribed",
            Self::Bounced => "bounced",
            Self::Complained => "complained",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "subscribed" => Self::Subscribed,
            "unsubscribed" => Self::Unsubscribed,
            "bounced" => Self::Bounced,
            "complained" => Self::Complained,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Subscriber {
    pub id: Uuid,
    pub email: String,
    pub status: Status,
    pub locale: String,
    pub market_id: Uuid,
    /// The customer account with the same (verified) address.
    pub customer_id: Option<Uuid>,
    /// Consent evidence: when the sign-up was requested and confirmed, and the consent text
    /// version shown (the IP hashes are stored but never shown).
    pub requested_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub text_version: String,
    pub source: String,
    pub unsubscribed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

struct Row {
    id: Uuid,
    email: String,
    status: String,
    locale: String,
    market_id: Uuid,
    customer_id: Option<Uuid>,
    requested_at: DateTime<Utc>,
    confirmed_at: Option<DateTime<Utc>>,
    text_version: String,
    source: String,
    unsubscribed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<Row> for Subscriber {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            email: r.email,
            status: Status::parse(&r.status),
            locale: r.locale,
            market_id: r.market_id,
            customer_id: r.customer_id,
            requested_at: r.requested_at,
            confirmed_at: r.confirmed_at,
            text_version: r.text_version,
            source: r.source,
            unsubscribed_at: r.unsubscribed_at,
            created_at: r.created_at,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Sign-up and double opt-in

/// A sign-up from the shop (`source` `form`) or the preference page (`resubscribe`). Always
/// `Ok` for a well-formed address (no enumeration); `429` when the IP made too many requests.
pub async fn subscribe(
    tx: &mut TenantTx,
    ctx: &Context,
    raw_email: &str,
    ip_hash: Option<&[u8]>,
    source: &'static str,
) -> Result<(), Error> {
    let email = crate::staff::normalize_email(raw_email)?;
    // Every request counts (the rate-limit ledger of customer sign-ins, purged daily), not
    // just new rows: repeating one address cannot get around the cap.
    if let Some(ip) = ip_hash {
        let recent = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM customer_auth_attempts
               WHERE kind = 'newsletter' AND ip_hash = $1 AND at > now() - interval '1 hour'"#,
            ip
        )
        .fetch_one(&mut **tx)
        .await?;
        if recent >= MAX_REQUESTS_PER_IP_HOUR {
            return Err(Error::TooManyRequests {
                code: "too_many_signups",
            });
        }
        sqlx::query!(
            "INSERT INTO customer_auth_attempts (tenant_id, kind, email, ip_hash)
             VALUES ($1, 'newsletter', $2, $3)",
            tx.tenant_id(),
            email,
            ip
        )
        .execute(&mut **tx)
        .await?;
    }
    let minted = capability::mint();
    let inserted = sqlx::query_scalar!(
        "INSERT INTO subscribers (tenant_id, email, locale, market_id, confirm_token_hash,
                                  confirm_expires_at, request_ip_hash, text_version, source)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(hours => $6), $7, $8, $9)
         ON CONFLICT ON CONSTRAINT subscribers_email_unique DO NOTHING
         RETURNING id",
        tx.tenant_id(),
        email,
        ctx.locale,
        ctx.market.id,
        minted.hash,
        i32::try_from(CONFIRM_HOURS).unwrap_or(48),
        ip_hash,
        consent::TEXT_VERSION,
        source
    )
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_none() {
        let current = sqlx::query!(
            r#"SELECT id, status,
                      requested_at < now() - make_interval(mins => $2) AS "cooled_down!"
               FROM subscribers WHERE email = $1 FOR UPDATE"#,
            email,
            i32::try_from(RESEND_COOLDOWN_MINUTES).unwrap_or(10)
        )
        .fetch_one(&mut **tx)
        .await?;
        let due = match Status::parse(&current.status) {
            Status::Subscribed => false,
            Status::Pending => current.cooled_down,
            // Coming back after leaving: a fresh opt-in, unless the address is suppressed
            // (a hard bounce or a complaint that staff have not cleared).
            Status::Unsubscribed | Status::Bounced | Status::Complained => {
                !notifications::is_suppressed(tx, &email, Stream::Marketing).await?
            }
        };
        if !due {
            return Ok(());
        }
        sqlx::query!(
            "UPDATE subscribers SET status = 'pending', confirm_token_hash = $2,
                 confirm_expires_at = now() + make_interval(hours => $3), requested_at = now(),
                 request_ip_hash = $4, locale = $5, market_id = $6, text_version = $7,
                 source = $8, updated_at = now()
             WHERE id = $1",
            current.id,
            minted.hash,
            i32::try_from(CONFIRM_HOURS).unwrap_or(48),
            ip_hash,
            ctx.locale,
            ctx.market.id,
            consent::TEXT_VERSION,
            source
        )
        .execute(&mut **tx)
        .await?;
    }
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue(
        tx,
        &brand,
        Email {
            template: Template::NewsletterConfirm,
            stream: Stream::Transactional,
            to: &email,
            locale: &ctx.locale,
            vars: json!({
                "url": ctx.checkout_url(&format!("/newsletter/confirm?token={}", minted.token)),
                "hours": CONFIRM_HOURS,
            }),
            idempotency_key: format!("newsletter_confirm:{}", hex::encode(&minted.hash)),
            sensitive: true,
        },
    )
    .await?;
    Ok(())
}

/// What the confirmation page shows before the button is pressed (a read, no change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Confirmation {
    /// The address, partly masked (`j***@example.com`).
    pub email: String,
}

/// The pending sign-up behind a confirmation token; `404` when invalid, used or expired.
pub async fn confirmation(tx: &mut TenantTx, token: &str) -> Result<Confirmation, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    let email = sqlx::query_scalar!(
        "SELECT email FROM subscribers
         WHERE confirm_token_hash = $1 AND confirm_expires_at > now() AND status = 'pending'",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Confirmation {
        email: mask(&email),
    })
}

/// Confirms a sign-up: subscribed, the `email_marketing` consent recorded with the evidence
/// (A20), linked to the customer with that verified address. `404` when the token is invalid,
/// used or expired.
pub async fn confirm(
    tx: &mut TenantTx,
    token: &str,
    ip_hash: Option<&[u8]>,
) -> Result<Subscriber, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    let row = sqlx::query_as!(
        Row,
        "UPDATE subscribers s SET status = 'subscribed', confirmed_at = now(),
             confirm_ip_hash = $2, confirm_token_hash = NULL, confirm_expires_at = NULL,
             unsubscribed_at = NULL, updated_at = now(),
             customer_id = coalesce(s.customer_id, (
                 SELECT c.id FROM customers c
                 WHERE c.email = s.email AND c.email_verified_at IS NOT NULL))
         WHERE confirm_token_hash = $1 AND confirm_expires_at > now() AND status = 'pending'
         RETURNING id, email, status, locale, market_id, customer_id, requested_at,
                   confirmed_at, text_version, source, unsubscribed_at, created_at",
        capability::hash(token),
        ip_hash
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    consent::record_server(
        tx,
        &Subject::Email(row.email.clone()),
        ConsentPurpose::EmailMarketing,
        true,
        &row.text_version,
        "double_opt_in",
        ip_hash,
    )
    .await?;
    Ok(row.into())
}

/// `j***@example.com`: enough to recognise one's own address on a page reached by a link.
pub fn mask(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".into(),
    }
}

// ---------------------------------------------------------------------------------------
// Unsubscribing

/// Unsubscribes `id` (from any active state) and records the withdrawal (A20). Returns the
/// subscriber, or `None` when it did not exist. Idempotent.
pub async fn unsubscribe(
    tx: &mut TenantTx,
    id: Uuid,
    source: &'static str,
    ip_hash: Option<&[u8]>,
) -> Result<Option<Subscriber>, Error> {
    let changed = sqlx::query_scalar!(
        "UPDATE subscribers SET status = 'unsubscribed', unsubscribed_at = now(),
             confirm_token_hash = NULL, confirm_expires_at = NULL, updated_at = now()
         WHERE id = $1 AND status IN ('pending', 'subscribed')
         RETURNING email",
        id
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(email) = changed {
        consent::record_server(
            tx,
            &Subject::Email(email),
            ConsentPurpose::EmailMarketing,
            false,
            consent::TEXT_VERSION,
            source,
            ip_hash,
        )
        .await?;
    }
    get_opt(tx, id).await
}

/// A20: a withdrawal of `email_marketing` (preferences page, a customer's account) ends the
/// newsletter of that person at once, whichever subject recorded it.
pub async fn on_withdrawal(tx: &mut TenantTx, subject: &Subject) -> Result<(), Error> {
    let (email, customer) = match subject {
        Subject::Email(e) => (Some(e.clone()), None),
        Subject::Customer(c) => (None, Some(*c)),
        Subject::Anon(_) => return Ok(()),
    };
    sqlx::query!(
        "UPDATE subscribers s SET status = 'unsubscribed', unsubscribed_at = now(),
             confirm_token_hash = NULL, confirm_expires_at = NULL, updated_at = now()
         WHERE s.status IN ('pending', 'subscribed')
           AND (s.email = $1 OR s.customer_id = $2
                OR s.email = (SELECT c.email FROM customers c WHERE c.id = $2))",
        email,
        customer
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Why subscriber `id` must not get marketing mail now (`None` = may): no longer subscribed,
/// or the latest `email_marketing` choice of the address or its customer account is not a
/// grant (resolved at send time, A20).
pub async fn may_receive(tx: &mut TenantTx, id: Uuid) -> Result<Option<&'static str>, Error> {
    let Some(s) = sqlx::query!(
        "SELECT email, status, customer_id FROM subscribers WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(Some("not_subscribed"));
    };
    if s.status != "subscribed" {
        return Ok(Some("not_subscribed"));
    }
    let mut subjects = vec![Subject::Email(s.email)];
    subjects.extend(s.customer_id.map(Subject::Customer));
    let granted = consent::latest_any(tx, &subjects, ConsentPurpose::EmailMarketing)
        .await?
        .unwrap_or(false);
    Ok((!granted).then_some("no_consent"))
}

/// Marks the subscriber of `email` bounced or complained (deliverability, WP18).
pub async fn mark_undeliverable(
    tx: &mut TenantTx,
    email: &str,
    status: Status,
) -> Result<(), Error> {
    if !matches!(status, Status::Bounced | Status::Complained) {
        return Err(Error::Internal("not an undeliverable status".into()));
    }
    let changed = sqlx::query_scalar!(
        "UPDATE subscribers SET status = $2, confirm_token_hash = NULL,
             confirm_expires_at = NULL, updated_at = now()
         WHERE email = lower(btrim($1)) AND status <> $2
         RETURNING email",
        email,
        status.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?;
    // A complaint is the recipient saying "stop": record it as a withdrawal.
    if let (Some(email), Status::Complained) = (changed, status) {
        consent::record_server(
            tx,
            &Subject::Email(email),
            ConsentPurpose::EmailMarketing,
            false,
            consent::TEXT_VERSION,
            "complaint",
            None,
        )
        .await?;
    }
    Ok(())
}

/// `customer.email_verified`: subscribers with that address join the customer.
pub async fn link_customer(tx: &mut TenantTx, customer_id: Uuid) -> Result<u64, Error> {
    Ok(sqlx::query!(
        "UPDATE subscribers s SET customer_id = c.id, updated_at = now()
         FROM customers c
         WHERE c.id = $1 AND c.email_verified_at IS NOT NULL AND s.email = c.email
           AND s.customer_id IS DISTINCT FROM c.id",
        customer_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

// ---------------------------------------------------------------------------------------
// Admin

async fn get_opt(tx: &mut TenantTx, id: Uuid) -> Result<Option<Subscriber>, Error> {
    Ok(sqlx::query_as!(
        Row,
        "SELECT id, email, status, locale, market_id, customer_id, requested_at, confirmed_at,
                text_version, source, unsubscribed_at, created_at
         FROM subscribers WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(Subscriber::from))
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Subscriber, Error> {
    get_opt(tx, id).await?.ok_or(Error::NotFound)
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SubscriberFilter {
    pub status: Option<Status>,
    /// Part of the address (case-insensitive).
    pub q: Option<String>,
    pub locale: Option<String>,
    pub market_id: Option<Uuid>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<Uuid>,
    /// Page size, 1-100 (default 50).
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SubscriberPage {
    pub items: Vec<Subscriber>,
    pub next_cursor: Option<Uuid>,
    /// Matching subscribers in total (all pages).
    pub total: i64,
}

pub(crate) fn like_pattern(q: Option<&str>) -> Option<String> {
    q.map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        let escaped = q
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    })
}

/// Newest first.
pub async fn list(tx: &mut TenantTx, f: &SubscriberFilter) -> Result<SubscriberPage, Error> {
    let limit = f.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let pattern = like_pattern(f.q.as_deref());
    let status = f.status.map(Status::as_str);
    let mut items: Vec<Subscriber> = sqlx::query_as!(
        Row,
        "SELECT id, email, status, locale, market_id, customer_id, requested_at, confirmed_at,
                text_version, source, unsubscribed_at, created_at
         FROM subscribers
         WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR email LIKE $2)
           AND ($3::text IS NULL OR locale = $3) AND ($4::uuid IS NULL OR market_id = $4)
           AND ($5::uuid IS NULL OR id < $5)
         ORDER BY id DESC LIMIT $6",
        status,
        pattern,
        f.locale,
        f.market_id,
        f.cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Subscriber::from)
    .collect();
    let total = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM subscribers
           WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR email LIKE $2)
             AND ($3::text IS NULL OR locale = $3) AND ($4::uuid IS NULL OR market_id = $4)"#,
        status,
        pattern,
        f.locale,
        f.market_id
    )
    .fetch_one(&mut **tx)
    .await?;
    let limit = usize::try_from(limit).unwrap_or(50);
    let more = items.len() > limit;
    items.truncate(limit);
    Ok(SubscriberPage {
        next_cursor: if more {
            items.last().map(|s| s.id)
        } else {
            None
        },
        items,
        total,
    })
}

/// Staff unsubscribe someone (e.g. on request by phone), audited.
pub async fn admin_unsubscribe(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
) -> Result<Subscriber, Error> {
    let s = unsubscribe(tx, id, "admin", None)
        .await?
        .ok_or(Error::NotFound)?;
    crate::audit::record(
        tx,
        actor,
        "subscriber.unsubscribe",
        "subscriber",
        Some(&id.to_string()),
        &json!({ "status": s.status }),
    )
    .await?;
    Ok(s)
}

/// A CSV cell: quoted, and a leading `= + - @` defused so spreadsheets never run it.
fn csv_cell(v: &str) -> String {
    let v = if v.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_owned()
    };
    format!("\"{}\"", v.replace('"', "\"\""))
}

/// Every subscriber matching the filter (cursor/limit ignored) as CSV, audited (owner/admin
/// with a fresh sign-in, enforced by the API).
pub async fn export_csv(
    tx: &mut TenantTx,
    actor: &str,
    f: &SubscriberFilter,
) -> Result<String, Error> {
    let pattern = like_pattern(f.q.as_deref());
    let status = f.status.map(Status::as_str);
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, email, status, locale, market_id, customer_id, requested_at, confirmed_at,
                text_version, source, unsubscribed_at, created_at
         FROM subscribers
         WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR email LIKE $2)
           AND ($3::text IS NULL OR locale = $3) AND ($4::uuid IS NULL OR market_id = $4)
         ORDER BY id LIMIT $5",
        status,
        pattern,
        f.locale,
        f.market_id,
        MAX_EXPORT
    )
    .fetch_all(&mut **tx)
    .await?;
    let ts = |t: Option<DateTime<Utc>>| t.map(|t| t.to_rfc3339()).unwrap_or_default();
    let mut out = String::from(
        "email,status,locale,market_id,customer_id,requested_at,confirmed_at,text_version,source,unsubscribed_at\n",
    );
    for r in &rows {
        let cells = [
            r.email.clone(),
            r.status.clone(),
            r.locale.clone(),
            r.market_id.to_string(),
            r.customer_id.map(|c| c.to_string()).unwrap_or_default(),
            r.requested_at.to_rfc3339(),
            ts(r.confirmed_at),
            r.text_version.clone(),
            r.source.clone(),
            ts(r.unsubscribed_at),
        ];
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_cell(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    crate::audit::record(
        tx,
        actor,
        "subscriber.export",
        "subscriber",
        None,
        &json!({ "rows": rows.len(), "status": status }),
    )
    .await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_addresses() {
        assert_eq!(mask("jana@example.com"), "j***@example.com");
        assert_eq!(mask("broken"), "***");
    }

    #[test]
    fn csv_cells_are_quoted_and_defused() {
        assert_eq!(csv_cell("a@b.cz"), "\"a@b.cz\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("=HYPERLINK(1)"), "\"'=HYPERLINK(1)\"");
        assert_eq!(csv_cell("+420@x.cz"), "\"'+420@x.cz\"");
    }

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(
            like_pattern(Some(" Ja%n_ ")).as_deref(),
            Some("%ja\\%n\\_%")
        );
        assert_eq!(like_pattern(Some("  ")), None);
    }
}
