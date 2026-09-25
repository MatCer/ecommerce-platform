//! The AI-writable text fields of products, categories, pages and menus, read from and written
//! back through the existing services (validation, audit log, outbox events).
//!
//! Text fields are JSON strings; a page's `blocks` is its block array and a menu's `labels`
//! is `[{path, label}]` (entry index path like `0` or `0.2`).

use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::categories::{self, Category, CategoryTranslation, CategoryUpdate};
use crate::catalog::products::{self, Product, ProductInput, ProductTranslation, VariantInput};
use crate::content::menus::{self, MenuEntry, MenuInput};
use crate::content::{self, Page, PageInput, PageTranslation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Product,
    Category,
    Page,
    Menu,
}

impl EntityType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Category => "category",
            Self::Page => "page",
            Self::Menu => "menu",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "product" => Some(Self::Product),
            "category" => Some(Self::Category),
            "page" => Some(Self::Page),
            "menu" => Some(Self::Menu),
            _ => None,
        }
    }

    /// Fields a translation covers, in display order.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Product => &[
                "name",
                "slug",
                "short_description",
                "description_html",
                "seo_title",
                "seo_description",
            ],
            Self::Category => &[
                "name",
                "slug",
                "description_html",
                "seo_title",
                "seo_description",
            ],
            Self::Page => &[
                "title",
                "slug",
                "excerpt",
                "seo_title",
                "seo_description",
                "blocks",
            ],
            Self::Menu => &["labels"],
        }
    }

    /// Required when a translation is created (plus `slug`).
    pub fn title_field(self) -> Option<&'static str> {
        match self {
            Self::Product | Self::Category => Some("name"),
            Self::Page => Some("title"),
            Self::Menu => None,
        }
    }
}

/// An entity document as the services read and write it.
#[derive(Debug, Clone)]
pub enum Doc {
    Product(Box<Product>),
    Category(Category),
    Page(Page),
    Menu {
        handle: String,
        items: Vec<MenuEntry>,
    },
}

fn bad_field(field: &str) -> Error {
    Error::Validation {
        code: "unknown_field",
        detail: format!("{field:?} is not an AI-writable field"),
    }
}

fn text(v: &Value, field: &str) -> Result<String, Error> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad_field(field))
}

fn opt(v: &Option<String>) -> Option<Value> {
    v.as_ref().map(|s| json!(s))
}

impl Doc {
    pub async fn load(tx: &mut TenantTx, t: EntityType, id: &str) -> Result<Self, Error> {
        let uuid = || Uuid::parse_str(id).map_err(|_| Error::NotFound);
        Ok(match t {
            EntityType::Product => Self::Product(Box::new(products::get(tx, uuid()?).await?)),
            EntityType::Category => Self::Category(categories::get(tx, uuid()?).await?),
            EntityType::Page => Self::Page(content::get(tx, uuid()?).await?),
            EntityType::Menu => Self::Menu {
                handle: id.to_owned(),
                items: menus::entries(tx, id).await?.ok_or(Error::NotFound)?,
            },
        })
    }

    /// Row-locks the entity until the transaction ends, so a concurrent edit cannot commit
    /// between reading and writing it (`404` when it does not exist).
    pub async fn lock(tx: &mut TenantTx, t: EntityType, id: &str) -> Result<(), Error> {
        let uuid = || Uuid::parse_str(id).map_err(|_| Error::NotFound);
        let found = match t {
            EntityType::Product => {
                sqlx::query_scalar!("SELECT id FROM products WHERE id = $1 FOR UPDATE", uuid()?)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            EntityType::Category => {
                sqlx::query_scalar!(
                    "SELECT id FROM categories WHERE id = $1 FOR UPDATE",
                    uuid()?
                )
                .fetch_optional(&mut **tx)
                .await?
            }
            EntityType::Page => {
                sqlx::query_scalar!("SELECT id FROM pages WHERE id = $1 FOR UPDATE", uuid()?)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            EntityType::Menu => {
                sqlx::query_scalar!("SELECT id FROM menus WHERE handle = $1 FOR UPDATE", id)
                    .fetch_optional(&mut **tx)
                    .await?
            }
        };
        found.map(|_| ()).ok_or(Error::NotFound)
    }

    pub fn entity_type(&self) -> EntityType {
        match self {
            Self::Product(_) => EntityType::Product,
            Self::Category(_) => EntityType::Category,
            Self::Page(_) => EntityType::Page,
            Self::Menu { .. } => EntityType::Menu,
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Product(p) => p.id.to_string(),
            Self::Category(c) => c.id.to_string(),
            Self::Page(p) => p.id.to_string(),
            Self::Menu { handle, .. } => handle.clone(),
        }
    }

    /// Whether the entity has content in `locale` (menus: any label in it).
    pub fn has_locale(&self, locale: &str) -> bool {
        match self {
            Self::Product(p) => p.translations.iter().any(|t| t.locale == locale),
            Self::Category(c) => c.translations.iter().any(|t| t.locale == locale),
            Self::Page(p) => p.translations.iter().any(|t| t.locale == locale),
            Self::Menu { items, .. } => labels(items, locale).iter().any(|(_, l)| !l.is_empty()),
        }
    }

    /// The current value of a field (`None`: no translation in `locale`, or an unset field).
    pub fn get(&self, locale: &str, field: &str) -> Option<Value> {
        match self {
            Self::Product(p) => {
                let t = p.translations.iter().find(|t| t.locale == locale)?;
                match field {
                    "name" => Some(json!(t.name)),
                    "slug" => Some(json!(t.slug)),
                    "short_description" => Some(json!(t.short_description)),
                    "description_html" => Some(json!(t.description_html)),
                    "seo_title" => opt(&t.seo_title),
                    "seo_description" => opt(&t.seo_description),
                    _ => None,
                }
            }
            Self::Category(c) => {
                let t = c.translations.iter().find(|t| t.locale == locale)?;
                match field {
                    "name" => Some(json!(t.name)),
                    "slug" => Some(json!(t.slug)),
                    "description_html" => Some(json!(t.description_html)),
                    "seo_title" => opt(&t.seo_title),
                    "seo_description" => opt(&t.seo_description),
                    _ => None,
                }
            }
            Self::Page(p) => {
                let t = p.translations.iter().find(|t| t.locale == locale)?;
                match field {
                    "title" => Some(json!(t.title)),
                    "slug" => Some(json!(t.slug)),
                    "excerpt" => Some(json!(t.excerpt)),
                    "seo_title" => opt(&t.seo_title),
                    "seo_description" => opt(&t.seo_description),
                    "blocks" => serde_json::to_value(&t.blocks).ok(),
                    _ => None,
                }
            }
            Self::Menu { items, .. } => {
                if field != "labels" {
                    return None;
                }
                let list: Vec<Value> = labels(items, locale)
                    .into_iter()
                    .filter(|(_, l)| !l.is_empty())
                    .map(|(path, label)| json!({ "path": path, "label": label }))
                    .collect();
                (!list.is_empty()).then(|| Value::Array(list))
            }
        }
    }

    /// Sets a field, creating an empty translation for `locale` when needed.
    pub fn set(&mut self, locale: &str, field: &str, value: &Value) -> Result<(), Error> {
        match self {
            Self::Product(p) => {
                let t = match p.translations.iter().position(|t| t.locale == locale) {
                    Some(i) => &mut p.translations[i],
                    None => {
                        p.translations.push(ProductTranslation {
                            locale: locale.to_owned(),
                            name: String::new(),
                            slug: String::new(),
                            description_html: String::new(),
                            short_description: String::new(),
                            seo_title: None,
                            seo_description: None,
                        });
                        p.translations.last_mut().ok_or_else(|| bad_field(field))?
                    }
                };
                match field {
                    "name" => t.name = text(value, field)?,
                    "slug" => t.slug = text(value, field)?,
                    "short_description" => t.short_description = text(value, field)?,
                    "description_html" => t.description_html = text(value, field)?,
                    "seo_title" => t.seo_title = Some(text(value, field)?),
                    "seo_description" => t.seo_description = Some(text(value, field)?),
                    _ => return Err(bad_field(field)),
                }
            }
            Self::Category(c) => {
                let t = match c.translations.iter().position(|t| t.locale == locale) {
                    Some(i) => &mut c.translations[i],
                    None => {
                        c.translations.push(CategoryTranslation {
                            locale: locale.to_owned(),
                            name: String::new(),
                            slug: String::new(),
                            description_html: String::new(),
                            seo_title: None,
                            seo_description: None,
                        });
                        c.translations.last_mut().ok_or_else(|| bad_field(field))?
                    }
                };
                match field {
                    "name" => t.name = text(value, field)?,
                    "slug" => t.slug = text(value, field)?,
                    "description_html" => t.description_html = text(value, field)?,
                    "seo_title" => t.seo_title = Some(text(value, field)?),
                    "seo_description" => t.seo_description = Some(text(value, field)?),
                    _ => return Err(bad_field(field)),
                }
            }
            Self::Page(p) => {
                let t = match p.translations.iter().position(|t| t.locale == locale) {
                    Some(i) => &mut p.translations[i],
                    None => {
                        p.translations.push(PageTranslation {
                            locale: locale.to_owned(),
                            title: String::new(),
                            slug: String::new(),
                            excerpt: String::new(),
                            blocks: vec![],
                            seo_title: None,
                            seo_description: None,
                        });
                        p.translations.last_mut().ok_or_else(|| bad_field(field))?
                    }
                };
                match field {
                    "title" => t.title = text(value, field)?,
                    "slug" => t.slug = text(value, field)?,
                    "excerpt" => t.excerpt = text(value, field)?,
                    "seo_title" => t.seo_title = Some(text(value, field)?),
                    "seo_description" => t.seo_description = Some(text(value, field)?),
                    "blocks" => {
                        t.blocks =
                            serde_json::from_value(value.clone()).map_err(|_| bad_field(field))?;
                    }
                    _ => return Err(bad_field(field)),
                }
            }
            Self::Menu { items, .. } => {
                if field != "labels" {
                    return Err(bad_field(field));
                }
                for entry in value.as_array().ok_or_else(|| bad_field(field))? {
                    let path = entry["path"].as_str().ok_or_else(|| bad_field(field))?;
                    let label = entry["label"].as_str().ok_or_else(|| bad_field(field))?;
                    let e = entry_at(items, path).ok_or_else(|| bad_field(field))?;
                    e.label_i18n.insert(locale.to_owned(), label.to_owned());
                }
            }
        }
        Ok(())
    }

    /// Writes the document through its service; returns what was stored.
    pub async fn save(self, tx: &mut TenantTx, actor: &str) -> Result<Self, Error> {
        Ok(match self {
            Self::Product(p) => {
                let saved = products::replace(tx, actor, p.id, &product_input(&p)).await?;
                Self::Product(Box::new(saved))
            }
            Self::Category(c) => Self::Category(
                categories::update(
                    tx,
                    actor,
                    c.id,
                    &CategoryUpdate {
                        image_asset_id: c.image_asset_id,
                        translations: c.translations,
                    },
                )
                .await?,
            ),
            Self::Page(p) => Self::Page(
                content::update(
                    tx,
                    actor,
                    p.id,
                    &PageInput {
                        kind: p.kind,
                        legal_type: p.legal_type,
                        status: p.status,
                        published_at: p.published_at,
                        image_asset_id: p.image_asset_id,
                        translations: p.translations,
                    },
                )
                .await?,
            ),
            Self::Menu { handle, items } => {
                let menu = menus::put(tx, actor, &handle, &MenuInput { items }).await?;
                Self::Menu {
                    handle,
                    items: menu.items,
                }
            }
        })
    }
}

/// The product as a replace document (every part kept as it is).
pub fn product_input(p: &Product) -> ProductInput {
    ProductInput {
        status: p.status,
        brand: p.brand.clone(),
        gpsr: p.gpsr.clone(),
        unit_measure: p.unit_measure,
        unit_quantity: p.unit_quantity,
        heureka_category: p.heureka_category.clone(),
        google_category: p.google_category.clone(),
        translations: p.translations.clone(),
        options: p.options.clone(),
        variants: p
            .variants
            .iter()
            .map(|v| VariantInput {
                id: Some(v.id),
                sku: v.sku.clone(),
                ean: v.ean.clone(),
                option_values: v.option_values.clone(),
                weight_g: v.weight_g,
                is_default: v.is_default,
            })
            .collect(),
        category_ids: p.category_ids.clone(),
        media: p.media.clone(),
        parameters: p.parameters.clone(),
        tax_categories: p.tax_categories.clone(),
    }
}

/// `(path, label in locale)` of every entry, depth-first (`"0"`, `"0.1"`, ...).
pub fn labels(items: &[MenuEntry], locale: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    for (i, e) in items.iter().enumerate() {
        out.push((
            i.to_string(),
            e.label_i18n.get(locale).cloned().unwrap_or_default(),
        ));
        for (j, c) in e.children.iter().enumerate() {
            out.push((
                format!("{i}.{j}"),
                c.label_i18n.get(locale).cloned().unwrap_or_default(),
            ));
        }
    }
    out
}

fn entry_at<'a>(items: &'a mut [MenuEntry], path: &str) -> Option<&'a mut MenuEntry> {
    let mut parts = path.split('.').map(str::parse::<usize>);
    let first = items.get_mut(parts.next()?.ok()?)?;
    match parts.next() {
        None => Some(first),
        Some(j) => {
            let child = first.children.get_mut(j.ok()?)?;
            parts.next().is_none().then_some(child)
        }
    }
}
