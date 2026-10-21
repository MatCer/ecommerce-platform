//! SNS message verification for SES events (SHOP-8). With `MAIL_EVENTS_TOPIC_ARNS` set, every
//! message to `POST /webhooks/ses` must come from an allowlisted topic, be at most an hour off
//! our clock and carry a `SignatureVersion` 2 (RSA-SHA256) signature by the certificate at its
//! `SigningCertURL`, which is fetched only from `https://sns.<topic region>.amazonaws.com/
//! SimpleNotificationService-*.pem` through the SSRF-safe client and cached per URL.
//! Processed notifications are remembered by `(TopicArn, MessageId)` for a day, so a replay
//! changes nothing.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, TimeDelta, Utc};
use platform::Error;
use platform::http::{Limits, SafeClient};
use reqwest::Url;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde::Deserialize;
use sqlx::PgPool;
use webpki::EndEntityCert;

/// The SNS HTTPS envelope (fields we use). The signature fields default to empty so unsigned
/// dev fixtures still parse.
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
    #[serde(rename = "Subject", default)]
    pub subject: Option<String>,
    #[serde(rename = "SubscribeURL", default)]
    pub subscribe_url: Option<String>,
    #[serde(rename = "Token", default)]
    pub token: Option<String>,
    #[serde(rename = "Timestamp", default)]
    pub timestamp: String,
    #[serde(rename = "SignatureVersion", default)]
    pub signature_version: String,
    #[serde(rename = "Signature", default)]
    pub signature: String,
    #[serde(rename = "SigningCertURL", default)]
    pub signing_cert_url: String,
}

/// `arn:aws:sns:<region>:<12-digit account>:<topic name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicArn {
    arn: String,
    region: String,
}

impl TopicArn {
    pub fn parse(arn: &str) -> Option<Self> {
        let rest = arn.strip_prefix("arn:aws:sns:")?;
        let mut parts = rest.split(':');
        let (region, account, name) = (parts.next()?, parts.next()?, parts.next()?);
        let valid = parts.next().is_none()
            && !region.is_empty()
            && region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && account.len() == 12
            && account.bytes().all(|b| b.is_ascii_digit())
            && (1..=256).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        valid.then(|| Self {
            arn: arn.to_owned(),
            region: region.to_owned(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.arn
    }
}

const MAX_SKEW: TimeDelta = TimeDelta::hours(1);
const CERT_LIMITS: Limits = Limits {
    max_bytes: 16 * 1024,
    timeout: Duration::from_secs(5),
};

fn invalid_signature() -> Error {
    Error::Unauthorized {
        code: "invalid_signature",
    }
}

pub struct SnsVerifier {
    topics: Vec<TopicArn>,
    http: SafeClient,
    certs: Mutex<HashMap<String, CertificateDer<'static>>>,
}

impl SnsVerifier {
    pub fn new(topics: Vec<TopicArn>, http: SafeClient) -> Self {
        Self {
            topics,
            http,
            certs: Mutex::default(),
        }
    }

    /// Pre-seeds the certificate cache, so tests verify without the network.
    pub fn with_certificate(self, url: &str, pem: &[u8]) -> Result<Self, Error> {
        let cert = parse_certificate(pem)?;
        self.cache().insert(url.to_owned(), cert);
        Ok(self)
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, CertificateDer<'static>>> {
        self.certs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn verify(&self, env: &SnsEnvelope, now: DateTime<Utc>) -> Result<(), Error> {
        let topic =
            self.topics
                .iter()
                .find(|t| t.arn == env.topic_arn)
                .ok_or(Error::Forbidden {
                    code: "unknown_topic",
                })?;
        let fresh = DateTime::parse_from_rfc3339(&env.timestamp)
            .is_ok_and(|at| (now - at.with_timezone(&Utc)).abs() <= MAX_SKEW);
        if !fresh {
            return Err(Error::Validation {
                code: "stale_event",
                detail: "Timestamp is missing or more than an hour off".into(),
            });
        }
        if env.signature_version != "2" || !is_sns_cert_url(&env.signing_cert_url, &topic.region) {
            return Err(invalid_signature());
        }
        let to_sign = string_to_sign(env)?;
        let signature = base64::engine::general_purpose::STANDARD
            .decode(&env.signature)
            .map_err(|_| invalid_signature())?;
        let cert = self.certificate(&env.signing_cert_url).await?;
        EndEntityCert::try_from(&cert)
            .and_then(|c| {
                c.verify_signature(
                    webpki::aws_lc_rs::RSA_PKCS1_2048_8192_SHA256,
                    to_sign.as_bytes(),
                    &signature,
                )
            })
            .map_err(|_| invalid_signature())
    }

    async fn certificate(&self, url: &str) -> Result<CertificateDer<'static>, Error> {
        if let Some(cert) = self.cache().get(url) {
            return Ok(cert.clone());
        }
        let fetched = self
            .http
            .get(url, CERT_LIMITS)
            .await
            .map_err(|e| Error::Unavailable(format!("SNS signing certificate: {e}")))?;
        let cert = parse_certificate(&fetched.bytes)?;
        self.cache().insert(url.to_owned(), cert.clone());
        Ok(cert)
    }
}

/// `https://sns.<region>.amazonaws.com/SimpleNotificationService-<alphanumeric>.pem`, nothing else.
fn is_sns_cert_url(raw: &str, region: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let name = url
        .path()
        .strip_prefix("/SimpleNotificationService-")
        .and_then(|p| p.strip_suffix(".pem"));
    url.scheme() == "https"
        && url.host_str() == Some(&format!("sns.{region}.amazonaws.com"))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && name.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The AWS-documented canonical string: `Name\nValue\n` per signed field of the message type.
fn string_to_sign(env: &SnsEnvelope) -> Result<String, Error> {
    let fields: Vec<(&str, Option<&str>)> = match env.kind.as_str() {
        "Notification" => {
            let mut f = vec![
                ("Message", Some(env.message.as_str())),
                ("MessageId", Some(env.message_id.as_str())),
            ];
            if let Some(subject) = &env.subject {
                f.push(("Subject", Some(subject.as_str())));
            }
            f.extend([
                ("Timestamp", Some(env.timestamp.as_str())),
                ("TopicArn", Some(env.topic_arn.as_str())),
                ("Type", Some(env.kind.as_str())),
            ]);
            f
        }
        "SubscriptionConfirmation" | "UnsubscribeConfirmation" => vec![
            ("Message", Some(env.message.as_str())),
            ("MessageId", Some(env.message_id.as_str())),
            ("SubscribeURL", env.subscribe_url.as_deref()),
            ("Timestamp", Some(env.timestamp.as_str())),
            ("Token", env.token.as_deref()),
            ("TopicArn", Some(env.topic_arn.as_str())),
            ("Type", Some(env.kind.as_str())),
        ],
        _ => {
            return Err(Error::Validation {
                code: "invalid_event",
                detail: "unknown SNS message type".into(),
            });
        }
    };
    fields
        .into_iter()
        .try_fold(String::new(), |out, (name, value)| {
            let value = value.ok_or_else(invalid_signature)?;
            Ok(out + name + "\n" + value + "\n")
        })
}

/// Whether this notification was already applied (a replay or an SNS redelivery).
pub async fn already_processed(db: &PgPool, env: &SnsEnvelope) -> Result<bool, Error> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM platform.sns_messages
                          WHERE topic_arn = $1 AND message_id = $2) AS "seen!""#,
        env.topic_arn,
        env.message_id
    )
    .fetch_one(db)
    .await?)
}

/// Records an applied notification; only call it after the apply succeeded, so a failed apply
/// stays retryable.
pub async fn mark_processed(db: &PgPool, env: &SnsEnvelope) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO platform.sns_messages (topic_arn, message_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
        env.topic_arn,
        env.message_id
    )
    .execute(db)
    .await?;
    Ok(())
}

fn parse_certificate(pem: &[u8]) -> Result<CertificateDer<'static>, Error> {
    let cert = CertificateDer::from_pem_slice(pem).map_err(|_| invalid_signature())?;
    EndEntityCert::try_from(&cert).map_err(|_| invalid_signature())?;
    Ok(cert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use testkit::sns::{CERT_PEM, CERT_URL, TOPIC, sign};

    fn now() -> DateTime<Utc> {
        "2026-10-22T12:00:00Z".parse().unwrap()
    }

    fn verifier() -> SnsVerifier {
        SnsVerifier::new(
            vec![TopicArn::parse(TOPIC).unwrap()],
            SafeClient::new(Vec::<String>::new()).unwrap(),
        )
        .with_certificate(CERT_URL, CERT_PEM)
        .unwrap()
    }

    fn notification() -> Value {
        json!({
            "Type": "Notification",
            "MessageId": "22b80b92-fdea-4c2c-8f9d-bdfb0c7bf324",
            "TopicArn": TOPIC,
            "Message": "{\"notificationType\":\"Bounce\"}",
            "Timestamp": "2026-10-22T11:59:30.123Z",
        })
    }

    fn subscription() -> Value {
        json!({
            "Type": "SubscriptionConfirmation",
            "MessageId": "165545c9-2a5c-472c-8df2-7ff2be2b3b1b",
            "Token": "2336412f37fb687f5d51e6e241d09c805a5a57b30d712f794cc5f6a988666d92768dd60a747ba6f3beb71854e285d6ad02428b09ceece29417f1f02d609c582afbacc99c583a916b9981dd2728f4ae6fdb82efd087cc3b7849e05798d2d2785c03b0879594eeac82c01f235d0e717736",
            "TopicArn": TOPIC,
            "Message": "You have chosen to subscribe to the topic.",
            "SubscribeURL": "https://sns.eu-central-1.amazonaws.com/?Action=ConfirmSubscription&TopicArn=x&Token=y",
            "Timestamp": "2026-10-22T11:59:30.123Z",
        })
    }

    fn envelope(v: Value) -> SnsEnvelope {
        serde_json::from_value(v).unwrap()
    }

    async fn code(v: Value) -> &'static str {
        verifier()
            .verify(&envelope(v), now())
            .await
            .err()
            .map_or("ok", |e| e.code())
    }

    #[tokio::test]
    async fn signed_messages_verify() {
        assert_eq!(code(sign(notification())).await, "ok");
        let mut with_subject = notification();
        with_subject["Subject"] = "Amazon SES Email Event Notification".into();
        assert_eq!(code(sign(with_subject)).await, "ok");
        assert_eq!(code(sign(subscription())).await, "ok");
        let mut unsubscribe = subscription();
        unsubscribe["Type"] = "UnsubscribeConfirmation".into();
        assert_eq!(code(sign(unsubscribe)).await, "ok");
    }

    #[tokio::test]
    async fn tampered_or_unsigned_messages_fail() {
        let mut tampered = sign(notification());
        tampered["Message"] = "{\"notificationType\":\"Complaint\"}".into();
        assert_eq!(code(tampered).await, "invalid_signature");
        let mut subject_added = sign(notification());
        subject_added["Subject"] = "added".into();
        assert_eq!(code(subject_added).await, "invalid_signature");
        let mut token_dropped = sign(subscription());
        token_dropped.as_object_mut().unwrap().remove("Token");
        assert_eq!(code(token_dropped).await, "invalid_signature");
        let mut v1 = sign(notification());
        v1["SignatureVersion"] = "1".into();
        assert_eq!(code(v1).await, "invalid_signature");
        let mut garbage = sign(notification());
        garbage["Signature"] = "not base64!".into();
        assert_eq!(code(garbage).await, "invalid_signature");
        assert_eq!(code(notification()).await, "invalid_signature");
    }

    #[tokio::test]
    async fn topic_and_freshness_are_checked() {
        let mut other = notification();
        other["TopicArn"] = "arn:aws:sns:eu-central-1:123456789012:someone-else".into();
        assert_eq!(code(sign(other)).await, "unknown_topic");
        for at in ["2026-10-22T10:59:59Z", "2026-10-22T13:00:01Z", "yesterday"] {
            let mut stale = notification();
            stale["Timestamp"] = at.into();
            assert_eq!(code(sign(stale)).await, "stale_event", "{at}");
        }
        let mut unknown = notification();
        unknown["Type"] = "Mystery".into();
        assert_eq!(code(sign(unknown)).await, "invalid_event");
    }

    #[tokio::test]
    async fn certificate_urls_outside_the_topic_region_are_refused_before_any_fetch() {
        for url in [
            "http://sns.eu-central-1.amazonaws.com/SimpleNotificationService-abc.pem",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-abc.pem",
            "https://sns.eu-central-1.amazonaws.com.evil.com/SimpleNotificationService-abc.pem",
            "https://evil.com/SimpleNotificationService-abc.pem",
            "https://x@sns.eu-central-1.amazonaws.com/SimpleNotificationService-abc.pem",
            "https://sns.eu-central-1.amazonaws.com:8443/SimpleNotificationService-abc.pem",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-abc.pem?x=1",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-abc.pem#x",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-abc.crt",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-.pem",
            "https://sns.eu-central-1.amazonaws.com/x/SimpleNotificationService-abc.pem",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-a/b.pem",
            "not a url",
        ] {
            let mut v = sign(notification());
            v["SigningCertURL"] = url.into();
            assert_eq!(code(v).await, "invalid_signature", "{url}");
        }
    }

    #[test]
    fn topic_arns_parse_with_their_region() {
        let t = TopicArn::parse(TOPIC).unwrap();
        assert_eq!((t.as_str(), t.region.as_str()), (TOPIC, "eu-central-1"));
        for bad in [
            "",
            "arn:aws:sqs:eu-central-1:123456789012:q",
            "arn:aws:sns:eu-central-1:1234:ses",
            "arn:aws:sns::123456789012:ses",
            "arn:aws:sns:eu-central-1.evil.com:123456789012:ses",
            "arn:aws:sns:eu-central-1:123456789012:",
            "arn:aws:sns:eu-central-1:123456789012:ses:extra",
        ] {
            assert!(TopicArn::parse(bad).is_none(), "{bad}");
        }
    }
}
