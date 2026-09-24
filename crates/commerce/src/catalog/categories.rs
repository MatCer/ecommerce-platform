//! Category tree (spec §7.1). Siblings are ordered by a dense `position` (0..n). Every tree
//! mutation takes a per-tenant advisory lock, so concurrent moves cannot build a cycle that
//! each move alone would not.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{MAX_HTML, check_opt_text, check_text, db_error, sanitize_html, slug_valid};
use crate::audit;
use crate::markets::{invalid, is_locale};

/// Deepest allowed tree (root = depth 1).
pub const MAX_DEPTH: i32 = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryTranslation {
    #[schema(example = "cs")]
    pub locale: String,
    #[schema(example = "Trička")]
    pub name: String,
    #[schema(example = "tricka")]
    pub slug: String,
    /// Sanitized on write.
    #[serde(default)]
    pub description_html: String,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCategory {
    /// `null` for a root category. The new category is appended to its siblings.
    pub parent_id: Option<Uuid>,
    pub image_asset_id: Option<Uuid>,
    pub translations: Vec<CategoryTranslation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryUpdate {
    pub image_asset_id: Option<Uuid>,
    pub translations: Vec<CategoryTranslation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryMove {
    /// New parent, `null` for the root level.
    pub parent_id: Option<Uuid>,
    /// 0-based index among the new siblings; larger values append.
    pub position: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Category {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub position: i32,
    pub image_asset_id: Option<Uuid>,
    pub translations: Vec<CategoryTranslation>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A category with its subtree.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CategoryNode {
    #[serde(flatten)]
    pub category: Category,
    #[schema(no_recursion)]
    pub children: Vec<CategoryNode>,
}

fn validate_translations(translations: &[CategoryTranslation]) -> Result<(), Error> {
    const CODE: &str = "invalid_translation";
    if translations.is_empty() || translations.len() > 20 {
        return Err(invalid(CODE, "1-20 translations are required"));
    }
    let mut locales: Vec<&str> = translations.iter().map(|t| t.locale.as_str()).collect();
    locales.sort_unstable();
    locales.dedup();
    if locales.len() != translations.len() {
        return Err(invalid(CODE, "one translation per locale"));
    }
    for t in translations {
        if !is_locale(&t.locale) {
            return Err(invalid(CODE, format!("invalid locale {:?}", t.locale)));
        }
        check_text("name", CODE, &t.name, 1, 200)?;
        if !slug_valid(&t.slug) {
            return Err(invalid(
                "invalid_slug",
                "slug must be lowercase letters and digits joined by hyphens",
            ));
        }
        check_text("description_html", CODE, &t.description_html, 0, MAX_HTML)?;
        check_opt_text("seo_title", CODE, t.seo_title.as_deref(), 200)?;
        check_opt_text("seo_description", CODE, t.seo_description.as_deref(), 500)?;
    }
    Ok(())
}

async fn lock_tree(tx: &mut TenantTx) -> Result<(), Error> {
    let key = format!("categories:{}", tx.tenant_id());
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", key)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

async fn save_translations(
    tx: &mut TenantTx,
    id: Uuid,
    translations: &[CategoryTranslation],
) -> Result<(), Error> {
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "DELETE FROM category_translations WHERE category_id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    for t in translations {
        sqlx::query!(
            "INSERT INTO category_translations (tenant_id, category_id, locale, name, slug,
                 description_html, seo_title, seo_description)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            tenant_id,
            id,
            t.locale,
            t.name.trim(),
            t.slug,
            sanitize_html(&t.description_html),
            t.seo_title,
            t.seo_description
        )
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

/// Depth of `parent` (1 for a root) or `None` when it does not exist in this tenant.
async fn depth(tx: &mut TenantTx, parent: Uuid) -> Result<Option<i32>, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up AS (
               SELECT id, parent_id, 1 AS depth FROM categories WHERE id = $1
               UNION ALL
               SELECT c.id, c.parent_id, up.depth + 1 FROM categories c JOIN up ON c.id = up.parent_id
               WHERE up.depth <= $2
           )
           SELECT max(depth) AS "depth" FROM up"#,
        parent,
        MAX_DEPTH + 1
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// Height of the subtree rooted at `id` (1 for a leaf).
async fn height(tx: &mut TenantTx, id: Uuid) -> Result<i32, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE down AS (
               SELECT id, 1 AS h FROM categories WHERE id = $1
               UNION ALL
               SELECT c.id, down.h + 1 FROM categories c JOIN down ON c.parent_id = down.id
               WHERE down.h <= $2
           )
           SELECT coalesce(max(h), 1) AS "h!" FROM down"#,
        id,
        MAX_DEPTH + 1
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// Is `candidate` inside the subtree of `id` (or `id` itself)?
async fn in_subtree(tx: &mut TenantTx, id: Uuid, candidate: Uuid) -> Result<bool, Error> {
    Ok(sqlx::query_scalar!(
        r#"WITH RECURSIVE up AS (
               SELECT id, parent_id FROM categories WHERE id = $2
               UNION
               SELECT c.id, c.parent_id FROM categories c JOIN up ON c.id = up.parent_id
           )
           SELECT EXISTS (SELECT 1 FROM up WHERE id = $1) AS "found!""#,
        id,
        candidate
    )
    .fetch_one(&mut **tx)
    .await?)
}

/// Rewrites the positions of `parent`'s children to 0..n in the given order.
async fn renumber(tx: &mut TenantTx, ordered: &[Uuid]) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE categories c SET position = o.ord - 1
         FROM unnest($1::uuid[]) WITH ORDINALITY AS o(id, ord)
         WHERE c.id = o.id AND c.position <> o.ord - 1",
        ordered
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn siblings(
    tx: &mut TenantTx,
    parent: Option<Uuid>,
    except: Uuid,
) -> Result<Vec<Uuid>, Error> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM categories WHERE parent_id IS NOT DISTINCT FROM $1 AND id <> $2
         ORDER BY position, id",
        parent,
        except
    )
    .fetch_all(&mut **tx)
    .await?)
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &NewCategory,
) -> Result<Category, Error> {
    validate_translations(&input.translations)?;
    lock_tree(tx).await?;
    if let Some(parent) = input.parent_id {
        match depth(tx, parent).await? {
            None => {
                return Err(invalid(
                    "unknown_reference",
                    "parent category does not exist",
                ));
            }
            Some(d) if d >= MAX_DEPTH => {
                return Err(invalid(
                    "too_deep",
                    format!("categories nest at most {MAX_DEPTH} levels"),
                ));
            }
            Some(_) => {}
        }
    }
    let id = crate::id::new_id();
    sqlx::query!(
        "INSERT INTO categories (id, tenant_id, parent_id, position, image_asset_id)
         SELECT $1, $2, $3, coalesce(max(position) + 1, 0), $4
         FROM categories WHERE parent_id IS NOT DISTINCT FROM $3",
        id,
        tx.tenant_id(),
        input.parent_id,
        input.image_asset_id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    save_translations(tx, id, &input.translations).await?;
    let after = get(tx, id).await?;
    record(tx, actor, "category.created", id, json!({ "after": after })).await?;
    Ok(after)
}

pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &CategoryUpdate,
) -> Result<Category, Error> {
    validate_translations(&input.translations)?;
    let before = get(tx, id).await?;
    sqlx::query!(
        "UPDATE categories SET image_asset_id = $2, updated_at = now() WHERE id = $1",
        id,
        input.image_asset_id
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    save_translations(tx, id, &input.translations).await?;
    let after = get(tx, id).await?;
    record(
        tx,
        actor,
        "category.updated",
        id,
        json!({ "before": before, "after": after }),
    )
    .await?;
    Ok(after)
}

/// Moves a category (with its subtree) under `parent_id` at `position`. Moving a category
/// into its own subtree is `422 category_cycle`.
pub async fn move_to(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    m: &CategoryMove,
) -> Result<Category, Error> {
    if m.position < 0 {
        return Err(invalid("invalid_position", "position must be >= 0"));
    }
    lock_tree(tx).await?;
    let before = get(tx, id).await?;
    if let Some(parent) = m.parent_id {
        if in_subtree(tx, id, parent).await? {
            return Err(invalid(
                "category_cycle",
                "a category cannot move into its own subtree",
            ));
        }
        let d = depth(tx, parent)
            .await?
            .ok_or_else(|| invalid("unknown_reference", "parent category does not exist"))?;
        if d + height(tx, id).await? > MAX_DEPTH {
            return Err(invalid(
                "too_deep",
                format!("categories nest at most {MAX_DEPTH} levels"),
            ));
        }
    }
    let old = siblings(tx, before.parent_id, id).await?;
    let mut new = siblings(tx, m.parent_id, id).await?;
    let at = usize::try_from(m.position)
        .unwrap_or(usize::MAX)
        .min(new.len());
    new.insert(at, id);
    sqlx::query!(
        "UPDATE categories SET parent_id = $2, updated_at = now() WHERE id = $1",
        id,
        m.parent_id
    )
    .execute(&mut **tx)
    .await?;
    renumber(tx, &new).await?;
    if before.parent_id != m.parent_id {
        renumber(tx, &old).await?;
    }
    let after = get(tx, id).await?;
    record(
        tx,
        actor,
        "category.moved",
        id,
        json!({ "before": { "parent_id": before.parent_id, "position": before.position },
                "after": { "parent_id": after.parent_id, "position": after.position } }),
    )
    .await?;
    Ok(after)
}

/// Deletes a leaf category; products lose the assignment. `409 has_children` otherwise.
pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    lock_tree(tx).await?;
    let before = get(tx, id).await?;
    let has_children = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM categories WHERE parent_id = $1) AS "x!""#,
        id
    )
    .fetch_one(&mut **tx)
    .await?;
    if has_children {
        return Err(Error::Conflict {
            code: "has_children",
            detail: "move or delete the subcategories first".into(),
        });
    }
    sqlx::query!("DELETE FROM categories WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    let rest = siblings(tx, before.parent_id, id).await?;
    renumber(tx, &rest).await?;
    record(
        tx,
        actor,
        "category.deleted",
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
    audit::record(tx, actor, action, "category", Some(&id.to_string()), &diff).await?;
    platform::queue::publish(&mut **tx, action, &json!({ "category_id": id })).await?;
    Ok(())
}

async fn load(tx: &mut TenantTx, id: Option<Uuid>) -> Result<Vec<Category>, Error> {
    let rows = sqlx::query!(
        "SELECT id, parent_id, position, image_asset_id, created_at, updated_at FROM categories
         WHERE $1::uuid IS NULL OR id = $1 ORDER BY parent_id NULLS FIRST, position, id",
        id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut translations = sqlx::query!(
        "SELECT category_id, locale, name, slug, description_html, seo_title, seo_description
         FROM category_translations WHERE $1::uuid IS NULL OR category_id = $1
         ORDER BY category_id, locale",
        id
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .fold(
        std::collections::HashMap::<Uuid, Vec<CategoryTranslation>>::new(),
        |mut acc, r| {
            acc.entry(r.category_id)
                .or_default()
                .push(CategoryTranslation {
                    locale: r.locale,
                    name: r.name,
                    slug: r.slug,
                    description_html: r.description_html,
                    seo_title: r.seo_title,
                    seo_description: r.seo_description,
                });
            acc
        },
    );
    Ok(rows
        .into_iter()
        .map(|r| Category {
            id: r.id,
            parent_id: r.parent_id,
            position: r.position,
            image_asset_id: r.image_asset_id,
            translations: translations.remove(&r.id).unwrap_or_default(),
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect())
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Category, Error> {
    load(tx, Some(id)).await?.pop().ok_or(Error::NotFound)
}

/// The whole tree, roots first, siblings by position.
pub async fn tree(tx: &mut TenantTx) -> Result<Vec<CategoryNode>, Error> {
    let all = load(tx, None).await?;
    Ok(build_tree(all))
}

fn build_tree(all: Vec<Category>) -> Vec<CategoryNode> {
    let mut children: std::collections::HashMap<Option<Uuid>, Vec<Category>> =
        std::collections::HashMap::new();
    for c in all {
        children.entry(c.parent_id).or_default().push(c);
    }
    fn attach(
        parent: Option<Uuid>,
        children: &mut std::collections::HashMap<Option<Uuid>, Vec<Category>>,
    ) -> Vec<CategoryNode> {
        let mut level = children.remove(&parent).unwrap_or_default();
        level.sort_by_key(|c| (c.position, c.id));
        level
            .into_iter()
            .map(|category| {
                let kids = attach(Some(category.id), children);
                CategoryNode {
                    category,
                    children: kids,
                }
            })
            .collect()
    }
    attach(None, &mut children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: u128, parent: Option<u128>, position: i32) -> Category {
        Category {
            id: Uuid::from_u128(id),
            parent_id: parent.map(Uuid::from_u128),
            position,
            image_asset_id: None,
            translations: vec![],
            created_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn builds_nested_tree_in_position_order() {
        let tree = build_tree(vec![
            cat(3, Some(1), 1),
            cat(1, None, 0),
            cat(2, Some(1), 0),
            cat(4, None, 1),
        ]);
        let ids: Vec<u128> = tree.iter().map(|n| n.category.id.as_u128()).collect();
        assert_eq!(ids, [1, 4]);
        let kids: Vec<u128> = tree[0]
            .children
            .iter()
            .map(|n| n.category.id.as_u128())
            .collect();
        assert_eq!(kids, [2, 3]);
    }

    #[test]
    fn translations_are_validated() {
        let t = CategoryTranslation {
            locale: "cs".into(),
            name: "Trička".into(),
            slug: "tricka".into(),
            description_html: String::new(),
            seo_title: None,
            seo_description: None,
        };
        assert!(validate_translations(std::slice::from_ref(&t)).is_ok());
        assert!(validate_translations(&[]).is_err());
        assert!(validate_translations(&[t.clone(), t.clone()]).is_err());
        let bad = CategoryTranslation {
            slug: "Trička".into(),
            ..t
        };
        assert_eq!(
            validate_translations(&[bad]).unwrap_err().code(),
            "invalid_slug"
        );
    }
}
