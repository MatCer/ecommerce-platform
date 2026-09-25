//! Staff views of the mail pipeline (WP18, follow-ups of WP9): the sent-email log (bodies only
//! of non-sensitive mail: sign-in links and order links never leave the database), the
//! suppression list with audited add/remove, the tenant's email logo and the editable
//! subject/intro texts per template and language (§11.4).

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::{LOCALES, SuppressionReason, Template, default_text, suppress};
use crate::markets::invalid;

// ---------------------------------------------------------------------------------------
// Sent-email log

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MessageFilter {
    /// `pending`, `sending`, `accepted`, `uncertain` or `failed`.
    pub status: Option<String>,
    /// `transactional` or `marketing`.
    pub stream: Option<String>,
    /// Part of the recipient address.
    pub to: Option<String>,
    pub cursor: Option<Uuid>,
    /// 1-100 (default 50).
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MessageSummary {
    pub id: Uuid,
    pub stream: String,
    pub template: String,
    pub to_email: String,
    pub locale: String,
    pub subject: String,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessagePage {
    pub items: Vec<MessageSummary>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    /// `true` for sign-in/order links: the body is never shown.
    pub sensitive: bool,
    /// The HTML body (render it sandboxed); `None` for sensitive mail.
    pub html: Option<String>,
    pub text: Option<String>,
    pub list_unsubscribe: Option<String>,
}

fn pattern(q: Option<&str>) -> Option<String> {
    q.map(str::trim).filter(|q| !q.is_empty()).map(|q| {
        format!(
            "%{}%",
            q.to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}

/// Newest first.
pub async fn list_messages(tx: &mut TenantTx, f: &MessageFilter) -> Result<MessagePage, Error> {
    let limit = f.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let mut items: Vec<MessageSummary> = sqlx::query_as!(
        MessageSummary,
        "SELECT id, stream, template, to_email, locale, subject, status, attempts, last_error,
                created_at, accepted_at
         FROM email_messages
         WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR stream = $2)
           AND ($3::text IS NULL OR to_email LIKE $3) AND ($4::uuid IS NULL OR id < $4)
         ORDER BY id DESC LIMIT $5",
        f.status,
        f.stream,
        pattern(f.to.as_deref()),
        f.cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let limit = usize::try_from(limit).unwrap_or(50);
    let more = items.len() > limit;
    items.truncate(limit);
    Ok(MessagePage {
        next_cursor: if more {
            items.last().map(|m| m.id)
        } else {
            None
        },
        items,
    })
}

pub async fn get_message(tx: &mut TenantTx, id: Uuid) -> Result<MessageDetail, Error> {
    let r = sqlx::query!(
        "SELECT id, stream, template, to_email, locale, subject, status, attempts, last_error,
                created_at, accepted_at, sensitive, html, body_text, list_unsubscribe
         FROM email_messages WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(MessageDetail {
        summary: MessageSummary {
            id: r.id,
            stream: r.stream,
            template: r.template,
            to_email: r.to_email,
            locale: r.locale,
            subject: r.subject,
            status: r.status,
            attempts: r.attempts,
            last_error: r.last_error,
            created_at: r.created_at,
            accepted_at: r.accepted_at,
        },
        sensitive: r.sensitive,
        html: if r.sensitive { None } else { r.html },
        text: if r.sensitive { None } else { r.body_text },
        list_unsubscribe: r.list_unsubscribe,
    })
}

// ---------------------------------------------------------------------------------------
// Suppressions

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Suppression {
    pub email: String,
    /// `bounce`, `complaint` or `manual`.
    pub reason: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuppressionFilter {
    /// Part of the address.
    pub q: Option<String>,
    /// `email` of the last row of the previous page.
    pub after: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuppressionPage {
    pub items: Vec<Suppression>,
    pub next_after: Option<String>,
}

pub async fn list_suppressions(
    tx: &mut TenantTx,
    f: &SuppressionFilter,
) -> Result<SuppressionPage, Error> {
    let limit = f.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let mut items = sqlx::query_as!(
        Suppression,
        "SELECT email, reason, note, created_at FROM email_suppressions
         WHERE ($1::text IS NULL OR email LIKE $1) AND ($2::text IS NULL OR email > $2)
         ORDER BY email LIMIT $3",
        pattern(f.q.as_deref()),
        f.after,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let limit = usize::try_from(limit).unwrap_or(50);
    let more = items.len() > limit;
    items.truncate(limit);
    Ok(SuppressionPage {
        next_after: if more {
            items.last().map(|s| s.email.clone())
        } else {
            None
        },
        items,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SuppressionInput {
    pub email: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Staff add a manual suppression (every stream), audited.
pub async fn add_suppression(
    tx: &mut TenantTx,
    actor: &str,
    input: &SuppressionInput,
) -> Result<Suppression, Error> {
    let email = crate::staff::normalize_email(&input.email)?;
    let note = input
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 500) {
        return Err(invalid(
            "invalid_note",
            "note must be at most 500 characters",
        ));
    }
    suppress(tx, &email, SuppressionReason::Manual, note).await?;
    crate::audit::record(
        tx,
        actor,
        "email_suppression.add",
        "email_suppression",
        Some(&email),
        &json!({ "reason": "manual", "note": note }),
    )
    .await?;
    Ok(sqlx::query_as!(
        Suppression,
        "SELECT email, reason, note, created_at FROM email_suppressions WHERE email = $1",
        email
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// Staff remove a suppression (e.g. a mailbox that works again), audited with what it was.
pub async fn remove_suppression(tx: &mut TenantTx, actor: &str, email: &str) -> Result<(), Error> {
    let email = crate::staff::normalize_email(email)?;
    let removed = sqlx::query!(
        "DELETE FROM email_suppressions WHERE email = $1 RETURNING reason, note",
        email
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    crate::audit::record(
        tx,
        actor,
        "email_suppression.remove",
        "email_suppression",
        Some(&email),
        &json!({ "reason": removed.reason, "note": removed.note }),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Branding: logo and texts

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmailBranding {
    /// An image asset of the tenant shown at the top of every email (`null` = the shop name).
    pub logo_asset_id: Option<Uuid>,
}

pub async fn branding(tx: &mut TenantTx) -> Result<EmailBranding, Error> {
    Ok(EmailBranding {
        logo_asset_id: sqlx::query_scalar!("SELECT logo_asset_id FROM email_settings")
            .fetch_optional(&mut **tx)
            .await?
            .flatten(),
    })
}

pub async fn set_branding(
    tx: &mut TenantTx,
    actor: &str,
    input: &EmailBranding,
) -> Result<EmailBranding, Error> {
    if let Some(asset) = input.logo_asset_id {
        let ready = sqlx::query_scalar!(
            r#"SELECT status = 'ready' AS "ready!" FROM assets WHERE id = $1"#,
            asset
        )
        .fetch_optional(&mut **tx)
        .await?;
        if ready != Some(true) {
            return Err(invalid(
                "invalid_logo",
                "the logo must be a processed image of this shop",
            ));
        }
    }
    sqlx::query!(
        "INSERT INTO email_settings (tenant_id, logo_asset_id) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET logo_asset_id = EXCLUDED.logo_asset_id,
             updated_at = now()",
        tx.tenant_id(),
        input.logo_asset_id
    )
    .execute(&mut **tx)
    .await?;
    crate::audit::record(
        tx,
        actor,
        "email_branding.update",
        "email_settings",
        None,
        &json!({ "logo_asset_id": input.logo_asset_id }),
    )
    .await?;
    branding(tx).await
}

/// One editable template text set in one language: the platform default and the tenant's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TemplateText {
    pub template: String,
    pub locale: String,
    pub default_subject: String,
    pub default_intro: String,
    /// The tenant's subject (`{shop}` and, for order mail, `{number}` are filled in).
    pub subject: Option<String>,
    /// The tenant's intro paragraph (`{shop}` is filled in).
    pub intro: Option<String>,
}

pub async fn texts(tx: &mut TenantTx) -> Result<Vec<TemplateText>, Error> {
    let rows = sqlx::query!("SELECT template, locale, subject, intro FROM email_template_texts")
        .fetch_all(&mut **tx)
        .await?;
    let mut out = Vec::new();
    for t in Template::EDITABLE {
        for locale in LOCALES {
            let custom = rows
                .iter()
                .find(|r| r.template == t.name() && r.locale == locale);
            out.push(TemplateText {
                template: t.name().to_owned(),
                locale: locale.to_owned(),
                default_subject: default_text(locale, &format!("{}.subject", t.name())),
                default_intro: default_text(locale, &format!("{}.intro", t.name())),
                subject: custom.and_then(|r| r.subject.clone()),
                intro: custom.and_then(|r| r.intro.clone()),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateTextInput {
    /// Empty or `null` = the platform default.
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub intro: Option<String>,
}

fn clean(v: Option<&String>, max: usize, what: &str) -> Result<Option<String>, Error> {
    let v = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Some(s) = &v
        && (s.chars().count() > max || s.chars().any(|c| c.is_control() && c != '\n'))
    {
        return Err(invalid(
            "invalid_text",
            format!("{what}: at most {max} characters, no control characters"),
        ));
    }
    Ok(v)
}

/// Sets (or, with both empty, resets) the tenant's texts of `template` in `locale`, audited.
pub async fn set_text(
    tx: &mut TenantTx,
    actor: &str,
    template: &str,
    locale: &str,
    input: &TemplateTextInput,
) -> Result<TemplateText, Error> {
    let t = Template::editable(template).ok_or(Error::NotFound)?;
    if !LOCALES.contains(&locale) {
        return Err(Error::NotFound);
    }
    let subject = clean(input.subject.as_ref(), 200, "subject")?;
    if subject.as_deref().is_some_and(|s| s.contains('\n')) {
        return Err(invalid("invalid_text", "subject: one line"));
    }
    let intro = clean(input.intro.as_ref(), 1000, "intro")?;
    if subject.is_none() && intro.is_none() {
        sqlx::query!(
            "DELETE FROM email_template_texts WHERE template = $1 AND locale = $2",
            t.name(),
            locale
        )
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query!(
            "INSERT INTO email_template_texts (tenant_id, template, locale, subject, intro)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, template, locale) DO UPDATE
                 SET subject = EXCLUDED.subject, intro = EXCLUDED.intro, updated_at = now()",
            tx.tenant_id(),
            t.name(),
            locale,
            subject,
            intro
        )
        .execute(&mut **tx)
        .await?;
    }
    crate::audit::record(
        tx,
        actor,
        "email_text.update",
        "email_template_text",
        Some(&format!("{}:{locale}", t.name())),
        &json!({ "subject": subject, "intro": intro }),
    )
    .await?;
    Ok(TemplateText {
        template: t.name().to_owned(),
        locale: locale.to_owned(),
        default_subject: default_text(locale, &format!("{}.subject", t.name())),
        default_intro: default_text(locale, &format!("{}.intro", t.name())),
        subject,
        intro,
    })
}
