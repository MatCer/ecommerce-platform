//! Parameters (spec §7.1): typed, translatable product attributes, optionally filterable
//! (search facets). Values live on products or variants (`products::ParameterValue`).

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{I18n, check_i18n, check_opt_text, code_valid, db_error};
use crate::audit;
use crate::markets::invalid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    /// Translatable text: values are `{"cs": "bavlna", "en": "cotton"}`.
    Text,
    Number,
    Bool,
}

impl ParameterKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bool => "bool",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "number" => Self::Number,
            "bool" => Self::Bool,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterInput {
    /// Unique per tenant, e.g. `material`.
    #[schema(example = "material")]
    pub key: String,
    pub name_i18n: I18n,
    /// Cannot change while values exist.
    pub kind: ParameterKind,
    /// Display unit for numbers, e.g. `cm`.
    pub unit: Option<String>,
    #[serde(default)]
    pub filterable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Parameter {
    pub id: Uuid,
    pub key: String,
    pub name_i18n: I18n,
    pub kind: ParameterKind,
    pub unit: Option<String>,
    pub filterable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ParameterInput {
    pub fn validate(&self) -> Result<(), Error> {
        if !code_valid(&self.key) {
            return Err(invalid(
                "invalid_key",
                "key must be 1-64 lowercase letters, digits, `_` or `-`",
            ));
        }
        check_i18n("name_i18n", "invalid_name", &self.name_i18n, 200, true)?;
        check_opt_text("unit", "invalid_unit", self.unit.as_deref(), 20)
    }
}

/// A value must match its parameter's kind.
pub(crate) fn check_value(kind: &str, value: &Value) -> Result<(), Error> {
    let ok = match ParameterKind::parse(kind) {
        ParameterKind::Number => value.as_f64().is_some_and(f64::is_finite),
        ParameterKind::Bool => value.is_boolean(),
        ParameterKind::Text => serde_json::from_value::<I18n>(value.clone())
            .ok()
            .is_some_and(|m| check_i18n("value", "invalid_parameters", &m, 1000, true).is_ok()),
    };
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "invalid_parameters",
            format!("value does not match the parameter kind {kind}"),
        ))
    }
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ParameterPage {
    pub items: Vec<Parameter>,
    /// Pass as `cursor` for the next page (the last key); absent on the last page.
    pub next_cursor: Option<String>,
}

pub const MAX_PAGE: i64 = 100;

/// Parameters by key, keyset-paginated on the key.
pub async fn list(
    tx: &mut TenantTx,
    cursor: Option<&str>,
    limit: i64,
) -> Result<ParameterPage, Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_PAGE}"),
        ));
    }
    let rows = sqlx::query!(
        "SELECT id, key, name_i18n, kind, unit, filterable, created_at, updated_at
         FROM parameters WHERE $1::text IS NULL OR key > $1 ORDER BY key LIMIT $2",
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items = rows
        .into_iter()
        .map(|r| {
            Ok(Parameter {
                id: r.id,
                key: r.key,
                name_i18n: serde_json::from_value(r.name_i18n).map_err(internal)?,
                kind: ParameterKind::parse(&r.kind),
                unit: r.unit,
                filterable: r.filterable,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if more {
        items.last().map(|p| p.key.clone())
    } else {
        None
    };
    Ok(ParameterPage { items, next_cursor })
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Parameter, Error> {
    let r = sqlx::query!(
        "SELECT id, key, name_i18n, kind, unit, filterable, created_at, updated_at
         FROM parameters WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(Parameter {
        id: r.id,
        key: r.key,
        name_i18n: serde_json::from_value(r.name_i18n).map_err(internal)?,
        kind: ParameterKind::parse(&r.kind),
        unit: r.unit,
        filterable: r.filterable,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &ParameterInput,
) -> Result<Parameter, Error> {
    input.validate()?;
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO parameters (id, tenant_id, key, name_i18n, kind, unit, filterable)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        tx.tenant_id(),
        input.key,
        serde_json::to_value(&input.name_i18n).map_err(internal)?,
        input.kind.as_str(),
        input.unit,
        input.filterable
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let after = get(tx, id).await?;
    record(
        tx,
        actor,
        "parameter.created",
        id,
        json!({ "after": after }),
    )
    .await?;
    Ok(after)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &ParameterInput,
) -> Result<Parameter, Error> {
    input.validate()?;
    // Serializes with product saves validating values of this parameter (FOR SHARE there).
    sqlx::query!("SELECT id FROM parameters WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let before = get(tx, id).await?;
    if before.kind != input.kind {
        let used = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM product_parameter_values WHERE parameter_id = $1) AS "used!""#,
            id
        )
        .fetch_one(&mut **tx)
        .await?;
        if used {
            return Err(Error::Conflict {
                code: "parameter_in_use",
                detail: "the kind cannot change while products have values".into(),
            });
        }
    }
    sqlx::query!(
        "UPDATE parameters SET key = $2, name_i18n = $3, kind = $4, unit = $5, filterable = $6,
                updated_at = now()
         WHERE id = $1",
        id,
        input.key,
        serde_json::to_value(&input.name_i18n).map_err(internal)?,
        input.kind.as_str(),
        input.unit,
        input.filterable
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    let after = get(tx, id).await?;
    record(
        tx,
        actor,
        "parameter.updated",
        id,
        json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}

/// Deletes the parameter and its values on every product.
pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let before = get(tx, id).await?;
    sqlx::query!("DELETE FROM parameters WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    record(
        tx,
        actor,
        "parameter.deleted",
        id,
        json!({ "before": before }),
    )
    .await
}

async fn record(
    tx: &mut TenantTx,
    actor: &str,
    action: &str,
    id: Uuid,
    diff: Value,
) -> Result<(), Error> {
    audit::record(tx, actor, action, "parameter", Some(&id.to_string()), &diff).await?;
    platform::queue::publish(&mut **tx, action, &json!({ "parameter_id": id })).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_kinds() {
        assert!(check_value("number", &json!(12.5)).is_ok());
        assert!(check_value("number", &json!("12")).is_err());
        assert!(check_value("bool", &json!(true)).is_ok());
        assert!(check_value("bool", &json!(1)).is_err());
        assert!(check_value("text", &json!({"cs": "bavlna"})).is_ok());
        assert!(check_value("text", &json!({})).is_err());
        assert!(check_value("text", &json!("bavlna")).is_err());
        assert!(check_value("text", &json!({"cs": 1})).is_err());
    }

    #[test]
    fn keys_are_validated() {
        let p = ParameterInput {
            key: "Material".into(),
            name_i18n: [("cs".to_owned(), "Materiál".to_owned())].into(),
            kind: ParameterKind::Text,
            unit: None,
            filterable: true,
        };
        assert_eq!(p.validate().unwrap_err().code(), "invalid_key");
        assert!(
            ParameterInput {
                key: "material".into(),
                ..p
            }
            .validate()
            .is_ok()
        );
    }
}
