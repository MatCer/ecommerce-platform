//! Consent (spec A20, §7.3, §14). `consent_records` is append-only evidence: every choice is
//! a new row, and the latest row per (subject, purpose) is the current state. Anything that
//! depends on consent (forwarding events, sending marketing, personalization) asks
//! [`current`] at execution time; purposes claimed by a client (e.g. in a beacon) are never
//! trusted.
//!
//! Subjects: `anon` (a random id in the first-party consent cookie, set only after a choice),
//! `customer` (after sign-in the anonymous choices are copied over, [`link_anonymous`]) and
//! `email` (consents given with an address but without an account).

use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::markets::invalid;

/// Version of the platform's consent texts (banner, preferences page). Change it when the
/// wording changes: stored records keep the version the person agreed to.
pub const TEXT_VERSION: &str = "2026-09-25";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConsentPurpose {
    Analytics,
    Ads,
    Personalization,
    EmailMarketing,
    ReviewInvites,
}

impl ConsentPurpose {
    pub const ALL: [Self; 5] = [
        Self::Analytics,
        Self::Ads,
        Self::Personalization,
        Self::EmailMarketing,
        Self::ReviewInvites,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Analytics => "analytics",
            Self::Ads => "ads",
            Self::Personalization => "personalization",
            Self::EmailMarketing => "email_marketing",
            Self::ReviewInvites => "review_invites",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// 32 lowercase hex characters (128 random bits).
    Anon(String),
    Customer(Uuid),
    /// A normalized address.
    Email(String),
}

impl Subject {
    fn parts(&self) -> (&'static str, String) {
        match self {
            Self::Anon(id) => ("anon", id.clone()),
            Self::Customer(id) => ("customer", id.to_string()),
            Self::Email(e) => ("email", e.clone()),
        }
    }
}

/// A new anonymous subject id.
pub fn new_anon_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// Shape check for an anonymous subject id from a cookie.
pub fn well_formed_anon(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Per-purpose choices: `None` = not asked (or not part of this choice).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Purposes {
    #[serde(default)]
    pub analytics: Option<bool>,
    #[serde(default)]
    pub ads: Option<bool>,
    #[serde(default)]
    pub personalization: Option<bool>,
    #[serde(default)]
    pub email_marketing: Option<bool>,
    #[serde(default)]
    pub review_invites: Option<bool>,
}

impl Purposes {
    pub fn get(&self, p: ConsentPurpose) -> Option<bool> {
        match p {
            ConsentPurpose::Analytics => self.analytics,
            ConsentPurpose::Ads => self.ads,
            ConsentPurpose::Personalization => self.personalization,
            ConsentPurpose::EmailMarketing => self.email_marketing,
            ConsentPurpose::ReviewInvites => self.review_invites,
        }
    }

    fn set(&mut self, p: ConsentPurpose, v: Option<bool>) {
        let slot = match p {
            ConsentPurpose::Analytics => &mut self.analytics,
            ConsentPurpose::Ads => &mut self.ads,
            ConsentPurpose::Personalization => &mut self.personalization,
            ConsentPurpose::EmailMarketing => &mut self.email_marketing,
            ConsentPurpose::ReviewInvites => &mut self.review_invites,
        };
        *slot = v;
    }

    fn choices(&self) -> Vec<(ConsentPurpose, bool)> {
        ConsentPurpose::ALL
            .into_iter()
            .filter_map(|p| self.get(p).map(|g| (p, g)))
            .collect()
    }
}

/// Where a choice was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The theme's consent banner (shop origin).
    #[default]
    Banner,
    /// The preferences page (checkout origin).
    Preferences,
    /// Checkout checkboxes (WP10).
    Checkout,
    /// Copied from the anonymous subject at sign-in.
    Linked,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Banner => "banner",
            Self::Preferences => "preferences",
            Self::Checkout => "checkout",
            Self::Linked => "linked",
        }
    }
}

/// A choice as posted to `/_p/consent` (the contract in `docs/decisions/consent-contract.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsentChoice {
    pub purposes: Purposes,
    /// The consent text version the person saw (`GET /shop` → `consent.text_version`).
    pub text_version: String,
    /// `banner` (default) or `preferences`.
    #[serde(default)]
    pub source: Source,
}

/// Current consent of a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConsentState {
    pub purposes: Purposes,
    /// Text version of the latest choice; `None` = no choice yet (show the banner).
    pub text_version: Option<String>,
}

fn valid_text_version(v: &str) -> bool {
    (1..=32).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Records a choice (only the purposes present in it). Clients may not claim `linked`.
pub async fn record(
    tx: &mut TenantTx,
    subject: &Subject,
    choice: &ConsentChoice,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    if !valid_text_version(&choice.text_version) {
        return Err(invalid(
            "invalid_text_version",
            "text_version must be 1-32 of [A-Za-z0-9_-]",
        ));
    }
    if choice.source == Source::Linked {
        return Err(invalid(
            "invalid_source",
            "source must be banner or preferences",
        ));
    }
    let choices = choice.purposes.choices();
    if choices.is_empty() {
        return Err(invalid("no_purposes", "choose at least one purpose"));
    }
    insert(
        tx,
        subject,
        &choices,
        &choice.text_version,
        choice.source,
        ip_hash,
    )
    .await
}

async fn insert(
    tx: &mut TenantTx,
    subject: &Subject,
    choices: &[(ConsentPurpose, bool)],
    text_version: &str,
    source: Source,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    let (kind, id) = subject.parts();
    let purposes: Vec<&str> = choices.iter().map(|(p, _)| p.as_str()).collect();
    let granted: Vec<bool> = choices.iter().map(|(_, g)| *g).collect();
    sqlx::query!(
        "INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose, granted,
                                      text_version, source, ip_hash)
         SELECT $1, $2, $3, p, g, $6, $7, $8 FROM unnest($4::text[], $5::bool[]) AS c(p, g)",
        tx.tenant_id(),
        kind,
        id,
        &purposes as &[&str],
        &granted,
        text_version,
        source.as_str(),
        ip_hash
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The subject's current state: the latest record per purpose.
pub async fn state(tx: &mut TenantTx, subject: &Subject) -> Result<ConsentState, Error> {
    let (kind, id) = subject.parts();
    let rows = sqlx::query!(
        "SELECT DISTINCT ON (purpose) purpose, granted, text_version, at
         FROM consent_records WHERE subject_type = $1 AND subject_id = $2
         ORDER BY purpose, at DESC, id DESC",
        kind,
        id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut purposes = Purposes::default();
    let mut latest: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
    for r in rows {
        if let Some(p) = ConsentPurpose::ALL
            .into_iter()
            .find(|p| p.as_str() == r.purpose)
        {
            purposes.set(p, Some(r.granted));
        }
        if latest.as_ref().is_none_or(|(at, _)| r.at > *at) {
            latest = Some((r.at, r.text_version));
        }
    }
    Ok(ConsentState {
        purposes,
        text_version: latest.map(|(_, v)| v),
    })
}

/// Whether `subject` currently grants `purpose` (A20). No record means no consent.
pub async fn current(
    tx: &mut TenantTx,
    subject: &Subject,
    purpose: ConsentPurpose,
) -> Result<bool, Error> {
    let (kind, id) = subject.parts();
    Ok(sqlx::query_scalar!(
        "SELECT granted FROM consent_records
         WHERE subject_type = $1 AND subject_id = $2 AND purpose = $3
         ORDER BY at DESC, id DESC LIMIT 1",
        kind,
        id,
        purpose.as_str()
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or(false))
}

/// At sign-in: the anonymous subject's choices that are newer than the customer's own become
/// the customer's (source `linked`), so the latest decision of the person wins.
pub async fn link_anonymous(tx: &mut TenantTx, anon: &str, customer: Uuid) -> Result<(), Error> {
    if !well_formed_anon(anon) {
        return Ok(());
    }
    sqlx::query!(
        "INSERT INTO consent_records (tenant_id, subject_type, subject_id, purpose, granted,
                                      text_version, source, ip_hash)
         SELECT a.tenant_id, 'customer', $2, a.purpose, a.granted, a.text_version, 'linked', NULL
         FROM (SELECT DISTINCT ON (purpose) tenant_id, purpose, granted, text_version, at
               FROM consent_records WHERE subject_type = 'anon' AND subject_id = $1
               ORDER BY purpose, at DESC, id DESC) a
         WHERE NOT EXISTS (
             SELECT 1 FROM consent_records c
             WHERE c.subject_type = 'customer' AND c.subject_id = $2 AND c.purpose = a.purpose
               AND c.at >= a.at)",
        anon,
        customer.to_string()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The compact, script-readable summary the edge stores in the `consent` cookie so a theme can
/// tell whether to show the banner: `<text_version>.<mask>`, one mask character per purpose in
/// [`ConsentPurpose::ALL`] order: `1` granted, `0` refused, `-` not asked.
pub fn summary(state: &ConsentState) -> Option<String> {
    let version = state.text_version.as_ref()?;
    let mask: String = ConsentPurpose::ALL
        .into_iter()
        .map(|p| match state.purposes.get(p) {
            Some(true) => '1',
            Some(false) => '0',
            None => '-',
        })
        .collect();
    Some(format!("{version}.{mask}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anon_ids_are_128_bit_hex() {
        let a = new_anon_id();
        assert!(well_formed_anon(&a));
        assert_ne!(a, new_anon_id());
        assert!(!well_formed_anon(&a.to_uppercase()));
        assert!(!well_formed_anon("short"));
    }

    #[test]
    fn choice_json_is_strict() {
        let ok: ConsentChoice = serde_json::from_str(
            r#"{"purposes":{"analytics":true,"ads":false},"text_version":"2026-09-25"}"#,
        )
        .unwrap();
        assert_eq!(ok.purposes.analytics, Some(true));
        assert_eq!(ok.purposes.personalization, None);
        assert_eq!(ok.source, Source::Banner);
        assert!(
            serde_json::from_str::<ConsentChoice>(
                r#"{"purposes":{"tracking":true},"text_version":"v1"}"#
            )
            .is_err()
        );
        assert!(valid_text_version("2026-09-25"));
        assert!(!valid_text_version("v1;drop"));
        assert!(!valid_text_version(""));
    }

    #[test]
    fn summary_encodes_the_state_for_the_banner() {
        let mut s = ConsentState {
            purposes: Purposes {
                analytics: Some(true),
                ads: Some(false),
                ..Purposes::default()
            },
            text_version: Some("2026-09-25".into()),
        };
        assert_eq!(summary(&s).as_deref(), Some("2026-09-25.10---"));
        s.text_version = None;
        assert_eq!(summary(&s), None);
    }
}
