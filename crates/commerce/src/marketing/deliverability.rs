//! Bounce and complaint ingestion (spec §11.4, A29): Amazon SES notifications delivered by SNS
//! over HTTPS (`POST /webhooks/ses`). Locally the same JSON is posted by tests/mocks.
//!
//! Shape: an SNS envelope (`Type` = `Notification`, `SubscriptionConfirmation`,
//! `UnsubscribeConfirmation`) whose `Message` is the SES notification, either the
//! notification-topic form (`notificationType`) or the event-publishing form (`eventType`).
//! A permanent bounce suppresses the address for every stream and marks the subscriber
//! `bounced`; a complaint suppresses marketing, marks `complained` and records the consent
//! withdrawal. Transient bounces and other types are ignored.
//!
//! The tenant is found through our own Message-ID (`<email_messages.id@domain>`, in
//! `mail.commonHeaders.messageId`, or `mail.headers` with "include original headers"), and only
//! the recipient that message was sent to is ever suppressed, so a notification can never
//! suppress an arbitrary address of another shop.
//!
//! Authentication (API layer): the SNS subscription URL carries HTTP Basic credentials
//! (`https://ses:<MAIL_EVENTS_SECRET>@api.example/webhooks/ses`, a documented SNS feature), and
//! the endpoint is off without the secret. Production refuses to start with the secret until
//! full SNS verification is implemented: accept only
//! `SignatureVersion` 2 (RSA-SHA256), fetch `SigningCertURL` only when it is
//! `https://sns.<region>.amazonaws.com/...pem` (through the SSRF-safe client, cached per URL),
//! verify the signature over the canonical string of the message type, check `TopicArn`
//! against the configured topic, and reject messages older than an hour. Subscription
//! confirmations are never followed automatically; subscription URLs are never logged.

use platform::Error;
use platform::db::tenant_tx;
use platform::mail::Stream;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::subscribers::{self, Status};
use crate::notifications::{self, SuppressionReason};

/// The SNS HTTPS envelope (fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct SnsEnvelope {
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(rename = "MessageId")]
    pub message_id: String,
    #[serde(rename = "TopicArn", default)]
    pub topic_arn: String,
    #[serde(rename = "Message", default)]
    pub message: String,
    #[serde(rename = "SubscribeURL", default)]
    pub subscribe_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Bounce {
        permanent: bool,
        recipients: Vec<String>,
        feedback_id: String,
    },
    Complaint {
        recipients: Vec<String>,
        feedback_id: String,
    },
    Other(String),
}

/// A parsed SES notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SesEvent {
    pub notice: Notice,
    /// Our message id from the Message-ID header, when present.
    pub message: Option<Uuid>,
}

fn invalid_event(detail: &str) -> Error {
    Error::Validation {
        code: "invalid_event",
        detail: detail.into(),
    }
}

/// `<0190...@mail.example>` → the id.
fn our_id(message_id: &str) -> Option<Uuid> {
    let inner = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    Uuid::parse_str(inner.split('@').next()?).ok()
}

fn recipients(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("emailAddress").and_then(Value::as_str))
                .map(|e| e.trim().to_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Parses the SES notification in an SNS `Message`.
pub fn parse_ses(message: &str) -> Result<SesEvent, Error> {
    let v: Value =
        serde_json::from_str(message).map_err(|_| invalid_event("Message is not JSON"))?;
    let kind = v
        .get("notificationType")
        .or_else(|| v.get("eventType"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_event("no notificationType/eventType"))?;
    let mail = v.get("mail");
    let from_common = mail
        .and_then(|m| m.pointer("/commonHeaders/messageId"))
        .and_then(Value::as_str)
        .and_then(our_id);
    let from_headers = || {
        mail.and_then(|m| m.get("headers"))
            .and_then(Value::as_array)?
            .iter()
            .find(|h| {
                h.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.eq_ignore_ascii_case("message-id"))
            })
            .and_then(|h| h.get("value").and_then(Value::as_str))
            .and_then(our_id)
    };
    let message = from_common.or_else(from_headers);
    let feedback = |section: &Value| {
        section
            .get("feedbackId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>()
    };
    let notice = match kind {
        "Bounce" => {
            let b = v.get("bounce").ok_or_else(|| invalid_event("no bounce"))?;
            Notice::Bounce {
                permanent: b.get("bounceType").and_then(Value::as_str) == Some("Permanent"),
                recipients: recipients(b.get("bouncedRecipients")),
                feedback_id: feedback(b),
            }
        }
        "Complaint" => {
            let c = v
                .get("complaint")
                .ok_or_else(|| invalid_event("no complaint"))?;
            Notice::Complaint {
                recipients: recipients(c.get("complainedRecipients")),
                feedback_id: feedback(c),
            }
        }
        other => Notice::Other(other.chars().take(40).collect()),
    };
    Ok(SesEvent { notice, message })
}

/// What an event changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Suppressed addresses (0 or 1: only the message's own recipient).
    Suppressed(usize),
    Ignored(&'static str),
}

/// Applies a bounce/complaint: suppression, subscriber status, campaign send flags. Idempotent
/// (a redelivered notification changes nothing more).
pub async fn apply(db: &PgPool, event: &SesEvent) -> Result<Applied, Error> {
    let (reason, status, listed, feedback) = match &event.notice {
        Notice::Bounce {
            permanent: true,
            recipients,
            feedback_id,
        } => (
            SuppressionReason::Bounce,
            Status::Bounced,
            recipients,
            feedback_id,
        ),
        Notice::Bounce { .. } => return Ok(Applied::Ignored("transient bounce")),
        Notice::Complaint {
            recipients,
            feedback_id,
        } => (
            SuppressionReason::Complaint,
            Status::Complained,
            recipients,
            feedback_id,
        ),
        Notice::Other(_) => return Ok(Applied::Ignored("not a bounce or complaint")),
    };
    let Some(message) = event.message else {
        return Ok(Applied::Ignored("no message id of ours"));
    };
    let Some(tenant) = sqlx::query_scalar!(
        r#"SELECT platform.email_message_tenant($1) AS "tenant""#,
        message
    )
    .fetch_one(db)
    .await?
    else {
        return Ok(Applied::Ignored("unknown message"));
    };
    let mut tx = tenant_tx(db, tenant).await?;
    let to = sqlx::query_scalar!("SELECT to_email FROM email_messages WHERE id = $1", message)
        .fetch_one(&mut *tx)
        .await?;
    let to = to.trim().to_lowercase();
    if !listed.contains(&to) {
        return Ok(Applied::Ignored("recipient does not match the message"));
    }
    let note =
        format!("ses {}", reason.as_str()) + if feedback.is_empty() { "" } else { " " } + feedback;
    // A complaint never downgrades an existing (harder) bounce suppression.
    let existing = notifications::is_suppressed(&mut tx, &to, Stream::Transactional).await?;
    if !(existing && reason == SuppressionReason::Complaint) {
        notifications::suppress(&mut tx, &to, reason, Some(&note)).await?;
    }
    subscribers::mark_undeliverable(&mut tx, &to, status).await?;
    let column_bounce = status == Status::Bounced;
    sqlx::query!(
        "UPDATE campaign_sends SET
             bounced_at = CASE WHEN $2 THEN coalesce(bounced_at, now()) ELSE bounced_at END,
             complained_at = CASE WHEN $2 THEN complained_at ELSE coalesce(complained_at, now()) END
         WHERE message_id = $1",
        message,
        column_bounce
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Applied::Suppressed(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_notification_and_event_publishing_shapes() {
        let id = Uuid::now_v7();
        let bounce = json!({
            "notificationType": "Bounce",
            "bounce": {"bounceType": "Permanent", "feedbackId": "fb-1",
                       "bouncedRecipients": [{"emailAddress": "Jana@Example.com"}]},
            "mail": {"messageId": "ses-1", "commonHeaders": {"messageId": format!("<{id}@mail.example>")}}
        });
        let e = parse_ses(&bounce.to_string()).unwrap();
        assert_eq!(e.message, Some(id));
        assert_eq!(
            e.notice,
            Notice::Bounce {
                permanent: true,
                recipients: vec!["jana@example.com".into()],
                feedback_id: "fb-1".into()
            }
        );
        let complaint = json!({
            "eventType": "Complaint",
            "complaint": {"complainedRecipients": [{"emailAddress": "a@b.cz"}]},
            "mail": {"headers": [{"name": "Message-ID", "value": format!("<{id}@x>")}]}
        });
        let e = parse_ses(&complaint.to_string()).unwrap();
        assert_eq!(e.message, Some(id));
        assert!(matches!(e.notice, Notice::Complaint { .. }));
        let delivery = json!({"notificationType": "Delivery", "mail": {}});
        assert_eq!(
            parse_ses(&delivery.to_string()).unwrap().notice,
            Notice::Other("Delivery".into())
        );
        assert!(parse_ses("not json").is_err());
        assert!(parse_ses("{}").is_err());
        let foreign = json!({"notificationType": "Bounce", "bounce": {"bounceType": "Transient"},
                             "mail": {"commonHeaders": {"messageId": "<abc@ses>"}}});
        assert_eq!(parse_ses(&foreign.to_string()).unwrap().message, None);
    }
}
