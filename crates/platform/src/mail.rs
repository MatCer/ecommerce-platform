//! Outgoing email (spec §11.4, A14): one SMTP transport per stream (`transactional`,
//! `marketing`, separate credentials and from-addresses; both go to Mailpit locally) and MJML
//! rendering. What to send, suppression and the send states live in `commerce::notifications`.
//!
//! [`Mailer::send`] reports what is known about the outcome, which decides the message state:
//! - [`Delivery::Accepted`]: the server answered 250 to the message data;
//! - [`Delivery::Rejected`]: a permanent 5xx (or an unusable address): never retry;
//! - [`Delivery::NotSent`]: nothing was handed over (no connection, a 4xx): retry safely;
//! - [`Delivery::Uncertain`]: the connection broke while sending; the server may or may not
//!   have the message, so a retry may duplicate it.

use std::time::Duration;

use lettre::message::{Mailbox, MultiPart, header};
use lettre::transport::smtp::Error as SmtpError;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{ConfigError, Lookup};

const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Transactional,
    Marketing,
}

impl Stream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transactional => "transactional",
            Self::Marketing => "marketing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "transactional" => Some(Self::Transactional),
            "marketing" => Some(Self::Marketing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// `smtp://host:port` (plain, local), `smtp://user:pass@host:587?tls=required`, `smtps://…`.
    pub url: String,
    /// Envelope and header sender, e.g. `mail@shops.example`.
    pub from: String,
}

#[derive(Debug, Clone)]
pub struct MailConfig {
    pub transactional: StreamConfig,
    pub marketing: StreamConfig,
}

impl MailConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|k| std::env::var(k).ok())
    }

    /// `MAIL_TRANSACTIONAL_SMTP_URL`, `MAIL_TRANSACTIONAL_FROM`, `MAIL_MARKETING_SMTP_URL`,
    /// `MAIL_MARKETING_FROM`.
    pub fn from_lookup(lookup: Lookup) -> Result<Self, ConfigError> {
        let stream = |url: &'static str, from: &'static str| -> Result<StreamConfig, ConfigError> {
            let get = |name: &'static str| {
                lookup(name)
                    .filter(|v| !v.trim().is_empty())
                    .ok_or(ConfigError::Missing(name))
            };
            let cfg = StreamConfig {
                url: get(url)?,
                from: get(from)?,
            };
            AsyncSmtpTransport::<Tokio1Executor>::from_url(&cfg.url).map_err(|e| {
                ConfigError::Invalid {
                    name: url,
                    reason: e.to_string(),
                }
            })?;
            cfg.from
                .parse::<Mailbox>()
                .map_err(|e| ConfigError::Invalid {
                    name: from,
                    reason: e.to_string(),
                })?;
            Ok(cfg)
        };
        Ok(Self {
            transactional: stream("MAIL_TRANSACTIONAL_SMTP_URL", "MAIL_TRANSACTIONAL_FROM")?,
            marketing: stream("MAIL_MARKETING_SMTP_URL", "MAIL_MARKETING_FROM")?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid mail configuration: {0}")]
    Config(String),
    #[error("MJML rendering failed: {0}")]
    Render(String),
}

/// One message, already rendered.
#[derive(Debug, Clone)]
pub struct Outgoing<'a> {
    pub stream: Stream,
    /// Display name for the stream's from-address (the shop's name).
    pub from_name: &'a str,
    pub to: &'a str,
    pub subject: &'a str,
    pub html: &'a str,
    pub text: &'a str,
    /// Stable per message: the Message-ID becomes `<id@from-domain>`, so a duplicate after an
    /// uncertain send can be recognised by the receiver (spec §13).
    pub id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Accepted,
    Rejected(String),
    NotSent(String),
    Uncertain(String),
}

#[derive(Clone)]
struct Transport {
    smtp: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Transport {
    fn new(cfg: &StreamConfig) -> Result<Self, MailError> {
        Ok(Self {
            smtp: AsyncSmtpTransport::<Tokio1Executor>::from_url(&cfg.url)
                .map_err(|e| MailError::Config(e.to_string()))?
                .timeout(Some(SMTP_TIMEOUT))
                .build(),
            from: cfg
                .from
                .parse()
                .map_err(|e: lettre::address::AddressError| MailError::Config(e.to_string()))?,
        })
    }
}

#[derive(Clone)]
pub struct Mailer {
    transactional: Transport,
    marketing: Transport,
}

impl Mailer {
    pub fn new(cfg: &MailConfig) -> Result<Self, MailError> {
        Ok(Self {
            transactional: Transport::new(&cfg.transactional)?,
            marketing: Transport::new(&cfg.marketing)?,
        })
    }

    pub async fn send(&self, msg: &Outgoing<'_>) -> Delivery {
        let t = match msg.stream {
            Stream::Transactional => &self.transactional,
            Stream::Marketing => &self.marketing,
        };
        let email = match build(t, msg) {
            Ok(m) => m,
            Err(e) => return Delivery::Rejected(e),
        };
        // Opening a connection first separates "the server is unreachable" (nothing was sent,
        // retry freely) from a failure while the message was on the wire (uncertain). A
        // server that dies between the two is reported as uncertain, never as not sent.
        if let Err(e) = t.smtp.test_connection().await {
            return classify(&e, false);
        }
        match t.smtp.send(email).await {
            Ok(_) => Delivery::Accepted,
            Err(e) => classify(&e, true),
        }
    }
}

fn build(t: &Transport, msg: &Outgoing<'_>) -> Result<Message, String> {
    let to: Mailbox = msg
        .to
        .parse()
        .map_err(|e| format!("invalid recipient: {e}"))?;
    let from = Mailbox::new(
        Some(msg.from_name.chars().filter(|c| !c.is_control()).collect()),
        t.from.email.clone(),
    );
    Message::builder()
        .from(from)
        .to(to)
        .subject(msg.subject)
        .message_id(Some(format!("<{}@{}>", msg.id, t.from.email.domain())))
        .header(header::MIME_VERSION_1_0)
        .multipart(MultiPart::alternative_plain_html(
            msg.text.to_owned(),
            msg.html.to_owned(),
        ))
        .map_err(|e| format!("invalid message: {e}"))
}

fn classify(e: &SmtpError, sending: bool) -> Delivery {
    let text = e.to_string();
    if e.is_permanent() {
        Delivery::Rejected(text)
    } else if e.is_transient() || !sending {
        Delivery::NotSent(text)
    } else {
        Delivery::Uncertain(text)
    }
}

/// MJML → HTML (`mrml`). The input is platform-owned layout + escaped variables.
pub fn render_mjml(mjml: &str) -> Result<String, MailError> {
    let parsed = mrml::parse(mjml).map_err(|e| MailError::Render(e.to_string()))?;
    parsed
        .element
        .render(&mrml::prelude::render::RenderOptions::default())
        .map_err(|e| MailError::Render(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn config_needs_both_streams_and_valid_values() {
        let ok = [
            ("MAIL_TRANSACTIONAL_SMTP_URL", "smtp://mailpit:1025"),
            ("MAIL_TRANSACTIONAL_FROM", "shop@mail.example"),
            ("MAIL_MARKETING_SMTP_URL", "smtp://mailpit:1025"),
            ("MAIL_MARKETING_FROM", "news@mail.example"),
        ];
        assert!(MailConfig::from_lookup(&lookup(&ok)).is_ok());
        assert_eq!(
            MailConfig::from_lookup(&lookup(&ok[..2])).err(),
            Some(ConfigError::Missing("MAIL_MARKETING_SMTP_URL"))
        );
        let mut bad = ok;
        bad[1] = ("MAIL_TRANSACTIONAL_FROM", "not an address");
        assert!(matches!(
            MailConfig::from_lookup(&lookup(&bad)),
            Err(ConfigError::Invalid {
                name: "MAIL_TRANSACTIONAL_FROM",
                ..
            })
        ));
    }

    #[test]
    fn mjml_renders_to_html() {
        let html = render_mjml(
            "<mjml><mj-body><mj-section><mj-column><mj-text>Ahoj &amp; vítejte</mj-text></mj-column></mj-section></mj-body></mjml>",
        )
        .unwrap();
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("Ahoj &amp; vítejte"));
        assert!(render_mjml("<mjml><mj-body><mj-nope>").is_err());
    }
}
