//! Product feeds (spec §10.8, A28): imports from Heureka and Google Merchant XML ([`parse`],
//! [`import`]) and per-market exports for Google, Heureka and Zboží ([`export`]).

pub mod export;
pub mod import;
pub mod parse;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where a feed comes from (also the `import_mappings.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Heureka XML (`SHOP/SHOPITEM`).
    Heureka,
    /// Google Merchant RSS 2.0 / Atom with the `g:` namespace.
    Google,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heureka => "heureka",
            Self::Google => "google",
        }
    }

    pub fn parse(s: &str) -> Self {
        if s == "google" {
            Self::Google
        } else {
            Self::Heureka
        }
    }
}

/// One offer of a feed, normalized across formats.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedItem {
    /// `ITEM_ID` / `g:id`: becomes the variant SKU.
    pub item_id: String,
    /// `ITEMGROUP_ID` / `g:item_group_id`: items of a group are variants of one product.
    pub group_id: Option<String>,
    pub name: String,
    /// HTML or text; sanitized when saved.
    pub description: String,
    /// The product's URL on the old shop (becomes a redirect).
    pub url: Option<String>,
    /// Main image first.
    pub images: Vec<String>,
    /// Gross price in minor units and the currency if the feed states one.
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub ean: Option<String>,
    pub brand: Option<String>,
    /// Category path, root first (`CATEGORYTEXT` / `g:product_type`).
    pub category: Vec<String>,
    /// `PARAM` name/value pairs (Google: color, size, material, pattern, gender, age group).
    pub params: Vec<(String, String)>,
    pub stock: Option<i32>,
}

/// Parses a decimal price (`1 299,90`, `1299.90`, `299.00 CZK`) into minor units (2 decimals)
/// and the currency code if one is attached.
pub fn parse_price(raw: &str) -> Option<(i64, Option<String>)> {
    let raw = raw.trim();
    let (number, currency) = match raw.rsplit_once(char::is_whitespace) {
        Some((n, c)) if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) => {
            (n, Some(c.to_owned()))
        }
        _ => (raw, None),
    };
    let digits: String = number
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    // The last separator is the decimal one when followed by 1-2 digits; others group.
    let (int, frac) = match digits.rfind([',', '.']) {
        Some(i) if digits.len() - i - 1 <= 2 => (&digits[..i], &digits[i + 1..]),
        _ => (digits.as_str(), ""),
    };
    let int: String = int.chars().filter(|c| *c != ',' && *c != '.').collect();
    if int.is_empty() && frac.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || int.len() > 12
    {
        return None;
    }
    let units: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let cents: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 10,
        _ => frac.parse().ok()?,
    };
    Some((units.checked_mul(100)?.checked_add(cents)?, currency))
}

/// A URL slug from any text: folded to ASCII, lowercase words joined by `-`, at most `max`
/// bytes (cut at a word boundary).
pub fn slugify(text: &str, max: usize) -> String {
    let mut out = String::new();
    for word in crate::search::lang::fold(text).split_whitespace() {
        let word: String = word
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        if word.is_empty() {
            continue;
        }
        if out.len() + word.len() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices() {
        assert_eq!(parse_price("299"), Some((29_900, None)));
        assert_eq!(parse_price("1 299,90"), Some((129_990, None)));
        assert_eq!(parse_price("1299.9"), Some((129_990, None)));
        assert_eq!(
            parse_price("1,299.00 CZK"),
            Some((129_900, Some("CZK".into())))
        );
        assert_eq!(parse_price("12.50 EUR"), Some((1_250, Some("EUR".into()))));
        assert_eq!(
            parse_price("1.299"),
            Some((129_900, None)),
            "thousands separator"
        );
        for bad in ["", "abc", "12,5x", "-5", "1e9"] {
            assert_eq!(parse_price(bad), None, "{bad}");
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slugify("Tričko Basic – modrá, XL!", 200),
            "tricko-basic-modra-xl"
        );
        assert_eq!(slugify("Ťažké topánky", 200), "tazke-topanky");
        assert_eq!(slugify("aaa bbb ccc", 7), "aaa-bbb");
        assert_eq!(slugify("***", 200), "");
    }
}
