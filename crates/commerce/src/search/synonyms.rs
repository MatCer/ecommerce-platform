//! Tenant synonyms (WP7 follow-up): groups of equivalent words or phrases as the merchant types
//! them ("mikina, hoodie"). Stored raw; the worker normalizes them per locale with the same
//! analyzer as documents and queries ([`super::lang::analyze`]) and writes them to every index
//! of the tenant ([`super::index::apply_synonyms`]). Changes apply asynchronously.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::{SYNONYMS_JOB, lang};
use crate::audit;
use crate::markets::invalid;

pub const MAX_GROUPS: usize = 500;
pub const MAX_TERMS: usize = 20;
const MAX_TERM: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Synonyms {
    /// Each group lists 2-20 equivalent words or phrases.
    #[schema(example = json!([["mikina", "hoodie"], ["tričko", "triko", "tílko"]]))]
    pub groups: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SynonymsView {
    pub groups: Vec<Vec<String>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Synonyms {
    pub fn validate(&self) -> Result<(), Error> {
        const CODE: &str = "invalid_synonyms";
        if self.groups.len() > MAX_GROUPS {
            return Err(invalid(CODE, format!("at most {MAX_GROUPS} groups")));
        }
        for g in &self.groups {
            let distinct: BTreeSet<String> = g.iter().map(|t| t.trim().to_lowercase()).collect();
            if !(2..=MAX_TERMS).contains(&g.len()) || distinct.len() != g.len() {
                return Err(invalid(
                    CODE,
                    format!("a group lists 2-{MAX_TERMS} different terms"),
                ));
            }
            for t in g {
                let n = t.trim().chars().count();
                if n == 0 || n > MAX_TERM || t.chars().any(char::is_control) {
                    return Err(invalid(
                        CODE,
                        format!("terms have 1-{MAX_TERM} printable characters"),
                    ));
                }
            }
        }
        Ok(())
    }
}

pub async fn get(tx: &mut TenantTx) -> Result<SynonymsView, Error> {
    let row = sqlx::query!("SELECT groups, updated_at FROM search_synonyms")
        .fetch_optional(&mut **tx)
        .await?;
    Ok(match row {
        Some(r) => SynonymsView {
            groups: serde_json::from_value(r.groups).unwrap_or_default(),
            updated_at: Some(r.updated_at),
        },
        None => SynonymsView {
            groups: vec![],
            updated_at: None,
        },
    })
}

/// Saves the groups and queues their application to the indexes.
pub async fn put(tx: &mut TenantTx, actor: &str, input: &Synonyms) -> Result<SynonymsView, Error> {
    input.validate()?;
    let groups: Vec<Vec<String>> = input
        .groups
        .iter()
        .map(|g| g.iter().map(|t| t.trim().to_owned()).collect())
        .collect();
    let value = serde_json::to_value(&groups).map_err(|e| Error::Internal(e.to_string()))?;
    let before = get(tx).await?;
    sqlx::query!(
        "INSERT INTO search_synonyms (tenant_id, groups) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET groups = $2, updated_at = now()",
        tx.tenant_id(),
        value
    )
    .execute(&mut **tx)
    .await?;
    let mut job = NewJob::new(SYNONYMS_JOB, json!({}));
    job.tenant_id = Some(tx.tenant_id());
    job.max_attempts = 25;
    queue::enqueue(&mut **tx, &job).await?;
    audit::record(
        tx,
        actor,
        "search.synonyms_updated",
        "search",
        None,
        &json!({ "before": before.groups, "after": groups }),
    )
    .await?;
    get(tx).await
}

/// Meilisearch `synonyms` for `locale`: every normalized term maps to the other terms of its
/// group (mutual synonyms). Terms that normalize to nothing (stopwords) are dropped.
pub fn for_locale(groups: &[Vec<String>], locale: &str) -> Value {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for g in groups {
        let terms: BTreeSet<String> = g
            .iter()
            .map(|t| lang::analyze(t, locale))
            .filter(|t| !t.is_empty())
            .collect();
        for t in &terms {
            map.entry(t.clone())
                .or_default()
                .extend(terms.iter().filter(|o| *o != t).cloned());
        }
    }
    map.retain(|_, v| !v.is_empty());
    json!(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_validated() {
        let ok = Synonyms {
            groups: vec![vec!["mikina".into(), "hoodie".into()]],
        };
        assert!(ok.validate().is_ok());
        for bad in [
            vec![vec!["jen".to_owned()]],
            vec![vec!["a".to_owned(), "A".to_owned()]],
            vec![vec!["a".to_owned(), "x".repeat(51)]],
            vec![vec!["a".to_owned(), " ".to_owned()]],
        ] {
            assert!(Synonyms { groups: bad }.validate().is_err());
        }
    }

    #[test]
    fn synonyms_are_normalized_like_queries() {
        let v = for_locale(&[vec!["Mikina".into(), "hoodie".into()]], "cs");
        // Terms are folded and stemmed like queries; each lists the others.
        let map = v.as_object().cloned().unwrap_or_default();
        let hoodie = lang::analyze("hoodie", "cs");
        let mikina = lang::analyze("mikina", "cs");
        assert_eq!(map[&hoodie], json!([mikina]));
        assert_eq!(map[&mikina], json!([hoodie]));
    }
}
