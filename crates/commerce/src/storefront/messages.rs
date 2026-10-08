//! Platform message catalogs for themes and the checkout (cs, sk, en). `GET /shop` sends only
//! the active locale's catalog, so a page carries one language. Placeholders are `{name}`.
//! Every catalog has exactly the same keys (tested).

use std::collections::BTreeMap;
use std::sync::LazyLock;

type Catalog = BTreeMap<String, String>;

fn parse(json: &str) -> Catalog {
    serde_json::from_str(json).unwrap_or_default()
}

static CS: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/cs.json")));
static SK: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/sk.json")));
static EN: LazyLock<Catalog> = LazyLock::new(|| parse(include_str!("messages/en.json")));

/// The catalog for a locale tag (`cs`, `sk-SK`, ...); English for anything else.
pub fn catalog(locale: &str) -> &'static Catalog {
    match locale.split('-').next() {
        Some("cs") => &CS,
        Some("sk") => &SK,
        _ => &EN,
    }
}

/// One message; the key itself when it is missing (visible, never a panic).
pub fn text<'a>(locale: &str, key: &'a str) -> &'a str {
    catalog(locale).get(key).map_or(key, String::as_str)
}

/// A message with `{name}` placeholders filled in.
pub fn format(locale: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = text(locale, key).to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_are_complete_and_aligned() {
        assert!(CS.len() > 50);
        let keys = |c: &Catalog| c.keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&CS), keys(&SK));
        assert_eq!(keys(&CS), keys(&EN));
        assert!(CS.values().chain(SK.values()).all(|v| !v.trim().is_empty()));
    }

    #[test]
    fn popular_sort_labels() {
        for (locale, label) in [
            ("cs", "Nejoblíbenější"),
            ("sk", "Najobľúbenejšie"),
            ("en", "Most popular"),
        ] {
            assert_eq!(text(locale, "sort.popular"), label);
        }
    }

    #[test]
    fn lookup_and_format() {
        assert_eq!(text("sk-SK", "cart.add"), "Pridať do košíka");
        assert_eq!(text("de", "cart.add"), "Add to cart");
        assert_eq!(text("cs", "no.such.key"), "no.such.key");
        assert_eq!(
            format("cs", "listing.count", &[("count", "3")]),
            "3 produktů"
        );
    }
}
