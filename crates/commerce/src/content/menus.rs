//! Menus by handle (spec §7.5): `main` (header) and `footer` are what the default theme shows.
//! Entries link to a category, product, page or URL, at most two levels deep. Targets are
//! resolved when the storefront renders (a deleted or unpublished target drops the entry).

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::blocks::href_ok;
use crate::audit;
use crate::catalog::{I18n, check_i18n, code_valid};
use crate::markets::invalid;

pub const MAX_ENTRIES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MenuLink {
    Category {
        id: Uuid,
    },
    Product {
        id: Uuid,
    },
    /// A CMS page, legal page or blog post.
    Page {
        id: Uuid,
    },
    /// A shop path (`/search?q=x`) or an `https:`/`mailto:`/`tel:` URL.
    Url {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuEntry {
    /// Label per locale; empty = the target's own name (required for URLs).
    #[serde(default)]
    pub label_i18n: I18n,
    pub link: MenuLink,
    /// Second level (entries here cannot have children).
    #[serde(default)]
    #[schema(no_recursion)]
    pub children: Vec<MenuEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuInput {
    pub items: Vec<MenuEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Menu {
    pub id: Uuid,
    #[schema(example = "main")]
    pub handle: String,
    pub items: Vec<MenuEntry>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MenuList {
    pub items: Vec<Menu>,
}

fn check_entries(entries: &[MenuEntry], depth: usize) -> Result<(), Error> {
    if entries.len() > MAX_ENTRIES {
        return Err(invalid(
            "invalid_menu",
            format!("a menu level has at most {MAX_ENTRIES} entries"),
        ));
    }
    for e in entries {
        check_i18n("label", "invalid_menu", &e.label_i18n, 100, false)?;
        if let MenuLink::Url { url } = &e.link {
            if !href_ok(url) {
                return Err(invalid(
                    "invalid_href",
                    "menu URLs must be a shop path (/...) or an https:, mailto: or tel: URL",
                ));
            }
            if e.label_i18n.is_empty() {
                return Err(invalid("invalid_menu", "URL entries need a label"));
            }
        }
        if depth > 0 && !e.children.is_empty() {
            return Err(invalid("invalid_menu", "menus have at most two levels"));
        }
        check_entries(&e.children, depth + 1)?;
    }
    Ok(())
}

impl MenuInput {
    pub fn validate(&self) -> Result<(), Error> {
        check_entries(&self.items, 0)
    }

    fn targets(&self) -> (Vec<Uuid>, Vec<Uuid>, Vec<Uuid>) {
        let (mut c, mut p, mut g) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
        let all = self
            .items
            .iter()
            .flat_map(|e| std::iter::once(e).chain(&e.children));
        for e in all {
            match &e.link {
                MenuLink::Category { id } => c.insert(*id),
                MenuLink::Product { id } => p.insert(*id),
                MenuLink::Page { id } => g.insert(*id),
                MenuLink::Url { .. } => false,
            };
        }
        (
            c.into_iter().collect(),
            p.into_iter().collect(),
            g.into_iter().collect(),
        )
    }
}

fn check_handle(handle: &str) -> Result<(), Error> {
    if code_valid(handle) {
        Ok(())
    } else {
        Err(invalid(
            "invalid_handle",
            "handle: [a-z0-9][a-z0-9_-]{0,63} (main, footer, ...)",
        ))
    }
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

pub async fn list(tx: &mut TenantTx) -> Result<MenuList, Error> {
    let items = sqlx::query!("SELECT id, handle, items, updated_at FROM menus ORDER BY handle")
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|r| {
            Ok(Menu {
                id: r.id,
                handle: r.handle,
                items: serde_json::from_value(r.items).map_err(internal)?,
                updated_at: r.updated_at,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(MenuList { items })
}

/// The stored entries of a menu (storefront rendering), if it exists.
pub async fn entries(tx: &mut TenantTx, handle: &str) -> Result<Option<Vec<MenuEntry>>, Error> {
    sqlx::query_scalar!("SELECT items FROM menus WHERE handle = $1", handle)
        .fetch_optional(&mut **tx)
        .await?
        .map(|v| serde_json::from_value(v).map_err(internal))
        .transpose()
}

/// Creates or replaces the menu `handle`.
pub async fn put(
    tx: &mut TenantTx,
    actor: &str,
    handle: &str,
    input: &MenuInput,
) -> Result<Menu, Error> {
    check_handle(handle)?;
    input.validate()?;
    let (categories, products, pages) = input.targets();
    let known = sqlx::query_scalar!(
        r#"SELECT (SELECT count(*) FROM categories WHERE id = ANY($1))
                + (SELECT count(*) FROM products WHERE id = ANY($2))
                + (SELECT count(*) FROM pages WHERE id = ANY($3)) AS "n!""#,
        &categories,
        &products,
        &pages
    )
    .fetch_one(&mut **tx)
    .await?;
    if usize::try_from(known).ok() != Some(categories.len() + products.len() + pages.len()) {
        return Err(invalid(
            "unknown_reference",
            "a menu entry links to a category, product or page that does not exist",
        ));
    }
    let items = serde_json::to_value(&input.items).map_err(internal)?;
    let row = sqlx::query!(
        "INSERT INTO menus (tenant_id, handle, items) VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, handle) DO UPDATE SET items = $3, updated_at = now()
         RETURNING id, updated_at",
        tx.tenant_id(),
        handle,
        items
    )
    .fetch_one(&mut **tx)
    .await?;
    let menu = Menu {
        id: row.id,
        handle: handle.to_owned(),
        items: input.items.clone(),
        updated_at: row.updated_at,
    };
    audit::record(
        tx,
        actor,
        "menu.saved",
        "menu",
        Some(&row.id.to_string()),
        &json!({ "after": menu }),
    )
    .await?;
    platform::queue::publish(
        &mut **tx,
        super::MENU_CHANGED_EVENT,
        &json!({ "handle": handle }),
    )
    .await?;
    Ok(menu)
}

pub async fn delete(tx: &mut TenantTx, actor: &str, handle: &str) -> Result<(), Error> {
    let id = sqlx::query_scalar!("DELETE FROM menus WHERE handle = $1 RETURNING id", handle)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    audit::record(
        tx,
        actor,
        "menu.deleted",
        "menu",
        Some(&id.to_string()),
        &json!({ "handle": handle }),
    )
    .await?;
    platform::queue::publish(
        &mut **tx,
        super::MENU_CHANGED_EVENT,
        &json!({ "handle": handle }),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn menu_rules() {
        let ok: MenuInput = serde_json::from_value(json!({ "items": [
            { "link": { "type": "category", "id": Uuid::now_v7() }, "children": [
                { "label_i18n": { "cs": "Akce" }, "link": { "type": "url", "url": "/search?q=akce" } }
            ]},
            { "label_i18n": { "cs": "Blog" }, "link": { "type": "url", "url": "https://blog.example" } }
        ]}))
        .unwrap();
        assert!(ok.validate().is_ok());
        assert_eq!(ok.targets().0.len(), 1);

        let deep: MenuInput = serde_json::from_value(json!({ "items": [
            { "label_i18n": { "cs": "a" }, "link": { "type": "url", "url": "/a" }, "children": [
                { "label_i18n": { "cs": "b" }, "link": { "type": "url", "url": "/b" }, "children": [
                    { "label_i18n": { "cs": "c" }, "link": { "type": "url", "url": "/c" } }
                ]}
            ]}
        ]}))
        .unwrap();
        assert!(deep.validate().is_err(), "three levels");

        for bad in ["javascript:alert(1)", "//evil.example", "http://x.example"] {
            let m: MenuInput = serde_json::from_value(json!({ "items": [
                { "label_i18n": { "cs": "x" }, "link": { "type": "url", "url": bad } }
            ]}))
            .unwrap();
            assert!(m.validate().is_err(), "{bad}");
        }
        let unlabeled: MenuInput = serde_json::from_value(json!({ "items": [
            { "link": { "type": "url", "url": "/x" } }
        ]}))
        .unwrap();
        assert!(unlabeled.validate().is_err());
        assert!(check_handle("main").is_ok() && check_handle("Main").is_err());
    }
}
