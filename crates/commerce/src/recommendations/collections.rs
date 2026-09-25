//! Collections (spec §7.6, §11.2): merchant-curated product lists. A `seasonal` collection
//! has a schedule window and fills the home page's recommendations while it is open; a
//! `manual` one is always available (or within its optional window) wherever a theme asks for
//! it (`context=collection:<id>`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

pub const MAX_PRODUCTS: usize = 200;
const MAX_TITLES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Manual,
    Seasonal,
}

impl CollectionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Seasonal => "seasonal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    /// Shopper-facing heading per locale; the name where missing.
    pub title_i18n: BTreeMap<String, String>,
    pub kind: CollectionKind,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    /// In display order.
    pub product_ids: Vec<Uuid>,
    /// Open now (inside its window, or unscheduled).
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionInput {
    #[schema(example = "Vánoce")]
    pub name: String,
    #[serde(default)]
    pub title_i18n: BTreeMap<String, String>,
    pub kind: CollectionKind,
    /// Required for `seasonal`.
    pub starts_at: Option<DateTime<Utc>>,
    /// Required for `seasonal`; after `starts_at`.
    pub ends_at: Option<DateTime<Utc>>,
    /// Products in display order (at most 200; duplicates are dropped).
    pub product_ids: Vec<Uuid>,
}

fn locale_ok(l: &str) -> bool {
    let b = l.as_bytes();
    match b.len() {
        2 => b.iter().all(u8::is_ascii_lowercase),
        5 => {
            b[..2].iter().all(u8::is_ascii_lowercase)
                && b[2] == b'-'
                && b[3..].iter().all(u8::is_ascii_uppercase)
        }
        _ => false,
    }
}

impl CollectionInput {
    /// Validates and normalizes (trimmed texts, deduplicated products in order).
    pub fn validate(&self) -> Result<Self, Error> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(invalid("invalid_name", "name must be 1-200 characters"));
        }
        if self.title_i18n.len() > MAX_TITLES {
            return Err(invalid("invalid_title", "too many title translations"));
        }
        let mut titles = BTreeMap::new();
        for (locale, text) in &self.title_i18n {
            let text = text.trim();
            if !locale_ok(locale) || text.chars().count() > 200 {
                return Err(invalid(
                    "invalid_title",
                    "titles are keyed by locale (cs, en-GB) and at most 200 characters",
                ));
            }
            if !text.is_empty() {
                titles.insert(locale.clone(), text.to_owned());
            }
        }
        if self.kind == CollectionKind::Seasonal
            && (self.starts_at.is_none() || self.ends_at.is_none())
        {
            return Err(invalid(
                "invalid_schedule",
                "a seasonal collection needs starts_at and ends_at",
            ));
        }
        if let (Some(s), Some(e)) = (self.starts_at, self.ends_at)
            && e <= s
        {
            return Err(invalid(
                "invalid_schedule",
                "ends_at must be after starts_at",
            ));
        }
        let mut products: Vec<Uuid> = Vec::with_capacity(self.product_ids.len());
        for id in &self.product_ids {
            if !products.contains(id) {
                products.push(*id);
            }
        }
        if products.len() > MAX_PRODUCTS {
            return Err(invalid(
                "too_many_products",
                format!("at most {MAX_PRODUCTS} products"),
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            title_i18n: titles,
            kind: self.kind,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            product_ids: products,
        })
    }
}

struct Row {
    id: Uuid,
    name: String,
    title_i18n: Value,
    kind: String,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    product_ids: Vec<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub fn is_open(
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    starts_at.is_none_or(|s| s <= now) && ends_at.is_none_or(|e| e > now)
}

impl Row {
    fn into_collection(self, now: DateTime<Utc>) -> Collection {
        Collection {
            active: is_open(self.starts_at, self.ends_at, now),
            id: self.id,
            name: self.name,
            title_i18n: serde_json::from_value(self.title_i18n).unwrap_or_default(),
            kind: if self.kind == "seasonal" {
                CollectionKind::Seasonal
            } else {
                CollectionKind::Manual
            },
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            product_ids: self.product_ids,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Every collection, newest first. ponytail: unpaginated; add a cursor when a tenant keeps
/// hundreds of them.
pub async fn list(tx: &mut TenantTx) -> Result<Vec<Collection>, Error> {
    let now = Utc::now();
    Ok(sqlx::query_as!(
        Row,
        "SELECT id, name, title_i18n, kind, starts_at, ends_at, product_ids, created_at, updated_at
         FROM collections ORDER BY id DESC LIMIT 500"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| r.into_collection(now))
    .collect())
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Collection, Error> {
    Ok(sqlx::query_as!(
        Row,
        "SELECT id, name, title_i18n, kind, starts_at, ends_at, product_ids, created_at, updated_at
         FROM collections WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .into_collection(Utc::now()))
}

async fn check_products(tx: &mut TenantTx, ids: &[Uuid]) -> Result<(), Error> {
    let known = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM products WHERE id = ANY($1)"#,
        ids
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).ok() != Some(ids.len()) {
        return Err(invalid("unknown_product", "a product does not exist"));
    }
    Ok(())
}

fn titles(input: &CollectionInput) -> Result<Value, Error> {
    serde_json::to_value(&input.title_i18n).map_err(|e| Error::Internal(e.to_string()))
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &CollectionInput,
) -> Result<Collection, Error> {
    let input = input.validate()?;
    check_products(tx, &input.product_ids).await?;
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO collections (id, tenant_id, name, title_i18n, kind, starts_at, ends_at,
                                  product_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        tx.tenant_id(),
        input.name,
        titles(&input)?,
        input.kind.as_str(),
        input.starts_at,
        input.ends_at,
        &input.product_ids
    )
    .execute(&mut **tx)
    .await?;
    let created = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "collection.created",
        "collection",
        Some(&id.to_string()),
        &json!({ "after": created }),
    )
    .await?;
    Ok(created)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &CollectionInput,
) -> Result<Collection, Error> {
    let input = input.validate()?;
    let before = get(tx, id).await?;
    check_products(tx, &input.product_ids).await?;
    sqlx::query!(
        "UPDATE collections SET name = $2, title_i18n = $3, kind = $4, starts_at = $5,
                                ends_at = $6, product_ids = $7, updated_at = now()
         WHERE id = $1",
        id,
        input.name,
        titles(&input)?,
        input.kind.as_str(),
        input.starts_at,
        input.ends_at,
        &input.product_ids
    )
    .execute(&mut **tx)
    .await?;
    let after = get(tx, id).await?;
    audit::record(
        tx,
        actor,
        "collection.updated",
        "collection",
        Some(&id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM collections WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "collection.deleted",
        "collection",
        Some(&id.to_string()),
        &json!({ "before": before }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> CollectionInput {
        CollectionInput {
            name: "  Vánoce ".into(),
            title_i18n: BTreeMap::from([
                ("cs".into(), " Vánoční tipy ".into()),
                ("sk".into(), " ".into()),
            ]),
            kind: CollectionKind::Seasonal,
            starts_at: Some(DateTime::UNIX_EPOCH),
            ends_at: Some(DateTime::UNIX_EPOCH + chrono::Duration::days(30)),
            product_ids: vec![Uuid::nil(), Uuid::max(), Uuid::nil()],
        }
    }

    #[test]
    fn validation_normalizes() {
        let v = input().validate().unwrap();
        assert_eq!(v.name, "Vánoce");
        assert_eq!(
            v.title_i18n,
            BTreeMap::from([("cs".into(), "Vánoční tipy".into())])
        );
        assert_eq!(v.product_ids, vec![Uuid::nil(), Uuid::max()]);
    }

    #[test]
    fn seasonal_needs_a_window() {
        let mut i = input();
        i.ends_at = None;
        assert!(i.validate().is_err());
        i.kind = CollectionKind::Manual;
        assert!(i.validate().is_ok());
        i.ends_at = i.starts_at;
        assert!(i.validate().is_err());
        let mut bad = input();
        bad.title_i18n = BTreeMap::from([("CZ".into(), "x".into())]);
        assert!(bad.validate().is_err());
        let mut many = input();
        many.product_ids = (0..=MAX_PRODUCTS as u128).map(Uuid::from_u128).collect();
        assert!(many.validate().is_err());
    }

    #[test]
    fn open_window() {
        let t = DateTime::UNIX_EPOCH + chrono::Duration::days(10);
        let day = chrono::Duration::days(1);
        assert!(is_open(None, None, t));
        assert!(is_open(Some(t), Some(t + day), t));
        assert!(!is_open(Some(t + day), None, t));
        assert!(!is_open(None, Some(t), t));
    }
}
