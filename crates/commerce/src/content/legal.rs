//! Legal content (spec §14, A29): the seller's legal entity, platform-owned legal templates
//! (cs/sk/en; terms, privacy, cookies, withdrawal instructions + model form, complaints
//! procedure, review verification) installed as draft pages the merchant edits, and the
//! go-live checklist.
//!
//! The templates are a starting point, NOT legal advice: the admin says so wherever they are
//! installed or edited, and the merchant must have them reviewed by a lawyer.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Block, LegalType, PageInput, PageKind, PageStatus, PageTranslation};
use crate::audit;
use crate::catalog::check_text;
use crate::markets::invalid;

/// Shown next to every template (admin install dialog, legal page editor, API docs).
pub const NOTICE: &str = "These templates are a starting point, not legal advice. Have them \
reviewed by a lawyer before you publish them.";

/// Legal types a shop must publish before going live; plus [`LegalType::Reviews`] once it
/// shows reviews (see [`go_live`]).
pub const REQUIRED: [LegalType; 5] = [
    LegalType::Terms,
    LegalType::Privacy,
    LegalType::Cookies,
    LegalType::Withdrawal,
    LegalType::Complaints,
];

pub const TEMPLATE_LOCALES: [&str; 3] = ["cs", "sk", "en"];

// ---------------------------------------------------------------------------------------
// Legal entity

/// The seller as it must be identified to consumers (CZ Civil Code §435, §1811/§1820;
/// SK Act 108/2024 §3; GDPR art. 13). Fields may be saved incomplete; go-live lists the gaps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LegalEntity {
    /// Registered business name (obchodní firma) or the trader's full name.
    #[schema(example = "Demo s.r.o.")]
    pub company_name: String,
    /// IČO.
    #[schema(example = "12345678")]
    pub company_id: String,
    pub street: String,
    pub city: String,
    pub postal_code: String,
    /// ISO 3166-1 alpha-2.
    #[schema(example = "CZ")]
    pub country: String,
    /// Customer contact email.
    pub email: String,
    pub phone: String,
    /// Register entry ("zapsaná v OR vedeném Městským soudem v Praze, oddíl C, vložka 1").
    pub registry: String,
    /// Where returns and complaints are sent, if not the registered address.
    pub returns_address: String,
}

/// Fields go-live requires.
const REQUIRED_FIELDS: [&str; 7] = [
    "company_name",
    "company_id",
    "street",
    "city",
    "postal_code",
    "country",
    "email",
];

impl LegalEntity {
    pub fn validate(&self) -> Result<(), Error> {
        const CODE: &str = "invalid_legal_entity";
        for (name, value, max) in [
            ("company_name", &self.company_name, 200),
            ("company_id", &self.company_id, 20),
            ("street", &self.street, 200),
            ("city", &self.city, 100),
            ("postal_code", &self.postal_code, 20),
            ("country", &self.country, 2),
            ("email", &self.email, 320),
            ("phone", &self.phone, 40),
            ("registry", &self.registry, 500),
            ("returns_address", &self.returns_address, 500),
        ] {
            check_text(name, CODE, value, 0, max)?;
            if value.chars().any(char::is_control) {
                return Err(invalid(CODE, format!("{name} contains control characters")));
            }
        }
        let iso2 = self.country.len() == 2 && self.country.bytes().all(|b| b.is_ascii_uppercase());
        if !self.country.is_empty() && !iso2 {
            return Err(invalid(CODE, "country must be an ISO 3166-1 alpha-2 code"));
        }
        if !self.email.is_empty() && crate::staff::normalize_email(&self.email).is_err() {
            return Err(invalid(CODE, "email is not a valid address"));
        }
        Ok(())
    }

    fn get(&self, field: &str) -> &str {
        match field {
            "company_name" => &self.company_name,
            "company_id" => &self.company_id,
            "street" => &self.street,
            "city" => &self.city,
            "postal_code" => &self.postal_code,
            "country" => &self.country,
            "email" => &self.email,
            "phone" => &self.phone,
            "registry" => &self.registry,
            "returns_address" => &self.returns_address,
            _ => "",
        }
    }

    /// Required fields that are still empty.
    pub fn missing(&self) -> Vec<String> {
        REQUIRED_FIELDS
            .iter()
            .filter(|f| self.get(f).trim().is_empty())
            .map(|f| (*f).to_owned())
            .collect()
    }

    fn address(&self) -> String {
        [
            self.street.trim(),
            &format!("{} {}", self.postal_code.trim(), self.city.trim()),
            self.country.trim(),
        ]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LegalEntityView {
    #[serde(flatten)]
    pub entity: LegalEntity,
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn entity(tx: &mut TenantTx) -> Result<LegalEntityView, Error> {
    let row = sqlx::query!("SELECT data, updated_at FROM legal_entities")
        .fetch_optional(&mut **tx)
        .await?;
    Ok(match row {
        Some(r) => LegalEntityView {
            // Stored data was validated; unknown/old shapes fall back to empty fields.
            entity: serde_json::from_value(r.data).unwrap_or_default(),
            updated_at: Some(r.updated_at),
        },
        None => LegalEntityView {
            entity: LegalEntity::default(),
            updated_at: None,
        },
    })
}

pub async fn put_entity(
    tx: &mut TenantTx,
    actor: &str,
    input: &LegalEntity,
) -> Result<LegalEntityView, Error> {
    input.validate()?;
    let before = entity(tx).await?;
    let trimmed = LegalEntity {
        company_name: input.company_name.trim().into(),
        company_id: input.company_id.trim().into(),
        street: input.street.trim().into(),
        city: input.city.trim().into(),
        postal_code: input.postal_code.trim().into(),
        country: input.country.trim().into(),
        email: input.email.trim().into(),
        phone: input.phone.trim().into(),
        registry: input.registry.trim().into(),
        returns_address: input.returns_address.trim().into(),
    };
    let data = serde_json::to_value(&trimmed).map_err(|e| Error::Internal(e.to_string()))?;
    sqlx::query!(
        "INSERT INTO legal_entities (tenant_id, data) VALUES ($1, $2)
         ON CONFLICT (tenant_id) DO UPDATE SET data = $2, updated_at = now()",
        tx.tenant_id(),
        data
    )
    .execute(&mut **tx)
    .await?;
    let after = entity(tx).await?;
    audit::record(
        tx,
        actor,
        "legal_entity.updated",
        "legal_entity",
        None,
        &json!({ "before": before.entity, "after": after.entity }),
    )
    .await?;
    Ok(after)
}

// ---------------------------------------------------------------------------------------
// Templates

fn template(locale: &str, t: LegalType) -> Option<&'static str> {
    macro_rules! md {
        ($l:literal, $t:literal) => {
            include_str!(concat!("legal/", $l, "/", $t, ".md"))
        };
    }
    Some(match (locale, t) {
        ("cs", LegalType::Terms) => md!("cs", "terms"),
        ("cs", LegalType::Privacy) => md!("cs", "privacy"),
        ("cs", LegalType::Cookies) => md!("cs", "cookies"),
        ("cs", LegalType::Withdrawal) => md!("cs", "withdrawal"),
        ("cs", LegalType::Complaints) => md!("cs", "complaints"),
        ("cs", LegalType::Reviews) => md!("cs", "reviews"),
        ("sk", LegalType::Terms) => md!("sk", "terms"),
        ("sk", LegalType::Privacy) => md!("sk", "privacy"),
        ("sk", LegalType::Cookies) => md!("sk", "cookies"),
        ("sk", LegalType::Withdrawal) => md!("sk", "withdrawal"),
        ("sk", LegalType::Complaints) => md!("sk", "complaints"),
        ("sk", LegalType::Reviews) => md!("sk", "reviews"),
        ("en", LegalType::Terms) => md!("en", "terms"),
        ("en", LegalType::Privacy) => md!("en", "privacy"),
        ("en", LegalType::Cookies) => md!("en", "cookies"),
        ("en", LegalType::Withdrawal) => md!("en", "withdrawal"),
        ("en", LegalType::Complaints) => md!("en", "complaints"),
        ("en", LegalType::Reviews) => md!("en", "reviews"),
        _ => return None,
    })
}

/// Slug of a legal page per locale (the storefront path is `/pages/<slug>`).
pub fn slug(locale: &str, t: LegalType) -> &'static str {
    match (locale, t) {
        ("cs", LegalType::Terms) => "obchodni-podminky",
        ("cs", LegalType::Privacy) => "ochrana-osobnich-udaju",
        ("cs", LegalType::Withdrawal) => "odstoupeni-od-smlouvy",
        ("cs", LegalType::Complaints) => "reklamacni-rad",
        ("cs", LegalType::Reviews) => "overovani-recenzi",
        ("sk", LegalType::Terms) => "obchodne-podmienky",
        ("sk", LegalType::Privacy) => "ochrana-osobnych-udajov",
        ("sk", LegalType::Withdrawal) => "odstupenie-od-zmluvy",
        ("sk", LegalType::Complaints) => "reklamacny-poriadok",
        ("sk", LegalType::Reviews) => "overovanie-recenzii",
        (_, LegalType::Cookies) => "cookies",
        (_, LegalType::Terms) => "terms",
        (_, LegalType::Privacy) => "privacy",
        (_, LegalType::Withdrawal) => "withdrawal",
        (_, LegalType::Complaints) => "complaints",
        (_, LegalType::Reviews) => "review-verification",
    }
}

fn marker(locale: &str, field: &str) -> String {
    match locale {
        "en" => format!("[FILL IN: {field}]"),
        _ => format!("[DOPLŇTE: {field}]"),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Values of the template placeholders (`{{company_name}}`, ...), HTML-escaped; empty values
/// become a visible "fill in" marker.
fn values(
    locale: &str,
    e: &LegalEntity,
    shop_name: &str,
    vat_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let pick = |field: &'static str, v: &str| {
        let v = v.trim();
        if v.is_empty() {
            marker(locale, field)
        } else {
            escape(v)
        }
    };
    // Optional facts (a non-VAT-payer has no VAT id, not every trader is in a register) read
    // as a dash; only required ones leave a "fill in" marker (which go-live flags).
    let optional = |v: &str| {
        let v = v.trim();
        if v.is_empty() {
            "—".to_owned()
        } else {
            escape(v)
        }
    };
    let returns = if e.returns_address.trim().is_empty() {
        e.address()
    } else {
        e.returns_address.clone()
    };
    vec![
        ("shop_name", pick("shop_name", shop_name)),
        ("company_name", pick("company_name", &e.company_name)),
        ("company_id", pick("company_id", &e.company_id)),
        ("vat_id", optional(vat_id.unwrap_or_default())),
        ("address", pick("address", &e.address())),
        ("email", pick("email", &e.email)),
        ("phone", optional(&e.phone)),
        ("registry", optional(&e.registry)),
        ("returns_address", pick("returns_address", &returns)),
    ]
}

fn inline(text: &str) -> String {
    // `**bold**` only; everything else is literal (values are already escaped).
    let mut out = String::new();
    for (i, part) in text.split("**").enumerate() {
        if i % 2 == 1 {
            out.push_str("<strong>");
            out.push_str(part);
            out.push_str("</strong>");
        } else {
            out.push_str(part);
        }
    }
    out
}

/// The platform's markdown subset: `# Title` (first line), `## `/`### ` headings, paragraphs,
/// `- ` lists and `**bold**`. Returns the title and the blocks.
pub fn markdown_blocks(md: &str) -> (String, Vec<Block>) {
    let mut title = String::new();
    let mut blocks = vec![];
    let mut html = String::new();
    let mut para: Vec<&str> = vec![];
    let mut list: Vec<&str> = vec![];
    let flush_para = |para: &mut Vec<&str>, html: &mut String| {
        if !para.is_empty() {
            html.push_str(&format!("<p>{}</p>", inline(&para.join(" "))));
            para.clear();
        }
    };
    let flush_list = |list: &mut Vec<&str>, html: &mut String| {
        if !list.is_empty() {
            html.push_str("<ul>");
            for item in list.iter() {
                html.push_str(&format!("<li>{}</li>", inline(item)));
            }
            html.push_str("</ul>");
            list.clear();
        }
    };
    let flush_html = |html: &mut String, blocks: &mut Vec<Block>| {
        if !html.is_empty() {
            blocks.push(Block::RichText {
                html: std::mem::take(html),
            });
        }
    };
    for line in md.lines().map(str::trim_end) {
        if let Some(t) = line.strip_prefix("# ") {
            title = t.trim().to_owned();
        } else if let Some(h) = line
            .strip_prefix("## ")
            .map(|h| (h, 2))
            .or_else(|| line.strip_prefix("### ").map(|h| (h, 3)))
        {
            flush_para(&mut para, &mut html);
            flush_list(&mut list, &mut html);
            flush_html(&mut html, &mut blocks);
            blocks.push(Block::Heading {
                text: h.0.trim().to_owned(),
                level: h.1,
            });
        } else if let Some(item) = line.strip_prefix("- ") {
            flush_para(&mut para, &mut html);
            list.push(item.trim());
        } else if line.trim().is_empty() {
            flush_para(&mut para, &mut html);
            flush_list(&mut list, &mut html);
        } else if !line.starts_with("<!--") {
            flush_list(&mut list, &mut html);
            para.push(line.trim());
        }
    }
    flush_para(&mut para, &mut html);
    flush_list(&mut list, &mut html);
    flush_html(&mut html, &mut blocks);
    (title, blocks)
}

/// One legal template rendered for the tenant: title + blocks (placeholders filled in).
pub fn render(
    locale: &str,
    t: LegalType,
    e: &LegalEntity,
    shop_name: &str,
    vat_id: Option<&str>,
) -> Option<(String, Vec<Block>)> {
    let mut md = template(locale, t)?.to_owned();
    for (key, value) in values(locale, e, shop_name, vat_id) {
        md = md.replace(&format!("{{{{{key}}}}}"), &value);
    }
    Some(markdown_blocks(&md))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct InstallInput {
    /// Template locales (cs, sk, en); default: those the tenant's markets use.
    pub locales: Vec<String>,
    /// Types to install; default: all. Existing legal pages are never overwritten.
    pub types: Vec<LegalType>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InstallResult {
    /// Created draft pages (review, edit and publish them).
    pub created: Vec<Uuid>,
    /// Types that already had a page.
    pub skipped: Vec<LegalType>,
    pub notice: String,
}

/// Installs the templates as draft legal pages filled from the legal entity and tax profile.
pub async fn install(
    tx: &mut TenantTx,
    actor: &str,
    input: &InstallInput,
) -> Result<InstallResult, Error> {
    let mut locales: BTreeSet<String> = input.locales.iter().cloned().collect();
    if locales.is_empty() {
        locales = sqlx::query_scalar!("SELECT DISTINCT unnest(locales) AS \"l!\" FROM markets")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .filter(|l| TEMPLATE_LOCALES.contains(&l.as_str()))
            .collect();
    }
    if locales.is_empty() {
        locales.insert("cs".into());
    }
    if let Some(bad) = locales
        .iter()
        .find(|l| !TEMPLATE_LOCALES.contains(&l.as_str()))
    {
        return Err(invalid(
            "unsupported_locale",
            format!("no legal templates for {bad:?} (cs, sk, en)"),
        ));
    }
    let types: BTreeSet<LegalType> = if input.types.is_empty() {
        LegalType::ALL.into_iter().collect()
    } else {
        input.types.iter().copied().collect()
    };
    let existing: BTreeSet<LegalType> =
        sqlx::query_scalar!("SELECT legal_type FROM pages WHERE legal_type IS NOT NULL")
            .fetch_all(&mut **tx)
            .await?
            .into_iter()
            .flatten()
            .filter_map(|s| LegalType::parse(&s))
            .collect();
    let e = entity(tx).await?.entity;
    let shop_name = sqlx::query_scalar!(
        "SELECT name FROM platform.tenants WHERE id = $1",
        tx.tenant_id()
    )
    .fetch_one(&mut **tx)
    .await?;
    let vat_id = crate::tax::get(tx).await?.and_then(|p| p.vat_id);

    let mut created = vec![];
    let mut skipped = vec![];
    for t in types {
        if existing.contains(&t) {
            skipped.push(t);
            continue;
        }
        let mut translations = vec![];
        for l in &locales {
            let Some((title, blocks)) = render(l, t, &e, &shop_name, vat_id.as_deref()) else {
                continue;
            };
            let slug = free_slug(tx, l, slug(l, t)).await?;
            translations.push(PageTranslation {
                locale: l.clone(),
                title,
                slug,
                excerpt: String::new(),
                blocks,
                seo_title: None,
                seo_description: None,
            });
        }
        let page = super::create(
            tx,
            actor,
            &PageInput {
                kind: PageKind::Legal,
                legal_type: Some(t),
                status: PageStatus::Draft,
                published_at: None,
                image_asset_id: None,
                translations,
            },
        )
        .await?;
        created.push(page.id);
    }
    Ok(InstallResult {
        created,
        skipped,
        notice: NOTICE.into(),
    })
}

/// `base`, or `base-2`, `base-3`, ... if another page already uses it in `locale`.
async fn free_slug(tx: &mut TenantTx, locale: &str, base: &str) -> Result<String, Error> {
    let taken: BTreeSet<String> = sqlx::query_scalar!(
        "SELECT slug FROM page_translations WHERE locale = $1 AND (slug = $2 OR slug LIKE $2 || '-%')",
        locale,
        base
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();
    Ok(std::iter::once(base.to_owned())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|s| !taken.contains(s))
        .unwrap_or_else(|| base.to_owned()))
}

// ---------------------------------------------------------------------------------------
// Go-live validation (A29)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    LegalEntity,
    TaxProfile,
    LegalPages,
    Gpsr,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GoLiveCheck {
    pub code: CheckCode,
    pub ok: bool,
    /// What is missing: field names, `<legal type>:<locale>` pairs, or product names.
    pub missing: Vec<String>,
    /// Total count of missing items (lists are capped at 50).
    pub missing_count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GoLiveReport {
    pub ready: bool,
    pub checks: Vec<GoLiveCheck>,
    pub notice: String,
}

pub async fn go_live(tx: &mut TenantTx, now: DateTime<Utc>) -> Result<GoLiveReport, Error> {
    let mut checks = vec![];
    let check = |code, missing: Vec<String>, count: i64| GoLiveCheck {
        code,
        ok: count == 0,
        missing,
        missing_count: count,
    };

    let missing = entity(tx).await?.entity.missing();
    let n = i64::try_from(missing.len()).unwrap_or(i64::MAX);
    checks.push(check(CheckCode::LegalEntity, missing, n));

    let tax = crate::tax::get(tx).await?;
    checks.push(check(
        CheckCode::TaxProfile,
        if tax.is_none() {
            vec!["tax_profile".into()]
        } else {
            vec![]
        },
        i64::from(tax.is_none()),
    ));

    // Every required legal page, published, in the default locale of every market. The review
    // verification page (Omnibus) once the shop shows reviews.
    let mut required: Vec<&str> = REQUIRED.iter().map(|t| t.as_str()).collect();
    let shows_reviews = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM reviews WHERE status = 'published') AS "e!""#
    )
    .fetch_one(&mut **tx)
    .await?;
    if shows_reviews {
        required.push(LegalType::Reviews.as_str());
    }
    let pages_missing: Vec<String> = sqlx::query_scalar!(
        r#"SELECT t.legal_type || ':' || l.locale AS "missing!"
           FROM unnest($1::text[]) AS t (legal_type)
           CROSS JOIN (SELECT DISTINCT default_locale AS locale FROM markets) l
           WHERE NOT EXISTS (
               SELECT 1 FROM pages p JOIN page_translations pt ON pt.page_id = p.id
               WHERE p.legal_type = t.legal_type AND pt.locale = l.locale
                 AND p.status = 'published' AND p.published_at <= $2)
           ORDER BY 1"#,
        &required as &[&str],
        now
    )
    .fetch_all(&mut **tx)
    .await?;
    // Published but unfinished: empty, or still carrying a template "fill in" marker (e.g.
    // installed before the legal entity was complete), in any locale.
    let unfinished: Vec<String> = sqlx::query_scalar!(
        r#"SELECT p.legal_type || ':' || t.locale || ' (unfinished)' AS "missing!"
           FROM pages p JOIN page_translations t ON t.page_id = p.id
           WHERE p.legal_type = ANY($1::text[]) AND p.status = 'published'
             AND (t.blocks = '[]'::jsonb OR t.blocks::text LIKE '%[DOPLŇTE:%'
                  OR t.blocks::text LIKE '%[FILL IN:%'
                  -- The pre-WP16 review template said the shop publishes no reviews.
                  OR (p.legal_type = 'reviews'
                      AND (t.blocks::text LIKE '%nezveřejňuje recenze%'
                           OR t.blocks::text LIKE '%nezverejňuje recenzie%'
                           OR t.blocks::text LIKE '%does not publish customer reviews%')))
           ORDER BY 1"#,
        &required as &[&str]
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut pages_missing = pages_missing;
    pages_missing.extend(unfinished);
    let n = i64::try_from(pages_missing.len()).unwrap_or(i64::MAX);
    checks.push(check(CheckCode::LegalPages, pages_missing, n));

    // GPSR (EU 2023/988 art. 19): active products need at least the manufacturer.
    let gpsr_count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM products
           WHERE status = 'active' AND jsonb_typeof(gpsr -> 'manufacturer') IS DISTINCT FROM 'object'"#
    )
    .fetch_one(&mut **tx)
    .await?;
    let gpsr_missing = sqlx::query_scalar!(
        r#"SELECT coalesce((SELECT t.name FROM product_translations t WHERE t.product_id = p.id
                            ORDER BY t.locale LIMIT 1), p.id::text) AS "name!"
           FROM products p
           WHERE p.status = 'active' AND jsonb_typeof(p.gpsr -> 'manufacturer') IS DISTINCT FROM 'object'
           ORDER BY p.id LIMIT 50"#
    )
    .fetch_all(&mut **tx)
    .await?;
    checks.push(check(CheckCode::Gpsr, gpsr_missing, gpsr_count));

    Ok(GoLiveReport {
        ready: checks.iter().all(|c| c.ok),
        checks,
        notice: NOTICE.into(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn markdown_subset() {
        let (title, blocks) = markdown_blocks(
            "# Obchodní podmínky\n\n<!-- note -->\nÚvod **tučně**\npokračuje.\n\n## Článek 1\n\n- a\n- b\n\nKonec.\n### Pod",
        );
        assert_eq!(title, "Obchodní podmínky");
        assert_eq!(
            blocks,
            vec![
                Block::RichText {
                    html: "<p>Úvod <strong>tučně</strong> pokračuje.</p>".into()
                },
                Block::Heading {
                    text: "Článek 1".into(),
                    level: 2
                },
                Block::RichText {
                    html: "<ul><li>a</li><li>b</li></ul><p>Konec.</p>".into()
                },
                Block::Heading {
                    text: "Pod".into(),
                    level: 3
                },
            ]
        );
    }

    #[test]
    fn every_template_renders_with_known_placeholders_only() {
        let e = LegalEntity {
            company_name: "Demo <s.r.o.>".into(),
            company_id: "12345678".into(),
            street: "Dlouhá 1".into(),
            city: "Praha".into(),
            postal_code: "110 00".into(),
            country: "CZ".into(),
            email: "info@demo.test".into(),
            ..LegalEntity::default()
        };
        for l in TEMPLATE_LOCALES {
            for t in LegalType::ALL {
                let (title, blocks) = render(l, t, &e, "Demo", Some("CZ12345678")).unwrap();
                assert!(!title.is_empty(), "{l} {t:?} has a title");
                assert!(!blocks.is_empty(), "{l} {t:?} has content");
                let text = serde_json::to_string(&blocks).unwrap();
                assert!(
                    !text.contains("{{"),
                    "{l} {t:?}: unknown placeholder in {text}"
                );
                assert!(!text.contains("<s.r.o.>"), "values are escaped");
                // Blocks pass the same validation as merchant input.
                assert!(
                    crate::content::blocks::normalize(&blocks).is_ok(),
                    "{l} {t:?}"
                );
                assert!(crate::catalog::slug_valid(slug(l, t)));
            }
        }
    }

    #[test]
    fn empty_fields_render_markers_and_are_listed() {
        let e = LegalEntity::default();
        assert_eq!(e.missing().len(), REQUIRED_FIELDS.len());
        let (_, blocks) = render("cs", LegalType::Terms, &e, "Demo", None).unwrap();
        assert!(
            serde_json::to_string(&blocks)
                .unwrap()
                .contains("[DOPLŇTE: company_id]")
        );
        let bad = LegalEntity {
            country: "cz".into(),
            ..LegalEntity::default()
        };
        assert!(bad.validate().is_err());
        let bad = LegalEntity {
            email: "nope".into(),
            ..LegalEntity::default()
        };
        assert!(bad.validate().is_err());
    }
}
