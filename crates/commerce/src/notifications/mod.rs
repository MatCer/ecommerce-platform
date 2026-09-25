//! Email notifications (spec §11.4, §13, A14, A29).
//!
//! Every email is a row in `email_messages`, rendered when it is enqueued (MJML via `mrml`,
//! variables via minijinja, localized cs/sk/en, tenant-branded, with a plain-text part), plus
//! a `mail.send` job enqueued in the same transaction: an email exists exactly when the
//! business change that caused it commits.
//!
//! The job ([`deliver`]) moves the row through `pending → sending → accepted | uncertain |
//! failed`:
//! - `sending` is committed before SMTP is called and `accepted` only after the 250 reply;
//! - a later attempt that finds `sending` (the worker died mid-send) marks it `uncertain`;
//! - an `uncertain` transactional message is sent once more (a duplicate is tolerated, the
//!   Message-ID stays the same); marketing mail is never resent;
//! - the suppression list is checked right before every send.

pub mod brand;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use minijinja::value::Kwargs;
use minijinja::{AutoEscape, Environment, State, UndefinedBehavior};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::mail::{Delivery, Mailer, Outgoing, Stream};
use platform::queue::{self, NewJob};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub use brand::Brand;

/// Job kind: deliver one `email_messages` row.
pub const SEND_JOB: &str = "mail.send";
/// SMTP attempts before a message that could not be handed over is given up (`failed`).
const SMTP_ATTEMPTS: i32 = 8;
/// Job attempts: more than SMTP attempts, so the job normally outlives the message's own
/// decisions (the one uncertain retry included). If it still dies, the hourly reconciliation
/// ([`reconcile_jobs`]) queues a new job.
const SEND_JOB_ATTEMPTS: i32 = SMTP_ATTEMPTS + 4;

// ---------------------------------------------------------------------------------------
// Templates

type Catalog = BTreeMap<String, String>;

fn parse(json: &str) -> Catalog {
    serde_json::from_str(json).unwrap_or_default()
}

static CS: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/cs.json")));
static SK: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/sk.json")));
static EN: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/en.json")));

fn catalog(locale: &str) -> &'static Catalog {
    match locale.split('-').next() {
        Some("cs") => &CS,
        Some("sk") => &SK,
        _ => &EN,
    }
}

/// A message with `{name}` placeholders filled in (the key itself when missing).
/// A catalog text in `locale` (labels that Rust puts into template variables).
pub(crate) fn label(locale: &str, key: &str) -> String {
    text(locale, key, &[])
}

fn text(locale: &str, key: &str, args: &[(&str, String)]) -> String {
    let mut out = catalog(locale)
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.to_owned());
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// `{{ t("key", name=value) }}` inside templates, in the render's `locale`.
fn t(state: &State, key: &str, kwargs: Kwargs) -> Result<String, minijinja::Error> {
    let locale = state
        .lookup("locale")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut args = Vec::new();
    for name in kwargs.args() {
        let v: minijinja::Value = kwargs.get(name)?;
        args.push((name, v.to_string()));
    }
    Ok(text(&locale, key, &args))
}

static TEMPLATES: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // MJML is markup: every variable is HTML-escaped. Plain text is not.
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".mjml") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    // minijinja's HTML escaping also encodes `/`, which turns every link into
    // `http:&#x2f;&#x2f;…`: valid, but mail clients and spam filters dislike it. Escape exactly
    // what can break out of text or a quoted attribute.
    env.set_formatter(|out, state, value| {
        if state.auto_escape() != AutoEscape::Html || value.is_safe() {
            return minijinja::escape_formatter(out, state, value);
        }
        let raw = value.to_string();
        let mut escaped = String::with_capacity(raw.len());
        for c in raw.chars() {
            match c {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&#39;"),
                c => escaped.push(c),
            }
        }
        std::fmt::Write::write_str(out, &escaped)
            .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::WriteFailure, e.to_string()))
    });
    env.add_function("t", t);
    for (name, source) in [
        ("layout.mjml", include_str!("templates/layout.mjml")),
        ("layout.txt", include_str!("templates/layout.txt")),
        ("magic_link.mjml", include_str!("templates/magic_link.mjml")),
        ("magic_link.txt", include_str!("templates/magic_link.txt")),
        (
            "password_changed.mjml",
            include_str!("templates/password_changed.mjml"),
        ),
        (
            "password_changed.txt",
            include_str!("templates/password_changed.txt"),
        ),
        (
            "staff_invite.mjml",
            include_str!("templates/staff_invite.mjml"),
        ),
        (
            "staff_invite.txt",
            include_str!("templates/staff_invite.txt"),
        ),
        ("order.mjml", include_str!("templates/order.mjml")),
        ("order.txt", include_str!("templates/order.txt")),
        (
            "order_confirmation.mjml",
            include_str!("templates/order_confirmation.mjml"),
        ),
        (
            "order_confirmation.txt",
            include_str!("templates/order_confirmation.txt"),
        ),
    ] {
        // Templates are compiled into the binary and covered by tests.
        if let Err(e) = env.add_template(name, source) {
            tracing::error!(template = name, error = %e, "email template does not parse");
        }
    }
    env
});

/// The templates with a subject line (layouts and skeletons have none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    MagicLink,
    PasswordChanged,
    StaffInvite,
    /// The order skeleton (WP10-12 add concrete order emails on top of it).
    Order,
    /// Order placed (WP10): summary, VAT recap, delivery and payment.
    OrderConfirmation,
}

impl Template {
    pub fn name(self) -> &'static str {
        match self {
            Self::MagicLink => "magic_link",
            Self::PasswordChanged => "password_changed",
            Self::StaffInvite => "staff_invite",
            Self::Order => "order",
            Self::OrderConfirmation => "order_confirmation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub html: String,
    pub text: String,
}

/// Renders `template` with `vars` (a JSON object) for `locale` and `brand`.
pub fn render(
    template: Template,
    locale: &str,
    brand: &Brand,
    vars: &Value,
) -> Result<Rendered, Error> {
    let name = template.name();
    let mut subject_args = vec![("shop", brand.shop_name.clone())];
    if let Some(number) = vars.pointer("/order/number").and_then(Value::as_str) {
        subject_args.push(("number", number.to_owned()));
    }
    let subject = text(locale, &format!("{name}.subject"), &subject_args);
    let mut ctx = match vars {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    ctx.insert("locale".into(), json!(locale));
    ctx.insert("subject".into(), json!(subject));
    ctx.insert("preview".into(), json!(subject));
    ctx.insert(
        "brand".into(),
        serde_json::to_value(brand).map_err(|e| Error::Internal(e.to_string()))?,
    );
    let render = |file: String| -> Result<String, Error> {
        TEMPLATES
            .get_template(&file)
            .and_then(|t| t.render(&ctx))
            .map_err(|e| Error::Internal(format!("email template {file}: {e}")))
    };
    let mjml = render(format!("{name}.mjml"))?;
    let html =
        platform::mail::render_mjml(&mjml).map_err(|e| Error::Internal(format!("{name}: {e}")))?;
    let text = render(format!("{name}.txt"))?;
    Ok(Rendered {
        subject,
        html,
        text: text.trim().to_owned() + "\n",
    })
}

// ---------------------------------------------------------------------------------------
// Enqueueing

/// One email to send.
#[derive(Debug, Clone)]
pub struct Email<'a> {
    pub template: Template,
    pub stream: Stream,
    pub to: &'a str,
    pub locale: &'a str,
    pub vars: Value,
    /// Enqueueing the same key twice (per tenant) is a no-op, e.g. `magic_link:<hash>`.
    pub idempotency_key: String,
    /// Holds a credential (sign-in link): the body is deleted once the message is final.
    pub sensitive: bool,
}

/// Renders the email and stores it with its `mail.send` job in the caller's transaction.
/// Returns the message id (the existing one for a repeated idempotency key).
pub async fn enqueue(tx: &mut TenantTx, brand: &Brand, email: Email<'_>) -> Result<Uuid, Error> {
    let r = render(email.template, email.locale, brand, &email.vars)?;
    let inserted = sqlx::query_scalar!(
        "INSERT INTO email_messages (tenant_id, stream, template, idempotency_key, to_email, locale,
                                     subject, html, body_text, sensitive)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT ON CONSTRAINT email_messages_idempotency DO NOTHING
         RETURNING id",
        tx.tenant_id(),
        email.stream.as_str(),
        email.template.name(),
        email.idempotency_key,
        email.to,
        email.locale.split('-').next().unwrap_or("en"),
        r.subject,
        r.html,
        r.text,
        email.sensitive
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(id) = inserted else {
        return Ok(sqlx::query_scalar!(
            "SELECT id FROM email_messages WHERE idempotency_key = $1",
            email.idempotency_key
        )
        .fetch_one(&mut **tx)
        .await?);
    };
    let mut job = NewJob::new(SEND_JOB, json!({ "message_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = SEND_JOB_ATTEMPTS;
    job.idempotency_key = Some(format!("mail:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    Ok(id)
}

// ---------------------------------------------------------------------------------------
// Suppression (A29)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    Bounce,
    Complaint,
    Manual,
}

impl SuppressionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bounce => "bounce",
            Self::Complaint => "complaint",
            Self::Manual => "manual",
        }
    }
}

/// Adds (or updates) a suppression for `email`.
pub async fn suppress(
    tx: &mut TenantTx,
    email: &str,
    reason: SuppressionReason,
    note: Option<&str>,
) -> Result<(), Error> {
    let email = crate::staff::normalize_email(email)?;
    sqlx::query!(
        "INSERT INTO email_suppressions (tenant_id, email, reason, note) VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, email) DO UPDATE SET reason = EXCLUDED.reason, note = EXCLUDED.note",
        tx.tenant_id(),
        email,
        reason.as_str(),
        note
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Whether `email` must not receive mail of `stream` (complaints block marketing only).
pub async fn is_suppressed(tx: &mut TenantTx, email: &str, stream: Stream) -> Result<bool, Error> {
    let reason = sqlx::query_scalar!(
        "SELECT reason FROM email_suppressions WHERE email = lower(btrim($1))",
        email
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match reason.as_deref() {
        None => false,
        Some("complaint") => stream == Stream::Marketing,
        Some(_) => true,
    })
}

// ---------------------------------------------------------------------------------------
// Delivery (A14)

/// What the job should do after [`deliver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The message reached a final state (or needs nothing more from this job).
    Done,
    /// Try again later (backoff); the reason is for the job log.
    Retry(String),
}

/// A send in flight: `sending` was committed with `token`. Only the holder of the token may
/// record the outcome, so a worker that lost its lease cannot overwrite a newer attempt.
#[derive(Debug, Clone)]
pub struct Sending {
    pub tenant: Uuid,
    pub id: Uuid,
    pub token: Uuid,
    pub stream: Stream,
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
    /// SMTP attempts including this one.
    pub attempts: i32,
    pub uncertain_count: i16,
}

/// Longest a send can legitimately stay `sending` (SMTP timeout 30 s, lease 60 s). Older
/// `sending` rows belong to a worker that died mid-send.
const SENDING_STALE_SECS: f64 = 120.0;

/// Decides whether message `id` is sent now and, if so, commits `sending` with a fresh token.
/// `Err(Step)` when there is nothing to send now (final state, suppressed, or another worker is
/// sending it right now).
pub async fn begin_send(
    db: &PgPool,
    tenant: Uuid,
    id: Uuid,
) -> Result<Result<Sending, Step>, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let Some(m) = sqlx::query!(
        r#"SELECT stream, to_email, subject, html, body_text, status, uncertain_count, attempts,
                  updated_at < now() - make_interval(secs => $2) AS "stale!"
           FROM email_messages WHERE id = $1 FOR UPDATE"#,
        id,
        SENDING_STALE_SECS
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(Err(Step::Done));
    };
    let stream = Stream::parse(&m.stream)
        .ok_or_else(|| Error::Internal(format!("unknown stream {}", m.stream)))?;
    let mut uncertain = m.uncertain_count;
    match m.status.as_str() {
        "pending" | "uncertain" => {}
        // Another job is sending it right now (e.g. the reconciliation sweep overlapped).
        "sending" if !m.stale => {
            return Ok(Err(Step::Retry("the message is being sent".into())));
        }
        // An earlier attempt died between `sending` and the SMTP answer.
        "sending" => {
            uncertain += 1;
            sqlx::query!(
                "UPDATE email_messages SET status = 'uncertain', uncertain_count = $2,
                     send_token = NULL, last_error = 'worker stopped while sending',
                     updated_at = now()
                 WHERE id = $1",
                id,
                uncertain
            )
            .execute(&mut *tx)
            .await?;
        }
        _ => return Ok(Err(Step::Done)), // accepted, failed
    }
    // A14: one retry for transactional mail whose first send ended uncertain, none for
    // marketing, none after a second uncertain send.
    let refusal = if uncertain > 0 && !(stream == Stream::Transactional && uncertain == 1) {
        Some(("uncertain", None))
    } else if is_suppressed(&mut tx, &m.to_email, stream).await? {
        Some(("failed", Some("suppressed")))
    } else if m.html.is_none() || m.body_text.is_none() {
        Some(("failed", Some("message body is gone")))
    } else {
        None
    };
    if let Some((status, error)) = refusal {
        finish(&mut tx, id, status, error).await?;
        tx.commit().await?;
        return Ok(Err(Step::Done));
    }
    let token = Uuid::now_v7();
    sqlx::query!(
        "UPDATE email_messages SET status = 'sending', send_token = $2, attempts = attempts + 1,
             updated_at = now()
         WHERE id = $1",
        id,
        token
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Ok(Sending {
        tenant,
        id,
        token,
        stream,
        to: m.to_email,
        subject: m.subject,
        html: m.html.unwrap_or_default(),
        text: m.body_text.unwrap_or_default(),
        attempts: m.attempts + 1,
        uncertain_count: uncertain,
    }))
}

/// Final state: sensitive bodies (sign-in links) are deleted.
async fn finish(
    tx: &mut TenantTx,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE email_messages SET status = $2, last_error = coalesce($3, last_error),
             send_token = NULL,
             accepted_at = CASE WHEN $2 = 'accepted' THEN now() ELSE accepted_at END,
             html = CASE WHEN sensitive THEN NULL ELSE html END,
             body_text = CASE WHEN sensitive THEN NULL ELSE body_text END,
             updated_at = now()
         WHERE id = $1",
        id,
        status,
        error.map(|e| e.chars().take(1000).collect::<String>())
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records the SMTP outcome of `s`, fenced by its token: if another attempt took the message
/// over in the meantime, nothing is written and the job is done.
pub async fn finish_send(db: &PgPool, s: &Sending, outcome: Delivery) -> Result<Step, Error> {
    let mut tx = tenant_tx(db, s.tenant).await?;
    let current = sqlx::query_scalar!(
        "SELECT send_token FROM email_messages WHERE id = $1 AND status = 'sending' FOR UPDATE",
        s.id
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    if current != Some(s.token) {
        tracing::warn!(message = %s.id, "send outcome arrived after another attempt took over");
        return Ok(Step::Done);
    }
    let error = |e: &str| e.chars().take(1000).collect::<String>();
    let step = match outcome {
        Delivery::Accepted => {
            finish(&mut tx, s.id, "accepted", None).await?;
            Step::Done
        }
        Delivery::Rejected(e) => {
            finish(&mut tx, s.id, "failed", Some(&e)).await?;
            Step::Done
        }
        // Nothing was handed over: back to where it was, or failed when out of attempts.
        Delivery::NotSent(e) if s.attempts >= SMTP_ATTEMPTS => {
            finish(&mut tx, s.id, "failed", Some(&e)).await?;
            Step::Done
        }
        Delivery::NotSent(e) => {
            let back = if s.uncertain_count > 0 {
                "uncertain"
            } else {
                "pending"
            };
            sqlx::query!(
                "UPDATE email_messages SET status = $2, send_token = NULL, last_error = $3,
                     updated_at = now()
                 WHERE id = $1",
                s.id,
                back,
                error(&e)
            )
            .execute(&mut *tx)
            .await?;
            Step::Retry(e)
        }
        Delivery::Uncertain(e) => {
            let count = s.uncertain_count + 1;
            sqlx::query!(
                "UPDATE email_messages SET status = 'uncertain', uncertain_count = $2,
                     send_token = NULL, last_error = $3, updated_at = now()
                 WHERE id = $1",
                s.id,
                count,
                error(&e)
            )
            .execute(&mut *tx)
            .await?;
            if s.stream == Stream::Transactional && count == 1 {
                Step::Retry(e)
            } else {
                finish(&mut tx, s.id, "uncertain", None).await?;
                Step::Done
            }
        }
    };
    tx.commit().await?;
    Ok(step)
}

/// The `mail.send` job: sends message `id` of `tenant` if its state allows it.
pub async fn deliver(db: &PgPool, mailer: &Mailer, tenant: Uuid, id: Uuid) -> Result<Step, Error> {
    let s = match begin_send(db, tenant, id).await? {
        Ok(s) => s,
        Err(step) => return Ok(step),
    };
    let shop_name = {
        let mut tx = tenant_tx(db, tenant).await?;
        let name = sqlx::query_scalar!("SELECT name FROM platform.tenants WHERE id = $1", tenant)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        name
    };
    let id_text = id.to_string();
    let outcome = mailer
        .send(&Outgoing {
            stream: s.stream,
            from_name: &shop_name,
            to: &s.to,
            subject: &s.subject,
            html: &s.html,
            text: &s.text,
            id: &id_text,
        })
        .await;
    finish_send(db, &s, outcome).await
}

/// Messages whose delivery stalled (their job gave up, or a worker died mid-send), across
/// tenants, for the hourly reconciliation in `maintenance.cleanup`: returns a `mail.send` job
/// per message (idempotent per stall).
pub async fn reconcile_jobs(db: &PgPool) -> Result<Vec<NewJob<'static>>, Error> {
    let rows = sqlx::query!(
        r#"SELECT tenant_id AS "tenant_id!", id AS "id!", stalled_since AS "stalled_since!"
           FROM platform.stalled_email_messages(500)"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let mut job = NewJob::new(SEND_JOB, json!({ "message_id": r.id }));
            job.tenant_id = Some(r.tenant_id);
            job.max_attempts = SEND_JOB_ATTEMPTS;
            job.idempotency_key = Some(format!(
                "mail:{}:{}",
                r.id,
                r.stalled_since.timestamp_micros()
            ));
            job
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand() -> Brand {
        Brand {
            shop_name: "Kočka & <Pes>".into(),
            shop_url: "http://demo.localhost:8080".into(),
            colors: brand::Colors::default(),
        }
    }

    #[test]
    fn catalogs_are_aligned() {
        let keys = |c: &Catalog| c.keys().cloned().collect::<Vec<_>>();
        assert!(CS.len() > 10);
        assert_eq!(keys(&CS), keys(&SK));
        assert_eq!(keys(&CS), keys(&EN));
    }

    #[test]
    fn magic_link_renders_localized_escaped_html_and_text() {
        let r = render(
            Template::MagicLink,
            "cs",
            &brand(),
            &json!({"url": "http://checkout.demo.localhost:8080/account/verify?token=abc&x=1", "minutes": 15}),
        )
        .unwrap();
        assert_eq!(r.subject, "Přihlášení do obchodu Kočka & <Pes>");
        assert!(
            r.html.contains("Kočka &amp; &lt;Pes&gt;"),
            "shop name is escaped"
        );
        assert!(!r.html.contains("<Pes>"));
        assert!(
            r.html
                .contains("http://checkout.demo.localhost:8080/account/verify?token=abc&amp;x=1")
        );
        assert!(r.html.contains("Odkaz platí 15 minut"));
        assert!(
            r.text.contains("verify?token=abc&x=1"),
            "plain text is not escaped"
        );
        assert!(r.text.contains("Kočka & <Pes>"));
        assert!(!r.text.contains("<mj-"));
        let en = render(
            Template::MagicLink,
            "en",
            &brand(),
            &json!({"url": "http://x/", "minutes": 15}),
        )
        .unwrap();
        assert!(en.subject.starts_with("Sign in to"));
    }

    #[test]
    fn every_template_renders_in_every_locale() {
        let vars = json!({
            "url": "http://x/",
            "minutes": 15,
            "order": {
                "number": "2026000123",
                "lines": [{"name": "Tričko", "detail": "M / Zelená", "quantity": 2, "total": "798 Kč"}],
                "totals": [{"label": "Doprava", "amount": "79 Kč"}],
                "total": "877 Kč",
                "url": "http://x/o/abc"
            },
            "vat": [{"rate": "21", "net": "724,79 Kč", "vat": "152,21 Kč"}],
            "shipping": {"method": "Zásilkovna", "pickup_point": "Z-BOX, Dlouhá 1, 110 00 Praha", "address": null},
            "payment": {"method": "Dobírka", "bank_transfer": false, "cod": true}
        });
        for locale in ["cs", "sk", "en"] {
            for t in [
                Template::MagicLink,
                Template::PasswordChanged,
                Template::StaffInvite,
                Template::Order,
                Template::OrderConfirmation,
            ] {
                let r = render(t, locale, &brand(), &vars).unwrap();
                assert!(!r.subject.contains('{'), "{locale} {t:?}: {}", r.subject);
                assert!(r.html.contains("<!doctype html>"));
                assert!(!r.text.trim().is_empty());
                assert!(
                    !r.html.contains(&format!("{}.", t.name())),
                    "untranslated key"
                );
            }
        }
        let order = render(Template::Order, "cs", &brand(), &vars).unwrap();
        assert_eq!(order.subject, "Objednávka 2026000123");
        assert!(order.html.contains("Zelená") && order.html.contains("877 Kč"));
        assert!(order.text.contains("2x Tričko (M / Zelená)  798 Kč"));
        let confirmation = render(Template::OrderConfirmation, "cs", &brand(), &vars).unwrap();
        assert_eq!(confirmation.subject, "Potvrzení objednávky 2026000123");
        for text in [&confirmation.html, &confirmation.text] {
            assert!(text.contains("Z-BOX, Dlouhá 1"), "{text}");
            assert!(text.contains("152,21 Kč"));
            assert!(text.contains("Zaplatíte při převzetí"));
        }
    }

    #[test]
    fn missing_variables_are_errors_not_blanks() {
        assert!(render(Template::MagicLink, "cs", &brand(), &json!({})).is_err());
    }
}
