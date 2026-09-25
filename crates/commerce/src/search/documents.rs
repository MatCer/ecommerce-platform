//! Postgres → Meilisearch documents (spec A23): one document per sellable variant and locale.
//!
//! A variant is indexed in a locale when its product is `active`, has a translation in that
//! locale and the variant has an effective price in at least one market's price list.
//! Loading is batched per set of products; [`build_product`] is the pure part.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{lang, market_key};

/// Description text beyond this many characters is not searchable (keeps documents small).
const DESCRIPTION_CHARS: usize = 2000;

#[derive(Debug, Clone)]
pub struct MarketRef {
    pub id: Uuid,
    pub code: String,
    pub price_list_id: Option<Uuid>,
    pub locales: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CategoryRef {
    pub parent_id: Option<Uuid>,
    /// locale → name
    pub names: BTreeMap<String, String>,
}

/// Tenant-wide data every document needs: markets and the category tree.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub markets: Vec<MarketRef>,
    pub categories: HashMap<Uuid, CategoryRef>,
}

impl Context {
    /// Every locale some market sells in (one index each).
    pub fn locales(&self) -> BTreeSet<String> {
        self.markets
            .iter()
            .flat_map(|m| m.locales.iter().cloned())
            .collect()
    }

    /// The category and its ancestors (bounded: the tree is acyclic, the cap guards bad data).
    fn with_ancestors(&self, id: Uuid) -> Vec<Uuid> {
        let mut out = vec![];
        let mut cur = Some(id);
        while let Some(c) = cur {
            if out.contains(&c) || out.len() >= 32 {
                break;
            }
            out.push(c);
            cur = self.categories.get(&c).and_then(|c| c.parent_id);
        }
        out
    }
}

pub async fn load_context(tx: &mut TenantTx) -> Result<Context, Error> {
    let markets =
        sqlx::query!("SELECT id, code, price_list_id, locales FROM markets ORDER BY code")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| MarketRef {
                id: r.id,
                code: r.code,
                price_list_id: r.price_list_id,
                locales: r.locales,
            })
            .collect();
    let mut categories: HashMap<Uuid, CategoryRef> =
        sqlx::query!("SELECT id, parent_id FROM categories")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .map(|r| {
                (
                    r.id,
                    CategoryRef {
                        parent_id: r.parent_id,
                        names: BTreeMap::new(),
                    },
                )
            })
            .collect();
    for r in sqlx::query!("SELECT category_id, locale, name FROM category_translations")
        .fetch_all(&mut **tx)
        .await?
    {
        if let Some(c) = categories.get_mut(&r.category_id) {
            c.names.insert(r.locale, r.name);
        }
    }
    Ok(Context {
        markets,
        categories,
    })
}

/// Everything about one product that goes into its documents.
#[derive(Debug, Clone, Default)]
pub struct ProductData {
    pub id: Uuid,
    pub active: bool,
    pub brand: Option<String>,
    pub created_at: i64,
    /// locale → (name, description html)
    pub translations: BTreeMap<String, (String, String)>,
    /// option code → value code → locale → label
    pub option_labels: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
    pub variants: Vec<VariantData>,
    pub category_ids: Vec<Uuid>,
    /// Product-level parameter values.
    pub params: Vec<ParamValue>,
}

#[derive(Debug, Clone, Default)]
pub struct VariantData {
    pub id: Uuid,
    pub sku: String,
    pub ean: Option<String>,
    pub options: BTreeMap<String, String>,
    pub is_default: bool,
    /// price list → effective gross price (minor units) now
    pub prices: HashMap<Uuid, i64>,
    pub in_stock: bool,
    /// Variant-level parameter values (override product-level ones with the same key).
    pub params: Vec<ParamValue>,
}

#[derive(Debug, Clone)]
pub struct ParamValue {
    pub key: String,
    pub filterable: bool,
    /// `{"cs": "bavlna"}` for text parameters, a number or a boolean otherwise.
    pub value: Value,
}

impl ParamValue {
    /// The facet value in `locale`: text in that locale, numbers and booleans as they are.
    fn in_locale(&self, locale: &str) -> Option<Value> {
        match &self.value {
            Value::Object(m) => m
                .get(locale)
                .and_then(Value::as_str)
                .map(|s| Value::String(s.trim().to_owned())),
            v @ (Value::Number(_) | Value::Bool(_)) => Some(v.clone()),
            _ => None,
        }
    }
}

/// Loads [`ProductData`] for `ids` (missing ids are absent from the result), prices as in
/// force at `now`.
pub async fn load_products(
    tx: &mut TenantTx,
    ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<Vec<ProductData>, Error> {
    let mut products: BTreeMap<Uuid, ProductData> = sqlx::query!(
        "SELECT id, status, brand, created_at FROM products WHERE id = ANY($1)",
        ids
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        (
            r.id,
            ProductData {
                id: r.id,
                active: r.status == "active",
                brand: r.brand,
                created_at: r.created_at.timestamp(),
                ..Default::default()
            },
        )
    })
    .collect();
    let ids: Vec<Uuid> = products.keys().copied().collect();

    for r in sqlx::query!(
        "SELECT product_id, locale, name, description_html FROM product_translations
         WHERE product_id = ANY($1)",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        if let Some(p) = products.get_mut(&r.product_id) {
            p.translations
                .insert(r.locale, (r.name, r.description_html));
        }
    }

    for r in sqlx::query!(
        r#"SELECT product_id, code, "values" AS values_json FROM product_options
           WHERE product_id = ANY($1)"#,
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let Some(p) = products.get_mut(&r.product_id) else {
            continue;
        };
        let values: Vec<OptionValueRow> =
            serde_json::from_value(r.values_json).map_err(|e| Error::Internal(e.to_string()))?;
        p.option_labels.insert(
            r.code,
            values.into_iter().map(|v| (v.code, v.name_i18n)).collect(),
        );
    }

    let mut variant_index: HashMap<Uuid, (Uuid, usize)> = HashMap::new();
    for r in sqlx::query!(
        "SELECT v.id, v.product_id, v.sku, v.ean, v.option_values, v.is_default,
                COALESCE(l.track, true) AS \"track!\",
                COALESCE(l.allow_backorder, false) AS \"allow_backorder!\",
                COALESCE(l.on_hand - l.reserved, 0) AS \"available!\"
         FROM variants v LEFT JOIN inventory_levels l ON l.variant_id = v.id
         WHERE v.product_id = ANY($1)
         ORDER BY v.product_id, v.position",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let Some(p) = products.get_mut(&r.product_id) else {
            continue;
        };
        variant_index.insert(r.id, (r.product_id, p.variants.len()));
        p.variants.push(VariantData {
            id: r.id,
            sku: r.sku,
            ean: r.ean,
            options: serde_json::from_value(r.option_values)
                .map_err(|e| Error::Internal(e.to_string()))?,
            is_default: r.is_default,
            prices: HashMap::new(),
            in_stock: !r.track || r.allow_backorder || r.available > 0,
            params: vec![],
        });
    }

    for r in sqlx::query!(
        "SELECT pi.price_list_id, pi.variant_id, pi.amount_minor
         FROM price_intervals pi JOIN variants v ON v.id = pi.variant_id
         WHERE v.product_id = ANY($1)
           AND pi.valid_from <= $2 AND (pi.valid_to IS NULL OR pi.valid_to > $2)",
        &ids,
        now
    )
    .fetch_all(&mut **tx)
    .await?
    {
        if let Some((p, i)) = variant_index.get(&r.variant_id)
            && let Some(v) = products.get_mut(p).and_then(|p| p.variants.get_mut(*i))
        {
            v.prices.insert(r.price_list_id, r.amount_minor);
        }
    }

    for r in sqlx::query!(
        "SELECT product_id, category_id FROM product_categories
         WHERE product_id = ANY($1) ORDER BY product_id, position",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        if let Some(p) = products.get_mut(&r.product_id) {
            p.category_ids.push(r.category_id);
        }
    }

    for r in sqlx::query!(
        "SELECT v.product_id, v.variant_id, p.key, p.filterable, v.value
         FROM product_parameter_values v JOIN parameters p ON p.id = v.parameter_id
         WHERE v.product_id = ANY($1) ORDER BY v.product_id, v.position",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?
    {
        let param = ParamValue {
            key: r.key,
            filterable: r.filterable,
            value: r.value,
        };
        match r.variant_id.and_then(|v| variant_index.get(&v)) {
            Some((p, i)) => {
                if let Some(v) = products.get_mut(p).and_then(|p| p.variants.get_mut(*i)) {
                    v.params.push(param);
                }
            }
            None => {
                if let Some(p) = products.get_mut(&r.product_id) {
                    p.params.push(param);
                }
            }
        }
    }

    Ok(products.into_values().collect())
}

#[derive(serde::Deserialize)]
struct OptionValueRow {
    code: String,
    #[serde(default)]
    name_i18n: BTreeMap<String, String>,
}

/// The product's documents per locale (only locales with at least one document).
pub fn build_product(ctx: &Context, p: &ProductData) -> BTreeMap<String, Vec<Value>> {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if !p.active {
        return out;
    }
    let mut category_ids: Vec<Uuid> = vec![];
    for c in &p.category_ids {
        for a in ctx.with_ancestors(*c) {
            if !category_ids.contains(&a) {
                category_ids.push(a);
            }
        }
    }
    for locale in ctx.locales() {
        let Some((name, description_html)) = p.translations.get(&locale) else {
            continue;
        };
        let mut text = String::new();
        if let Some(brand) = &p.brand {
            text.push_str(brand);
            text.push(' ');
        }
        for c in &category_ids {
            if let Some(n) = ctx.categories.get(c).and_then(|c| c.names.get(&locale)) {
                text.push_str(n);
                text.push(' ');
            }
        }
        let description: String = strip_html(description_html)
            .chars()
            .take(DESCRIPTION_CHARS)
            .collect();

        for v in &p.variants {
            let mut prices = Map::new();
            let mut markets = vec![];
            for m in &ctx.markets {
                if let Some(amount) = m.price_list_id.and_then(|l| v.prices.get(&l)) {
                    prices.insert(market_key(&m.code), json!(amount));
                    markets.push(m.code.clone());
                }
            }
            if markets.is_empty() {
                continue; // not sellable anywhere
            }
            // Variant values override product values with the same key.
            let mut params = Map::new();
            let mut param_text = String::new();
            for pv in p.params.iter().chain(&v.params) {
                let Some(value) = pv.in_locale(&locale) else {
                    continue;
                };
                if let Some(s) = value.as_str() {
                    param_text.push_str(s);
                    param_text.push(' ');
                }
                if pv.filterable {
                    params.insert(pv.key.clone(), value);
                }
            }
            let option_text: Vec<&str> = v
                .options
                .iter()
                .filter_map(|(o, val)| {
                    p.option_labels
                        .get(o)
                        .and_then(|vals| vals.get(val))
                        .and_then(|l| l.get(&locale))
                        .map(String::as_str)
                })
                .collect();
            let doc = json!({
                "id": v.id,
                "product_id": p.id,
                "skus": [v.sku],
                "eans": v.ean.iter().collect::<Vec<_>>(),
                "name_folded": lang::fold(name),
                "name_stems": lang::analyze(name, &locale),
                "variant_stems": lang::analyze(&option_text.join(" "), &locale),
                "brand": p.brand,
                "text_stems": lang::analyze(&format!("{text}{param_text}{description}"), &locale),
                "category_ids": category_ids,
                "opt": v.options,
                "param": params,
                "price": prices,
                "in_stock": v.in_stock,
                "active_in_markets": markets,
                "is_default": v.is_default,
                // ponytail: placeholder until the analytics rollups (WP14) feed sales counts.
                "popularity": 0,
                "created_at": p.created_at,
            });
            out.entry(locale.clone()).or_default().push(doc);
        }
    }
    out
}

/// Text content of sanitized HTML (tags dropped, the common entities decoded).
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (Context, Uuid, Uuid) {
        let (cz_list, sk_list) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let (root, shirts) = (Uuid::from_u128(10), Uuid::from_u128(11));
        let ctx = Context {
            markets: vec![
                MarketRef {
                    id: Uuid::from_u128(100),
                    code: "cz".into(),
                    price_list_id: Some(cz_list),
                    locales: vec!["cs".into()],
                },
                MarketRef {
                    id: Uuid::from_u128(101),
                    code: "sk-eu".into(),
                    price_list_id: Some(sk_list),
                    locales: vec!["sk".into()],
                },
            ],
            categories: HashMap::from([
                (
                    root,
                    CategoryRef {
                        parent_id: None,
                        names: [("cs".to_owned(), "Oblečení".to_owned())].into(),
                    },
                ),
                (
                    shirts,
                    CategoryRef {
                        parent_id: Some(root),
                        names: [("cs".to_owned(), "Trička".to_owned())].into(),
                    },
                ),
            ]),
        };
        (ctx, cz_list, sk_list)
    }

    fn variant(n: u128, color: &str, size: &str, prices: &[(Uuid, i64)]) -> VariantData {
        VariantData {
            id: Uuid::from_u128(n),
            sku: format!("TS-{n}"),
            ean: None,
            options: [("color".into(), color.into()), ("size".into(), size.into())].into(),
            is_default: n == 1,
            prices: prices.iter().copied().collect(),
            in_stock: n != 2,
            params: vec![],
        }
    }

    fn product(cz: Uuid, sk: Uuid) -> ProductData {
        ProductData {
            id: Uuid::from_u128(500),
            active: true,
            brand: Some("Acme".into()),
            created_at: 1,
            translations: [(
                "cs".to_owned(),
                (
                    "Tričko Basic".to_owned(),
                    "<p>Bavlněné &amp; měkké</p>".to_owned(),
                ),
            )]
            .into(),
            option_labels: [(
                "color".to_owned(),
                [(
                    "red".to_owned(),
                    [("cs".to_owned(), "Červená".to_owned())].into(),
                )]
                .into(),
            )]
            .into(),
            variants: vec![
                variant(1, "red", "m", &[(cz, 29_900), (sk, 1_290)]),
                variant(2, "blue", "xl", &[(cz, 31_900)]),
                variant(3, "red", "xl", &[]),
            ],
            category_ids: vec![Uuid::from_u128(11)],
            params: vec![
                ParamValue {
                    key: "material".into(),
                    filterable: true,
                    value: json!({ "cs": "bavlna" }),
                },
                ParamValue {
                    key: "internal".into(),
                    filterable: false,
                    value: json!({ "cs": "skladem v Brně" }),
                },
            ],
        }
    }

    #[test]
    fn one_document_per_sellable_variant_and_translated_locale() {
        let (ctx, cz, sk) = ctx();
        let docs = build_product(&ctx, &product(cz, sk));
        // No sk translation: nothing in sk. Variant 3 has no price: not indexed.
        assert_eq!(docs.keys().collect::<Vec<_>>(), ["cs"]);
        let cs = &docs["cs"];
        assert_eq!(cs.len(), 2);
        let red = &cs[0];
        assert_eq!(red["id"], json!(Uuid::from_u128(1)));
        assert_eq!(red["product_id"], json!(Uuid::from_u128(500)));
        assert_eq!(red["opt"], json!({ "color": "red", "size": "m" }));
        assert_eq!(red["price"], json!({ "cz": 29_900, "sk_eu": 1_290 }));
        assert_eq!(red["active_in_markets"], json!(["cz", "sk-eu"]));
        assert_eq!(red["in_stock"], json!(true));
        assert_eq!(cs[1]["in_stock"], json!(false));
        assert_eq!(cs[1]["active_in_markets"], json!(["cz"]));
        assert_eq!(red["skus"], json!(["TS-1"]));
        assert_eq!(red["eans"], json!([]));
    }

    #[test]
    fn documents_carry_normalized_text_ancestors_and_filterable_params() {
        let (ctx, cz, sk) = ctx();
        let red = build_product(&ctx, &product(cz, sk))["cs"][0].clone();
        assert_eq!(red["name_folded"], "tricko basic");
        assert_eq!(red["name_stems"], "trick basik");
        assert_eq!(red["variant_stems"], "cervn");
        assert_eq!(
            red["category_ids"],
            json!([Uuid::from_u128(11), Uuid::from_u128(10)])
        );
        // Only filterable parameters become facets; all text is searchable.
        assert_eq!(red["param"], json!({ "material": "bavlna" }));
        let text = red["text_stems"].as_str().unwrap_or_default();
        for stem in ["acm", "trick", "oblecn", "bavln", "mekk", "brn"] {
            assert!(text.split(' ').any(|w| w == stem), "{stem} in {text}");
        }
    }

    #[test]
    fn inactive_products_have_no_documents() {
        let (ctx, cz, sk) = ctx();
        let mut p = product(cz, sk);
        p.active = false;
        assert!(build_product(&ctx, &p).is_empty());
    }

    #[test]
    fn variant_parameters_override_product_ones() {
        let (ctx, cz, sk) = ctx();
        let mut p = product(cz, sk);
        p.variants[0].params.push(ParamValue {
            key: "material".into(),
            filterable: true,
            value: json!({ "cs": "len" }),
        });
        let docs = build_product(&ctx, &p);
        assert_eq!(docs["cs"][0]["param"]["material"], "len");
        assert_eq!(docs["cs"][1]["param"]["material"], "bavlna");
    }

    #[test]
    fn html_is_reduced_to_text() {
        assert_eq!(strip_html("<p>a &amp; b</p><br>c"), " a & b  c");
    }
}
