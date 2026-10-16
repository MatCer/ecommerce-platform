//! AI alt texts for product images: one vision call per image fills the market languages whose
//! alt text is empty, translating the shop's own alt texts of that image where there are any.
//! A non-empty alt text is never changed; written ones carry an `ai_marks` label
//! (field `alt_<asset id>`) until someone edits them.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use object_store::ObjectStoreExt;
use object_store::path::Path;
use platform::Error;
use platform::ai::Image;
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::fields::EntityType;
use super::marks::{self, NewMark};
use super::{Ai, Call, CallError, Outcome, prompts};
use crate::audit;
use crate::catalog::I18n;
use crate::media::AssetVariant;

pub const JOB: &str = "ai.alt_text";
pub const FEATURE: &str = "alt_text";
pub const ALT_MAX: usize = 200;
pub const DEBOUNCE_SECS: i64 = 60;
/// The Messages API limit for one image.
const IMAGE_MAX_BYTES: u64 = 5 * 1024 * 1024;
const IMAGE_WIDTH: u32 = 1024;
const ACTOR: &str = "system";

/// What a job covers: one product, or every product showing an asset that just became ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    #[serde(rename = "product_id")]
    Product(Uuid),
    #[serde(rename = "asset_id")]
    Asset(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payload {
    #[serde(flatten)]
    pub target: Target,
    /// Also images uploaded before the switch was turned on.
    #[serde(default)]
    pub backfill: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Backfill {
    /// Products queued for alt texts.
    pub queued: usize,
}

/// The `ai_marks` field of an image's alt text.
pub fn mark_field(asset_id: Uuid) -> String {
    format!("alt_{}", asset_id.simple())
}

fn is_empty(alt: Option<&String>) -> bool {
    alt.is_none_or(|s| s.trim().is_empty())
}

pub fn debounced_job(tenant_id: Uuid, payload: Payload, now: DateTime<Utc>) -> NewJob<'static> {
    let slot = now.timestamp().div_euclid(DEBOUNCE_SECS);
    let (id, mode) = match (payload.target, payload.backfill) {
        (Target::Product(id), false) => (id, "auto"),
        (Target::Product(id), true) => (id, "backfill"),
        (Target::Asset(id), _) => (id, "asset"),
    };
    let mut j = NewJob::new(JOB, json!(payload));
    j.tenant_id = Some(tenant_id);
    j.max_attempts = 3;
    j.idempotency_key = Some(format!("{JOB}:{tenant_id}:{id}:{slot}:{mode}"));
    j.run_at = DateTime::from_timestamp((slot + 1) * DEBOUNCE_SECS, 0);
    j
}

pub fn job_for_event(
    tenant_id: Option<Uuid>,
    event_type: &str,
    payload: &Value,
    now: DateTime<Utc>,
) -> Option<NewJob<'static>> {
    let id = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let target = match event_type {
        "product.created" | "product.updated" => Target::Product(id("product_id")?),
        "asset.ready" => Target::Asset(id("asset_id")?),
        _ => return None,
    };
    Some(debounced_job(
        tenant_id?,
        Payload {
            target,
            backfill: false,
        },
        now,
    ))
}

async fn since(tx: &mut TenantTx) -> Result<Option<DateTime<Utc>>, Error> {
    Ok(
        sqlx::query_scalar!("SELECT alt_texts_since FROM ai_settings")
            .fetch_optional(&mut **tx)
            .await?
            .flatten(),
    )
}

/// `POST /admin/v1/ai/alt-texts/backfill`: one job per product with a ready image missing an
/// alt text in some market language. `409 ai_alt_texts_disabled` while the switch is off.
pub async fn backfill(tx: &mut TenantTx, ai: &Ai) -> Result<Backfill, Error> {
    if since(tx).await?.is_none() {
        return Err(Error::Conflict {
            code: "ai_alt_texts_disabled",
            detail: "turn on AI alt texts first".into(),
        });
    }
    super::ensure_quota(tx, ai).await?;
    let products = sqlx::query_scalar!(
        r#"SELECT DISTINCT m.product_id FROM product_media m
           JOIN assets a ON a.id = m.asset_id AND a.status = 'ready'
           WHERE EXISTS (SELECT 1 FROM markets k, unnest(k.locales) l
                         WHERE btrim(coalesce(m.alt_i18n ->> l, ''), E' \t\r\n') = '')"#
    )
    .fetch_all(&mut **tx)
    .await?;
    let (tenant_id, now) = (tx.tenant_id(), Utc::now());
    for &product_id in &products {
        let payload = Payload {
            target: Target::Product(product_id),
            backfill: true,
        };
        queue::enqueue(&mut **tx, &debounced_job(tenant_id, payload, now)).await?;
    }
    Ok(Backfill {
        queued: products.len(),
    })
}

/// The variant sent to the model: a format the API reads, at most 5 MB, width closest to 1024.
fn pick(variants: &[AssetVariant]) -> Option<(&AssetVariant, &'static str)> {
    variants
        .iter()
        .filter(|v| v.bytes <= IMAGE_MAX_BYTES)
        .filter_map(|v| {
            let media_type = match v.format.as_str() {
                "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                "png" => "image/png",
                _ => return None,
            };
            Some((v, media_type))
        })
        .min_by_key(|(v, _)| (v.width.abs_diff(IMAGE_WIDTH), v.width))
}

/// The model output: exactly the requested locales, each a short single line of plain text.
fn validate(output: Value, locales: &[String]) -> Result<BTreeMap<String, String>, CallError> {
    let out: BTreeMap<String, String> = super::parse(output)?;
    if !out.keys().eq(locales.iter()) {
        return Err(CallError::output(format!(
            "the alt texts do not cover exactly the requested locales {locales:?}"
        )));
    }
    out.into_iter()
        .map(|(locale, text)| {
            let text = text.trim();
            if text.contains(['\n', '\r']) {
                return Err(CallError::output("an alt text spans several lines"));
            }
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !(1..=ALT_MAX).contains(&text.chars().count()) {
                return Err(CallError::output(format!(
                    "an alt text must be 1-{ALT_MAX} characters"
                )));
            }
            let lower = text.to_lowercase();
            if text.contains(['<', '>']) || lower.contains("//") || lower.contains("www.") {
                return Err(CallError::output("an alt text contains a link or markup"));
            }
            Ok((locale, text))
        })
        .collect()
}

struct ImageRow {
    asset_id: Uuid,
    alt: I18n,
    variants: Vec<AssetVariant>,
}

struct Generated {
    asset_id: Uuid,
    texts: BTreeMap<String, String>,
    model: String,
}

pub async fn run(
    db: &PgPool,
    ai: &Ai,
    storage: &Storage,
    tenant: Uuid,
    payload: Payload,
    last_attempt: bool,
) -> Result<Outcome, Error> {
    if ai.client.is_none() {
        return Ok(Outcome::Done);
    }
    let products = match payload.target {
        Target::Product(id) => vec![id],
        Target::Asset(asset_id) => {
            let mut tx = tenant_tx(db, tenant).await?;
            sqlx::query_scalar!(
                "SELECT product_id FROM product_media WHERE asset_id = $1 ORDER BY product_id",
                asset_id
            )
            .fetch_all(&mut *tx)
            .await?
        }
    };
    for product_id in products {
        let mut generated = Vec::new();
        let end = generate(
            db,
            ai,
            storage,
            tenant,
            product_id,
            payload.backfill,
            last_attempt,
            &mut generated,
        )
        .await;
        store(db, tenant, product_id, &generated).await?;
        if let Outcome::Retry(reason) = end? {
            return Ok(Outcome::Retry(reason));
        }
    }
    Ok(Outcome::Done)
}

/// Calls the model image by image; `generated` keeps what came back before a stop.
#[allow(clippy::too_many_arguments)]
async fn generate(
    db: &PgPool,
    ai: &Ai,
    storage: &Storage,
    tenant: Uuid,
    product_id: Uuid,
    backfill: bool,
    last_attempt: bool,
    generated: &mut Vec<Generated>,
) -> Result<Outcome, Error> {
    let (name, locales, images) = {
        let mut tx = tenant_tx(db, tenant).await?;
        let Some(since) = since(&mut tx).await? else {
            return Ok(Outcome::Done);
        };
        let Some(name) = super::product_name(&mut tx, product_id).await? else {
            return Ok(Outcome::Done);
        };
        let locales = super::market_locales(&mut tx).await?;
        let images = sqlx::query!(
            "SELECT m.asset_id, m.alt_i18n, a.variants FROM product_media m
             JOIN assets a ON a.id = m.asset_id
             WHERE m.product_id = $1 AND a.status = 'ready' AND ($2 OR a.created_at >= $3)
             ORDER BY m.position",
            product_id,
            backfill,
            since
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| {
            Ok(ImageRow {
                asset_id: r.asset_id,
                alt: serde_json::from_value(r.alt_i18n).map_err(internal)?,
                variants: serde_json::from_value(r.variants).map_err(internal)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
        tx.commit().await?;
        (name, locales, images)
    };
    for image in images {
        let targets: Vec<String> = locales
            .iter()
            .filter(|l| is_empty(image.alt.get(*l)))
            .cloned()
            .collect();
        if targets.is_empty() {
            continue;
        }
        let Some((variant, media_type)) = pick(&image.variants) else {
            tracing::warn!(asset_id = %image.asset_id, "no image variant the model reads");
            continue;
        };
        let bytes = match storage.public.get(&Path::from(variant.key.as_str())).await {
            Ok(r) => r.bytes().await?,
            Err(object_store::Error::NotFound { .. }) => {
                tracing::warn!(asset_id = %image.asset_id, "image variant missing in storage");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let existing: BTreeMap<&String, &String> = image
            .alt
            .iter()
            .filter(|(_, t)| !t.trim().is_empty())
            .collect();
        let task = format!(
            "Write the alt text of this product photo in these languages: {}.",
            targets.join(", ")
        );
        let result = super::call(
            db,
            ai,
            tenant,
            ACTOR,
            &Call {
                feature: FEATURE,
                system: prompts::ALT_TEXT_SYSTEM,
                task: &task,
                data: &json!({ "product": name, "existing": existing, "locales": targets }),
                schema: &prompts::alt_text_schema(&targets),
                max_tokens: 1000,
                image: Some(Image {
                    media_type,
                    data: &bytes,
                }),
            },
        )
        .await
        .and_then(|(output, model)| validate(output, &targets).map(|t| (t, model)));
        match result {
            Ok((texts, model)) => generated.push(Generated {
                asset_id: image.asset_id,
                texts,
                model,
            }),
            Err(CallError::Quota) => return Ok(Outcome::Done),
            Err(CallError::Platform(e)) => return Err(e),
            Err(e) if e.retryable() && !last_attempt => return Ok(Outcome::Retry(e.to_string())),
            Err(e) if e.retryable() => {
                tracing::warn!(%product_id, code = e.code(), error = %e, "alt text failed");
                return Ok(Outcome::Done);
            }
            Err(e) => {
                tracing::warn!(%product_id, asset_id = %image.asset_id, code = e.code(), error = %e,
                               "alt text rejected");
            }
        }
    }
    Ok(Outcome::Done)
}

/// Writes the generated texts into the locales that are still empty, under the product row
/// lock the catalog save takes, so a concurrent edit is never overwritten.
async fn store(
    db: &PgPool,
    tenant: Uuid,
    product_id: Uuid,
    generated: &[Generated],
) -> Result<(), Error> {
    if generated.is_empty() {
        return Ok(());
    }
    let mut tx = tenant_tx(db, tenant).await?;
    if sqlx::query_scalar!(
        "SELECT id FROM products WHERE id = $1 FOR UPDATE",
        product_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_none()
    {
        return Ok(());
    }
    let entity_id = product_id.to_string();
    let mut written = BTreeMap::new();
    for g in generated {
        let Some(alt) = sqlx::query_scalar!(
            "SELECT alt_i18n FROM product_media WHERE product_id = $1 AND asset_id = $2",
            product_id,
            g.asset_id
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            continue;
        };
        let mut alt: I18n = serde_json::from_value(alt).map_err(internal)?;
        let fill: BTreeMap<&String, &String> = g
            .texts
            .iter()
            .filter(|(l, _)| is_empty(alt.get(*l)))
            .collect();
        if fill.is_empty() {
            continue;
        }
        for (l, t) in &fill {
            alt.insert((*l).clone(), (*t).clone());
        }
        sqlx::query!(
            "UPDATE product_media SET alt_i18n = $3 WHERE product_id = $1 AND asset_id = $2",
            product_id,
            g.asset_id,
            json!(alt)
        )
        .execute(&mut *tx)
        .await?;
        let field = mark_field(g.asset_id);
        for (locale, text) in &fill {
            marks::record(
                &mut tx,
                EntityType::Product,
                &entity_id,
                &NewMark {
                    locale,
                    field: &field,
                    value: &json!(text),
                    feature: FEATURE,
                    model: &g.model,
                    proposal_id: None,
                    accepted_by: ACTOR,
                },
            )
            .await?;
        }
        written.insert(g.asset_id, fill);
    }
    if !written.is_empty() {
        audit::record(
            &mut tx,
            ACTOR,
            "product.alt_texts_generated",
            "product",
            Some(&entity_id),
            &json!({ "alt_texts": written }),
        )
        .await?;
        queue::publish(
            &mut *tx,
            "product.updated",
            &json!({ "product_id": product_id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn internal(e: serde_json::Error) -> Error {
    Error::Internal(e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use chrono::TimeZone;

    use super::*;

    fn locales() -> Vec<String> {
        vec!["en".into(), "sk".into()]
    }

    #[test]
    fn valid_output_is_one_collapsed_line_per_requested_locale() {
        let got = validate(
            json!({ "sk": "  Modré\ttričko   spredu ", "en": "Blue T-shirt, front view" }),
            &locales(),
        )
        .unwrap();
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            vec![
                ("en".to_owned(), "Blue T-shirt, front view".to_owned()),
                ("sk".to_owned(), "Modré tričko spredu".to_owned()),
            ]
        );
    }

    #[test]
    fn invalid_output_is_rejected() {
        let long = "a".repeat(ALT_MAX + 1);
        for bad in [
            json!({ "en": "Blue" }),
            json!({ "en": "Blue", "sk": "Modré", "cs": "Modré" }),
            json!({ "en": "Blue\nshirt", "sk": "Modré" }),
            json!({ "en": "   ", "sk": "Modré" }),
            json!({ "en": long, "sk": "Modré" }),
            json!({ "en": "<b>Blue</b>", "sk": "Modré" }),
            json!({ "en": "See https://x.example", "sk": "Modré" }),
            json!({ "en": "See //x.example", "sk": "Modré" }),
            json!({ "en": "See WWW.example.com", "sk": "Modré" }),
            json!({ "en": 1, "sk": "Modré" }),
        ] {
            let e = validate(bad.clone(), &locales()).unwrap_err();
            assert_eq!(e.code(), "ai_invalid_output", "{bad}");
            assert!(!e.to_string().contains("example"), "{e}");
        }
    }

    fn variant(width: u32, format: &str, bytes: u64) -> AssetVariant {
        AssetVariant {
            width,
            height: width,
            format: format.into(),
            key: format!("{width}.{format}"),
            bytes,
            url: String::new(),
        }
    }

    #[test]
    fn the_model_gets_a_readable_variant_closest_to_1024_px() {
        let key = |v: &[AssetVariant]| pick(v).map(|(v, t)| (v.key.clone(), t));
        assert_eq!(
            key(&[
                variant(1024, "avif", 10),
                variant(640, "webp", 10),
                variant(1280, "webp", 10),
                variant(1080, "jpeg", 10),
            ]),
            Some(("1080.jpeg".into(), "image/jpeg"))
        );
        assert_eq!(
            key(&[
                variant(1024, "png", IMAGE_MAX_BYTES + 1),
                variant(320, "png", 10)
            ]),
            Some(("320.png".into(), "image/png"))
        );
        assert_eq!(key(&[variant(1024, "avif", 10)]), None);
    }

    #[test]
    fn product_and_asset_events_debounce_per_target_and_mode() {
        let (t, p, a) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let now = Utc.with_ymd_and_hms(2026, 10, 16, 12, 1, 10).unwrap();
        let later = now + chrono::Duration::seconds(30);
        let product = json!({ "product_id": p });
        let job = job_for_event(Some(t), "product.updated", &product, now).unwrap();
        assert_eq!(job.kind, "ai.alt_text");
        assert_eq!(job.payload, json!({ "product_id": p, "backfill": false }));
        assert_eq!(
            job.run_at,
            Some(Utc.with_ymd_and_hms(2026, 10, 16, 12, 2, 0).unwrap())
        );
        assert_eq!((job.tenant_id, job.max_attempts), (Some(t), 3));
        assert_eq!(
            job_for_event(Some(t), "product.created", &product, later).map(|j| j.idempotency_key),
            Some(job.idempotency_key.clone())
        );
        let backfill = debounced_job(
            t,
            Payload {
                target: Target::Product(p),
                backfill: true,
            },
            now,
        );
        assert_ne!(backfill.idempotency_key, job.idempotency_key);
        let ready = job_for_event(Some(t), "asset.ready", &json!({ "asset_id": a }), now).unwrap();
        assert_eq!(ready.payload, json!({ "asset_id": a, "backfill": false }));
        assert_eq!(
            serde_json::from_value::<Payload>(ready.payload).unwrap(),
            Payload {
                target: Target::Asset(a),
                backfill: false
            }
        );
        assert!(job_for_event(Some(t), "product.deleted", &product, now).is_none());
        assert!(job_for_event(None, "product.updated", &product, now).is_none());
        assert!(job_for_event(Some(t), "asset.ready", &product, now).is_none());
    }
}
