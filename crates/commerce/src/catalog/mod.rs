//! Catalog (spec §6, §7.1): products, variants, options, parameters, categories,
//! translations, media links, GPSR, unit price data and per-country tax categories (A3).
//!
//! Inputs are validated by pure functions before anything touches the database; the database
//! constraints (composite tenant FKs, uniques, checks) are the second line.

pub mod categories;
pub mod parameters;
pub mod products;
pub mod tax;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use platform::Error;

use crate::markets::{invalid, is_locale};

/// Translatable text: locale (`cs`, `sk`, `en-GB`) -> text.
pub type I18n = BTreeMap<String, String>;

/// Validates a translation map: known locale format, at most 20 entries, non-blank values of at
/// most `max` characters; `required` demands at least one entry.
pub(crate) fn check_i18n(
    field: &'static str,
    code: &'static str,
    value: &I18n,
    max: usize,
    required: bool,
) -> Result<(), Error> {
    if required && value.is_empty() {
        return Err(invalid(
            code,
            format!("{field} needs at least one translation"),
        ));
    }
    if value.len() > 20 {
        return Err(invalid(code, format!("{field} has more than 20 locales")));
    }
    for (locale, text) in value {
        if !is_locale(locale) {
            return Err(invalid(code, format!("{field}: invalid locale {locale:?}")));
        }
        check_text(field, code, text, 1, max)?;
    }
    Ok(())
}

/// `min..=max` characters and not only whitespace (when `min > 0`).
pub(crate) fn check_text(
    field: &'static str,
    code: &'static str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<(), Error> {
    let len = value.chars().count();
    if len > max || (min > 0 && value.trim().chars().count() < min) {
        return Err(invalid(
            code,
            format!("{field} must be {min}-{max} characters"),
        ));
    }
    Ok(())
}

pub(crate) fn check_opt_text(
    field: &'static str,
    code: &'static str,
    value: Option<&str>,
    max: usize,
) -> Result<(), Error> {
    value.map_or(Ok(()), |v| check_text(field, code, v, 1, max))
}

/// URL slug: lowercase ASCII words joined by single hyphens, at most 200 characters.
pub fn slug_valid(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 200
        && slug.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Codes used as keys (option and value codes): `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn code_valid(code: &str) -> bool {
    let b = code.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// GTIN-8, -12 (UPC-A), -13 (EAN) or -14 with a valid GS1 mod-10 check digit.
pub fn ean_valid(ean: &str) -> bool {
    if !matches!(ean.len(), 8 | 12 | 13 | 14) || !ean.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let digits: Vec<u32> = ean.bytes().map(|b| u32::from(b - b'0')).collect();
    let (body, check) = digits.split_at(digits.len() - 1);
    // Weights 3,1,3,... from the rightmost body digit.
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| if i % 2 == 0 { d * 3 } else { *d })
        .sum();
    (10 - sum % 10) % 10 == check[0]
}

static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.url_schemes(["http", "https", "mailto", "tel"].into_iter().collect());
    b
});

/// Rich text from staff or imports, reduced to safe HTML (spec §14): no scripts, event
/// handlers, styles or `javascript:` URLs; links get `rel="noopener noreferrer"`.
pub fn sanitize_html(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

pub(crate) const MAX_HTML: usize = 100_000;

/// Maps database errors on the catalog's constraints to API errors.
pub(crate) fn db_error(e: sqlx::Error) -> Error {
    let Some(db) = e.as_database_error() else {
        return e.into();
    };
    let constraint = db.constraint().unwrap_or_default().to_owned();
    match db.code().as_deref() {
        Some("23505") => {
            let (code, detail) = match constraint.as_str() {
                "variants_sku_unique" => ("sku_taken", "a variant with this SKU already exists"),
                c if c.ends_with("_locale_slug_key") => {
                    ("slug_taken", "this slug is already used in that locale")
                }
                "parameters_tenant_id_key_key" => {
                    ("key_taken", "a parameter with this key already exists")
                }
                _ => ("already_exists", "a conflicting record already exists"),
            };
            Error::Conflict {
                code,
                detail: detail.into(),
            }
        }
        // A composite (tenant_id, id) reference to a row that does not exist in this tenant.
        Some("23503") => invalid(
            "unknown_reference",
            format!("a referenced record does not exist ({constraint})"),
        ),
        _ => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eans() {
        for ok in [
            "4006381333931",
            "73513537",
            "036000291452",
            "10012345678902",
            "8594001021499",
        ] {
            assert!(ean_valid(ok), "{ok}");
        }
        for bad in [
            "4006381333932",
            "7351353",
            "abcdefghijklm",
            "",
            "400638133393100",
        ] {
            assert!(!ean_valid(bad), "{bad}");
        }
    }

    #[test]
    fn slugs_and_codes() {
        assert!(slug_valid("modre-tricko-xl"));
        for bad in [
            "",
            "Modre",
            "a--b",
            "-a",
            "a-",
            "čaj",
            "a b",
            &"a".repeat(201),
        ] {
            assert!(!slug_valid(bad), "{bad}");
        }
        assert!(code_valid("color") && code_valid("xl") && code_valid("size_eu-42"));
        for bad in ["", "Color", "_x", "a b", &"a".repeat(65)] {
            assert!(!code_valid(bad), "{bad}");
        }
    }

    #[test]
    fn sanitizer_strips_active_content() {
        let dirty = r#"<p onclick="x()">Hi<script>alert(1)</script><a href="javascript:alert(1)">x</a><a href="https://ok.test">y</a><img src=x onerror=alert(1)></p>"#;
        let clean = sanitize_html(dirty);
        for bad in ["script", "onclick", "javascript:", "onerror"] {
            assert!(!clean.contains(bad), "{bad} in {clean}");
        }
        assert!(clean.contains(r#"href="https://ok.test""#));
        assert!(clean.contains("noopener"));
    }

    #[test]
    fn i18n_rules() {
        let mut m = I18n::new();
        assert!(check_i18n("name", "invalid_name", &m, 10, true).is_err());
        assert!(check_i18n("name", "invalid_name", &m, 10, false).is_ok());
        m.insert("cs".into(), "Barva".into());
        assert!(check_i18n("name", "invalid_name", &m, 10, true).is_ok());
        m.insert("CZ".into(), "x".into());
        assert!(check_i18n("name", "invalid_name", &m, 10, true).is_err());
        m.remove("CZ");
        m.insert("sk".into(), "  ".into());
        assert!(check_i18n("name", "invalid_name", &m, 10, true).is_err());
    }
}
