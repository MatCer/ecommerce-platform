//! Tenant glossary for translations: brand and product-line terms that stay as they are, or
//! get one fixed translation per locale. Given to the model and checked in its output.

use std::collections::{BTreeMap, BTreeSet};

use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::audit;
use crate::markets::is_locale;

pub const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GlossaryEntry {
    /// The term as written in the source text, e.g. `Lnen & Co.`.
    #[schema(example = "Lnen & Co.")]
    pub term: String,
    /// Locale -> the fixed translation; locales not listed keep the term unchanged.
    #[serde(default)]
    pub translations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Glossary {
    pub entries: Vec<GlossaryEntry>,
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::Validation {
        code: "invalid_glossary",
        detail: detail.into(),
    }
}

impl Glossary {
    pub fn validate(&self) -> Result<(), Error> {
        if self.entries.len() > MAX_ENTRIES {
            return Err(invalid(format!("at most {MAX_ENTRIES} terms")));
        }
        let mut seen = BTreeSet::new();
        for e in &self.entries {
            let len = e.term.trim().chars().count();
            if !(1..=100).contains(&len) || e.term.trim() != e.term {
                return Err(invalid(
                    "terms are 1-100 characters without surrounding spaces",
                ));
            }
            if !seen.insert(e.term.to_lowercase()) {
                return Err(invalid(format!("{:?} is listed twice", e.term)));
            }
            for (locale, text) in &e.translations {
                if !is_locale(locale) {
                    return Err(invalid(format!("invalid locale {locale:?}")));
                }
                if !(1..=100).contains(&text.trim().chars().count()) {
                    return Err(invalid("translations are 1-100 characters"));
                }
            }
        }
        Ok(())
    }

    /// `(term, required form)` for the terms occurring in `source`, for `target` locale.
    pub fn applicable(&self, source: &str, target: &str) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|e| source.contains(&e.term))
            .map(|e| {
                let form = e
                    .translations
                    .get(target)
                    .cloned()
                    .unwrap_or_else(|| e.term.clone());
                (e.term.clone(), form)
            })
            .collect()
    }

    /// Terms of `source` whose required form is missing from `output`.
    pub fn violations(&self, source: &str, output: &str, target: &str) -> Vec<String> {
        self.applicable(source, target)
            .into_iter()
            .filter(|(_, form)| !output.contains(form.as_str()))
            .map(|(term, _)| term)
            .collect()
    }
}

pub async fn get(tx: &mut TenantTx) -> Result<Glossary, Error> {
    let entries = sqlx::query_scalar!("SELECT entries FROM ai_glossaries")
        .fetch_optional(&mut **tx)
        .await?;
    match entries {
        None => Ok(Glossary::default()),
        Some(v) => Ok(Glossary {
            entries: serde_json::from_value(v).map_err(|e| Error::Internal(e.to_string()))?,
        }),
    }
}

/// Replaces the glossary. Audited.
pub async fn put(tx: &mut TenantTx, actor: &str, input: &Glossary) -> Result<Glossary, Error> {
    input.validate()?;
    let before = get(tx).await?;
    let entries =
        serde_json::to_value(&input.entries).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "INSERT INTO ai_glossaries (tenant_id, entries) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET entries = EXCLUDED.entries, updated_at = now()",
        tx.tenant_id(),
        entries
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "ai.glossary.updated",
        "ai_glossary",
        None,
        &json!({ "before": before.entries, "after": input.entries }),
    )
    .await?;
    Ok(input.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glossary() -> Glossary {
        Glossary {
            entries: vec![
                GlossaryEntry {
                    term: "Lnen & Co.".into(),
                    translations: BTreeMap::new(),
                },
                GlossaryEntry {
                    term: "Merino".into(),
                    translations: BTreeMap::from([("en".into(), "Merino wool".into())]),
                },
            ],
        }
    }

    #[test]
    fn detects_dropped_or_translated_terms() {
        let g = glossary();
        let src = "Tričko Lnen & Co. z vlny Merino";
        assert!(
            g.violations(src, "T-shirt Lnen & Co. of Merino wool", "en")
                .is_empty()
        );
        assert_eq!(
            g.violations(src, "T-shirt Linen & Co. of merino", "en"),
            vec!["Lnen & Co.".to_owned(), "Merino".to_owned()]
        );
        // Terms absent from the source are not required.
        assert!(g.violations("Tričko", "T-shirt", "en").is_empty());
        assert_eq!(
            g.applicable(src, "sk"),
            vec![
                ("Lnen & Co.".to_owned(), "Lnen & Co.".to_owned()),
                ("Merino".to_owned(), "Merino".to_owned())
            ]
        );
    }

    #[test]
    fn validates_entries() {
        assert!(glossary().validate().is_ok());
        let mut dup = glossary();
        dup.entries.push(GlossaryEntry {
            term: "merino".into(),
            translations: BTreeMap::new(),
        });
        assert_eq!(dup.validate().unwrap_err().code(), "invalid_glossary");
        let mut bad = glossary();
        bad.entries[0].term = " x".into();
        assert!(bad.validate().is_err());
        let mut locale = glossary();
        locale.entries[0]
            .translations
            .insert("english".into(), "x".into());
        assert!(locale.validate().is_err());
    }
}
