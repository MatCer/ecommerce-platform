//! Content blocks (spec §7.5): a small typed set that themes render. Validated and sanitized on
//! write, so what is stored is always safe to render; links are same-shop paths or `https:`,
//! `mailto:` and `tel:` URLs.

use std::collections::BTreeSet;

use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalog::{MAX_HTML, check_text, sanitize_html};
use crate::markets::invalid;

pub const MAX_BLOCKS: usize = 100;
pub const MAX_GRID_PRODUCTS: usize = 24;
pub const MAX_FAQ_ITEMS: usize = 50;
const MAX_URL: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FaqItem {
    pub question: String,
    /// Sanitized on write.
    pub answer_html: String,
}

/// One block of a page. `type` selects the variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    /// A section heading; `level` 2 or 3 (the page title is the only h1).
    Heading {
        text: String,
        #[serde(default = "h2")]
        level: u8,
    },
    /// Rich text (HTML), sanitized on write.
    RichText { html: String },
    /// An image asset of the tenant.
    Image {
        asset_id: Uuid,
        #[serde(default)]
        alt: String,
        #[serde(default)]
        caption: String,
    },
    /// A call-to-action link: a shop path (`/c/tricka`) or an `https:`/`mailto:`/`tel:` URL.
    Button { label: String, href: String },
    /// Up to 24 products, in the given order (hidden if not purchasable).
    ProductGrid {
        #[serde(default)]
        title: String,
        product_ids: Vec<Uuid>,
    },
    /// Questions and answers.
    Faq { items: Vec<FaqItem> },
}

fn h2() -> u8 {
    2
}

/// A same-shop path (`/x`, no `//host` or `/\host` smuggling, no whitespace or control
/// characters) or an absolute `https:`, `mailto:` or `tel:` URL.
pub fn href_ok(href: &str) -> bool {
    if href.is_empty()
        || href.len() > MAX_URL
        || href
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return false;
    }
    if href.starts_with('/') {
        return !href.starts_with("//");
    }
    match reqwest::Url::parse(href) {
        Ok(u) => match u.scheme() {
            "https" => u.host_str().is_some() && u.username().is_empty() && u.password().is_none(),
            "mailto" | "tel" => true,
            _ => false,
        },
        Err(_) => false,
    }
}

const CODE: &str = "invalid_blocks";

impl Block {
    /// Validates and returns the stored form (rich text sanitized, text trimmed).
    pub fn normalized(&self) -> Result<Self, Error> {
        Ok(match self {
            Self::Heading { text, level } => {
                check_text("heading", CODE, text, 1, 200)?;
                if !matches!(level, 2 | 3) {
                    return Err(invalid(CODE, "heading level must be 2 or 3"));
                }
                Self::Heading {
                    text: text.trim().to_owned(),
                    level: *level,
                }
            }
            Self::RichText { html } => {
                check_text("rich text", CODE, html, 0, MAX_HTML)?;
                Self::RichText {
                    html: sanitize_html(html),
                }
            }
            Self::Image {
                asset_id,
                alt,
                caption,
            } => {
                check_text("image alt", CODE, alt, 0, 300)?;
                check_text("image caption", CODE, caption, 0, 300)?;
                Self::Image {
                    asset_id: *asset_id,
                    alt: alt.trim().to_owned(),
                    caption: caption.trim().to_owned(),
                }
            }
            Self::Button { label, href } => {
                check_text("button label", CODE, label, 1, 100)?;
                if !href_ok(href) {
                    return Err(invalid(
                        "invalid_href",
                        "links must be a shop path (/...) or an https:, mailto: or tel: URL",
                    ));
                }
                Self::Button {
                    label: label.trim().to_owned(),
                    href: href.clone(),
                }
            }
            Self::ProductGrid { title, product_ids } => {
                check_text("product grid title", CODE, title, 0, 200)?;
                let distinct: BTreeSet<_> = product_ids.iter().collect();
                if product_ids.is_empty()
                    || product_ids.len() > MAX_GRID_PRODUCTS
                    || distinct.len() != product_ids.len()
                {
                    return Err(invalid(
                        CODE,
                        format!("a product grid lists 1-{MAX_GRID_PRODUCTS} distinct products"),
                    ));
                }
                Self::ProductGrid {
                    title: title.trim().to_owned(),
                    product_ids: product_ids.clone(),
                }
            }
            Self::Faq { items } => {
                if items.is_empty() || items.len() > MAX_FAQ_ITEMS {
                    return Err(invalid(
                        CODE,
                        format!("an FAQ has 1-{MAX_FAQ_ITEMS} questions"),
                    ));
                }
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    check_text("question", CODE, &i.question, 1, 300)?;
                    check_text("answer", CODE, &i.answer_html, 1, 10_000)?;
                    out.push(FaqItem {
                        question: i.question.trim().to_owned(),
                        answer_html: sanitize_html(&i.answer_html),
                    });
                }
                Self::Faq { items: out }
            }
        })
    }
}

/// Validates and normalizes a block list.
pub fn normalize(blocks: &[Block]) -> Result<Vec<Block>, Error> {
    if blocks.len() > MAX_BLOCKS {
        return Err(invalid(CODE, format!("at most {MAX_BLOCKS} blocks")));
    }
    blocks.iter().map(Block::normalized).collect()
}

/// Asset and product ids the blocks reference (checked against the tenant by the caller).
pub fn references(blocks: &[Block]) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut assets = vec![];
    let mut products = vec![];
    for b in blocks {
        match b {
            Block::Image { asset_id, .. } => assets.push(*asset_id),
            Block::ProductGrid { product_ids, .. } => products.extend(product_ids),
            _ => {}
        }
    }
    (assets, products)
}

/// Plain text of the blocks (excerpts, search): headings, rich text and FAQ answers.
pub fn plain_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        let part = match b {
            Block::Heading { text, .. } => text.clone(),
            Block::RichText { html } => crate::storefront::plain_excerpt(html, usize::MAX),
            Block::Faq { items } => items
                .iter()
                .map(|i| {
                    format!(
                        "{} {}",
                        i.question,
                        crate::storefront::plain_excerpt(&i.answer_html, usize::MAX)
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => continue,
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&part);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn hrefs() {
        for ok in [
            "/c/tricka",
            "/pages/kontakt?x=1",
            "https://example.com/a",
            "mailto:info@example.com",
            "tel:+420123456789",
        ] {
            assert!(href_ok(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<script>",
            "//evil.example",
            "/\\evil.example",
            "http://example.com",
            "https://user:pw@example.com",
            "vbscript:x",
            "/a b",
            "",
            "c/tricka",
        ] {
            assert!(!href_ok(bad), "{bad}");
        }
    }

    #[test]
    fn blocks_are_sanitized_and_checked() {
        let blocks: Vec<Block> = serde_json::from_value(json!([
            { "type": "heading", "text": " Doprava " },
            { "type": "rich_text", "html": "<p onclick=\"x()\">Ahoj<script>alert(1)</script></p>" },
            { "type": "faq", "items": [{ "question": "Kdy?", "answer_html": "<img src=x onerror=alert(1)>Hned" }] },
            { "type": "button", "label": "Nakupovat", "href": "/c/tricka" }
        ]))
        .unwrap();
        let out = normalize(&blocks).unwrap();
        assert_eq!(
            out[0],
            Block::Heading {
                text: "Doprava".into(),
                level: 2
            }
        );
        let s = serde_json::to_string(&out).unwrap();
        for bad in ["onclick", "<script", "onerror"] {
            assert!(!s.contains(bad), "{bad} in {s}");
        }
        let bad_link = Block::Button {
            label: "x".into(),
            href: "javascript:alert(1)".into(),
        };
        assert!(normalize(&[bad_link]).is_err());
        let bad_level = Block::Heading {
            text: "x".into(),
            level: 1,
        };
        assert!(normalize(&[bad_level]).is_err());
        let empty_grid = Block::ProductGrid {
            title: String::new(),
            product_ids: vec![],
        };
        assert!(normalize(&[empty_grid]).is_err());
        assert!(
            serde_json::from_value::<Block>(json!({ "type": "heading", "text": "x", "evil": 1 }))
                .is_err(),
            "unknown fields are refused"
        );
        assert!(serde_json::from_value::<Block>(json!({ "type": "script", "src": "x" })).is_err());
    }

    #[test]
    fn plain_text_joins_readable_parts() {
        let blocks = vec![
            Block::Heading {
                text: "Nadpis".into(),
                level: 2,
            },
            Block::RichText {
                html: "<p>Text <b>tučně</b></p>".into(),
            },
        ];
        assert_eq!(plain_text(&blocks), "Nadpis Text tučně");
    }
}
