//! Newsletter subscriber rows (spec §11.5, A20). Consent evidence is `consent_at` +
//! `consent_source` (plus `consent_ip` and `consent_text_version` where the old shop kept
//! them). A row with evidence records an `email_marketing` grant at that time (source
//! `import`) and subscribes the address only when that grant is the address's latest decision
//! and the address is not suppressed. A row without evidence becomes a `pending` subscriber
//! that was never sent a confirmation: listed, never marketable. Addresses that unsubscribed,
//! bounced or complained stay as they are. Nothing is ever emailed.

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use serde_json::json;
use uuid::Uuid;

use super::imports::{Check, DataImportReport, Defaults};
use super::table::{self, Field, Row, field};
use crate::consent::{self, ConsentPurpose, Subject};

pub const FIELDS: &[Field] = &[
    field("email", true),
    field("locale", false),
    field("consent_at", false),
    field("consent_source", false),
    field("consent_ip", false),
    field("consent_text_version", false),
];

/// The text version recorded when the file does not name one.
pub const IMPORTED_TEXT_VERSION: &str = "imported";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub at: DateTime<Utc>,
    pub source: String,
    pub ip: Option<IpAddr>,
    pub text_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub email: String,
    pub locale: Option<String>,
    pub evidence: Option<Evidence>,
}

impl Record {
    pub fn preview(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::from([("email".to_owned(), self.email.clone())]);
        if let Some(l) = &self.locale {
            m.insert("locale".into(), l.clone());
        }
        match &self.evidence {
            Some(e) => {
                m.insert("consent_at".into(), e.at.to_rfc3339());
                m.insert("consent_source".into(), e.source.clone());
            }
            None => {
                m.insert("consent_at".into(), String::new());
            }
        }
        m
    }
}

fn text_version(v: &str) -> table::CellResult<String> {
    if consent::valid_text_version(v) {
        Ok(v.to_owned())
    } else {
        Err((
            "invalid_text_version",
            format!("{v:?} must be 1-32 of A-Z a-z 0-9 _ -"),
        ))
    }
}

pub fn validate(rows: &[Row], d: &Defaults, report: &mut DataImportReport) -> Vec<Record> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let mut c = Check::new(row, report);
        let email = c.req("email", table::email);
        let locale = c.opt("locale", table::locale);
        let any = [
            "consent_at",
            "consent_source",
            "consent_ip",
            "consent_text_version",
        ]
        .iter()
        .any(|f| c.has(f));
        let evidence = if any {
            let at = c.req("consent_at", |v| table::past_time(v, d.now));
            let source = c.req("consent_source", |v| table::text(v, 200));
            let ip = c.opt("consent_ip", table::ip);
            let text_version = c.opt("consent_text_version", text_version);
            at.zip(source).map(|(at, source)| Evidence {
                at,
                source,
                ip,
                text_version,
            })
        } else {
            None
        };
        if let Some(e) = &email
            && !seen.insert(e.clone())
        {
            c.fail(
                Some("email"),
                "duplicate",
                format!("{e} appears on an earlier line"),
            );
        }
        if !c.ok {
            report.invalid_rows += 1;
            continue;
        }
        if let Some(email) = email {
            out.push(Record {
                email,
                locale,
                evidence,
            });
        }
    }
    out
}

/// What applying a record does (the dry run predicts it, apply reports it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Subscribed with the imported evidence.
    Subscribed,
    /// Listed as `pending` (no evidence, or a newer withdrawal, or suppressed): not marketable.
    Pending,
    /// Already subscribed; evidence added when given.
    AlreadySubscribed,
    /// Unsubscribed, bounced or complained: left alone.
    Kept,
}

impl Outcome {
    pub fn key(self) -> &'static str {
        match self {
            Self::Subscribed => "subscribed",
            Self::Pending => "pending_not_marketable",
            Self::AlreadySubscribed => "already_subscribed",
            Self::Kept => "kept_unsubscribed",
        }
    }
}

pub async fn classify(
    tx: &mut TenantTx,
    records: &[Record],
    report: &mut DataImportReport,
) -> Result<(), Error> {
    let emails: Vec<String> = records.iter().map(|r| r.email.clone()).collect();
    let rows = sqlx::query!(
        "SELECT email, status FROM subscribers WHERE email = ANY($1)",
        &emails
    )
    .fetch_all(&mut **tx)
    .await?;
    let status: BTreeMap<String, String> = rows.into_iter().map(|r| (r.email, r.status)).collect();
    report.existing = u32::try_from(status.len()).unwrap_or(u32::MAX);
    report.new = report.records.saturating_sub(report.existing);
    for r in records {
        let predicted = match status.get(&r.email).map(String::as_str) {
            Some("subscribed") => Outcome::AlreadySubscribed,
            Some("unsubscribed" | "bounced" | "complained") => Outcome::Kept,
            _ if r.evidence.is_none() => Outcome::Pending,
            _ if crate::notifications::is_suppressed(tx, &r.email, Stream::Marketing).await? => {
                Outcome::Pending
            }
            // A newer withdrawal of the address would still win; apply reports the result.
            _ => Outcome::Subscribed,
        };
        report.count(predicted.key(), 1);
    }
    Ok(())
}

pub async fn apply(
    tx: &mut TenantTx,
    r: &Record,
    d: &Defaults,
    import_id: Uuid,
) -> Result<(bool, Outcome), Error> {
    let evidence = r.evidence.as_ref().map(|e| {
        json!({
            "source": e.source,
            "at": e.at,
            "ip": e.ip.map(|ip| ip.to_string()),
            "text_version": e.text_version,
            "import_id": import_id,
        })
    });
    let tv = r
        .evidence
        .as_ref()
        .and_then(|e| e.text_version.clone())
        .unwrap_or_else(|| IMPORTED_TEXT_VERSION.to_owned());
    let created = sqlx::query_scalar!(
        "INSERT INTO subscribers (tenant_id, email, status, locale, market_id, requested_at,
                                  text_version, source, consent_evidence)
         VALUES ($1, $2, 'pending', coalesce($3, $4), $5, coalesce($6, now()), $7, 'import', $8)
         ON CONFLICT ON CONSTRAINT subscribers_email_unique DO NOTHING
         RETURNING id",
        tx.tenant_id(),
        r.email,
        r.locale,
        d.locale,
        d.market_id,
        r.evidence.as_ref().map(|e| e.at),
        tv,
        evidence
    )
    .fetch_optional(&mut **tx)
    .await?
    .is_some();
    let s = sqlx::query!(
        "SELECT id, status, customer_id FROM subscribers WHERE email = $1 FOR UPDATE",
        r.email
    )
    .fetch_one(&mut **tx)
    .await?;
    match s.status.as_str() {
        "unsubscribed" | "bounced" | "complained" => return Ok((created, Outcome::Kept)),
        "subscribed" => {
            if let Some(e) = &r.evidence {
                consent::record_imported_grant(
                    tx,
                    &Subject::Email(r.email.clone()),
                    ConsentPurpose::EmailMarketing,
                    &tv,
                    e.at,
                )
                .await?;
                sqlx::query!(
                    "UPDATE subscribers SET consent_evidence = coalesce(consent_evidence, $2)
                     WHERE id = $1",
                    s.id,
                    evidence
                )
                .execute(&mut **tx)
                .await?;
            }
            return Ok((created, Outcome::AlreadySubscribed));
        }
        _ => {}
    }
    let Some(e) = &r.evidence else {
        return Ok((created, Outcome::Pending));
    };
    let subject = Subject::Email(r.email.clone());
    consent::record_imported_grant(tx, &subject, ConsentPurpose::EmailMarketing, &tv, e.at).await?;
    // The person's decisions as a customer count too, linked or not: an account that
    // withdrew after the imported consent keeps the address unmarketable.
    let account = sqlx::query_scalar!("SELECT id FROM customers WHERE email = $1", r.email)
        .fetch_optional(&mut **tx)
        .await?;
    let mut subjects = vec![subject];
    subjects.extend(
        s.customer_id
            .into_iter()
            .chain(account)
            .map(Subject::Customer),
    );
    let granted = consent::latest_any(tx, &subjects, ConsentPurpose::EmailMarketing)
        .await?
        .unwrap_or(false);
    if !granted || crate::notifications::is_suppressed(tx, &r.email, Stream::Marketing).await? {
        return Ok((created, Outcome::Pending));
    }
    // A pending double opt-in of the address is superseded by the imported consent.
    sqlx::query!(
        "UPDATE subscribers SET status = 'subscribed', confirmed_at = $2, text_version = $3,
             consent_evidence = $4, confirm_token_hash = NULL, confirm_expires_at = NULL,
             unsubscribed_at = NULL, updated_at = now()
         WHERE id = $1",
        s.id,
        e.at,
        tv,
        evidence
    )
    .execute(&mut **tx)
    .await?;
    Ok((created, Outcome::Subscribed))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn evidence_needs_a_time_and_a_source() {
        let rows = [
            Row::of(2, &[("email", "a@example.com")]),
            Row::of(
                3,
                &[
                    ("email", "b@example.com"),
                    ("consent_at", "2023-05-01"),
                    ("consent_source", "checkout box"),
                    ("consent_ip", "192.0.2.7"),
                    ("consent_text_version", "v3"),
                ],
            ),
            Row::of(
                4,
                &[("email", "c@example.com"), ("consent_at", "2023-05-01")],
            ),
            Row::of(
                5,
                &[("email", "d@example.com"), ("consent_ip", "192.0.2.7")],
            ),
            Row::of(
                6,
                &[
                    ("email", "e@example.com"),
                    ("consent_at", "2023-05-01"),
                    ("consent_source", "form"),
                    ("consent_text_version", "v 3"),
                ],
            ),
            Row::of(7, &[("email", "A@example.com")]),
        ];
        let mut report = DataImportReport::default();
        let out = validate(&rows, &Defaults::test(), &mut report);
        assert_eq!(out.len(), 2);
        assert!(out[0].evidence.is_none());
        let e = out[1].evidence.as_ref().unwrap();
        assert_eq!(e.source, "checkout box");
        assert_eq!(e.ip.unwrap().to_string(), "192.0.2.7");
        let codes: Vec<(u64, &str)> = report
            .errors
            .iter()
            .map(|e| (e.line, e.code.as_str()))
            .collect();
        assert_eq!(
            codes,
            [
                (4, "missing"),
                (5, "missing"),
                (5, "missing"),
                (6, "invalid_text_version"),
                (7, "duplicate")
            ]
        );
    }
}
