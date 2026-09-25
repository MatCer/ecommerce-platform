//! Campaigns (spec §11.5): per-locale content made of blocks, rendered per recipient (the
//! personalized products block asks the recommendation engine for each subscriber, with the
//! bestseller fallback), previews and test sends, scheduling and the throttled batch sender.
//!
//! Sending (A12, A14, A20):
//! - `marketing.campaign_batch` jobs process up to [`BATCH_SIZE`] segment members per
//!   transaction, at most [`RATE_PER_MINUTE`] marketing messages per tenant and minute (a
//!   window in `email_settings`, shared by every campaign of the tenant);
//! - each member gets exactly one `campaign_sends` row (unique per campaign and subscriber) and
//!   at most one message (idempotency key `campaign:<campaign>:<subscriber>`), in the same
//!   transaction as the next batch job, so a crash repeats nothing and loses nothing;
//! - status, consent (resolved now) and suppression are checked when the batch runs and again
//!   right before SMTP; marketing mail is never resent after an uncertain outcome.
//!
//! Links: every `http(s)` link goes through `checkout.<shop>/_p/newsletter/click` with the
//! recipient's send token and an HMAC-SHA256 signature over token and target made with the
//! campaign's own `link_key`, so the redirect only ever leads where the campaign pointed.
//! The token is also the recipient's capability for one-click unsubscribe (RFC 8058) and the
//! preference page. No open tracking (privacy default).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::mail::Stream;
use platform::queue::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::segments::{self, Rules};
use super::subscribers;
use crate::capability;
use crate::catalog::{check_text, sanitize_html};
use crate::consent::{self, ConsentPurpose, Subject};
use crate::content::blocks::href_ok;
use crate::markets::invalid;
use crate::media::AssetVariant;
use crate::notifications::{self, Brand, Rendered, Stored};
use crate::recommendations::engine::{self, Target, Visitor};
use crate::recommendations::settings as rec_settings;
use crate::storefront::{self, Context, PublicUrls, cards, images};

/// Job kind: one batch of a campaign (`{"campaign_id"}`).
pub const BATCH_JOB: &str = "marketing.campaign_batch";
/// Segment members per batch transaction.
pub const BATCH_SIZE: i64 = 500;
/// Marketing messages per tenant and minute. ponytail: one platform-wide value; make it a
/// per-tenant setting when sending quotas differ per tenant.
pub const RATE_PER_MINUTE: i32 = 500;
pub const MAX_TEST_RECIPIENTS: usize = 5;
/// Test messages per tenant and hour.
pub const MAX_TEST_SENDS_PER_HOUR: i64 = 50;
pub const MAX_BLOCKS: usize = 50;
pub const MAX_GRID: usize = 12;
const CODE: &str = "invalid_content";

// ---------------------------------------------------------------------------------------
// Content

/// One block of a campaign; `type` selects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailBlock {
    Heading {
        text: String,
    },
    /// Rich text (HTML), sanitized on write.
    Text {
        html: String,
    },
    /// An image asset of the tenant, optionally linked.
    Image {
        asset_id: Uuid,
        #[serde(default)]
        alt: String,
        #[serde(default)]
        href: String,
    },
    /// A shop path (`/c/tricka`) or an `https:`/`mailto:`/`tel:` URL.
    Button {
        label: String,
        href: String,
    },
    /// Chosen products (only those still sold in the recipient's market are shown).
    ProductGrid {
        #[serde(default)]
        title: String,
        product_ids: Vec<Uuid>,
    },
    /// Per-recipient recommendations (WP17): category/brand affinity of subscribers who are
    /// customers granting `personalization`, else the shop's best sellers.
    PersonalizedProducts {
        #[serde(default)]
        title: String,
        #[serde(default = "default_limit")]
        limit: u8,
    },
}

fn default_limit() -> u8 {
    4
}

/// The content in one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocaleContent {
    pub subject: String,
    /// The inbox preview text (defaults to the subject).
    #[serde(default)]
    pub preheader: String,
    pub blocks: Vec<EmailBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CampaignInput {
    pub name: String,
    /// Recipients; `null` = every subscriber.
    #[serde(default)]
    pub segment_id: Option<Uuid>,
    /// Content per language (`cs`, `sk`, `en`); a subscriber gets their language, else the
    /// market's default language, else the first one.
    pub content: BTreeMap<String, LocaleContent>,
}

impl EmailBlock {
    fn normalized(&self) -> Result<Self, Error> {
        Ok(match self {
            Self::Heading { text } => {
                check_text("heading", CODE, text, 1, 200)?;
                Self::Heading {
                    text: text.trim().to_owned(),
                }
            }
            Self::Text { html } => {
                check_text("text", CODE, html, 1, 20_000)?;
                Self::Text {
                    html: sanitize_html(html),
                }
            }
            Self::Image {
                asset_id,
                alt,
                href,
            } => {
                check_text("image alt", CODE, alt, 0, 300)?;
                if !href.is_empty() && !href_ok(href) {
                    return Err(bad_href());
                }
                Self::Image {
                    asset_id: *asset_id,
                    alt: alt.trim().to_owned(),
                    href: href.clone(),
                }
            }
            Self::Button { label, href } => {
                check_text("button label", CODE, label, 1, 100)?;
                if !href_ok(href) {
                    return Err(bad_href());
                }
                Self::Button {
                    label: label.trim().to_owned(),
                    href: href.clone(),
                }
            }
            Self::ProductGrid { title, product_ids } => {
                check_text("product grid title", CODE, title, 0, 200)?;
                let mut distinct = product_ids.clone();
                distinct.sort_unstable();
                distinct.dedup();
                if product_ids.is_empty()
                    || product_ids.len() > MAX_GRID
                    || distinct.len() != product_ids.len()
                {
                    return Err(invalid(
                        CODE,
                        format!("a product grid lists 1-{MAX_GRID} distinct products"),
                    ));
                }
                Self::ProductGrid {
                    title: title.trim().to_owned(),
                    product_ids: product_ids.clone(),
                }
            }
            Self::PersonalizedProducts { title, limit } => {
                check_text("personalized products title", CODE, title, 0, 200)?;
                if !(2..=8).contains(limit) {
                    return Err(invalid(CODE, "personalized products: 2-8 products"));
                }
                Self::PersonalizedProducts {
                    title: title.trim().to_owned(),
                    limit: *limit,
                }
            }
        })
    }
}

fn bad_href() -> Error {
    invalid(
        "invalid_href",
        "links must be a shop path (/...) or an https:, mailto: or tel: URL",
    )
}

/// Validates the input and returns the normalized name and content.
fn validate(input: &CampaignInput) -> Result<(String, BTreeMap<String, LocaleContent>), Error> {
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("invalid_name", "name must be 1-200 characters"));
    }
    if input.content.is_empty() || input.content.len() > notifications::LOCALES.len() {
        return Err(invalid(CODE, "content in 1-3 languages"));
    }
    let mut content = BTreeMap::new();
    for (locale, c) in &input.content {
        if !notifications::LOCALES.contains(&locale.as_str()) {
            return Err(invalid(CODE, "languages are cs, sk or en"));
        }
        check_text("subject", CODE, &c.subject, 1, 200)?;
        check_text("preheader", CODE, &c.preheader, 0, 200)?;
        if c.blocks.is_empty() || c.blocks.len() > MAX_BLOCKS {
            return Err(invalid(CODE, format!("1-{MAX_BLOCKS} blocks per language")));
        }
        content.insert(
            locale.clone(),
            LocaleContent {
                subject: c.subject.trim().to_owned(),
                preheader: c.preheader.trim().to_owned(),
                blocks: c
                    .blocks
                    .iter()
                    .map(EmailBlock::normalized)
                    .collect::<Result<_, _>>()?,
            },
        );
    }
    Ok((name, content))
}

/// Referenced assets and products must belong to the tenant.
async fn check_references(
    tx: &mut TenantTx,
    content: &BTreeMap<String, LocaleContent>,
) -> Result<(), Error> {
    let mut assets = Vec::new();
    let mut products = Vec::new();
    for c in content.values() {
        for b in &c.blocks {
            match b {
                EmailBlock::Image { asset_id, .. } => assets.push(*asset_id),
                EmailBlock::ProductGrid { product_ids, .. } => products.extend(product_ids),
                _ => {}
            }
        }
    }
    let found = sqlx::query_scalar!(
        r#"SELECT (SELECT count(DISTINCT id) FROM assets WHERE id = ANY($1)) = cardinality(
                      ARRAY(SELECT DISTINCT unnest($1::uuid[])))
              AND (SELECT count(DISTINCT id) FROM products WHERE id = ANY($2)) = cardinality(
                      ARRAY(SELECT DISTINCT unnest($2::uuid[]))) AS "ok!""#,
        &assets,
        &products
    )
    .fetch_one(&mut **tx)
    .await?;
    if !found {
        return Err(invalid(
            "unknown_reference",
            "an image or product does not exist",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Campaign records

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Draft,
    Scheduled,
    Sending,
    Sent,
    Cancelled,
}

impl CampaignStatus {
    fn parse(s: &str) -> Self {
        match s {
            "scheduled" => Self::Scheduled,
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "cancelled" => Self::Cancelled,
            _ => Self::Draft,
        }
    }
}

/// Delivery numbers of a campaign.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct CampaignStats {
    /// Segment members handled so far (sent + skipped).
    pub recipients: i64,
    /// Messages created.
    pub sent: i64,
    /// Members not mailed (no consent, suppressed, unsubscribed meanwhile).
    pub skipped: i64,
    /// Accepted by the mail server (SMTP 250).
    pub accepted: i64,
    pub failed: i64,
    /// Outcome unknown (never resent, A14).
    pub uncertain: i64,
    /// Recipients who clicked at least once.
    pub clicked: i64,
    /// Clicks in total.
    pub clicks: i64,
    pub unsubscribed: i64,
    pub bounced: i64,
    pub complained: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Campaign {
    pub id: Uuid,
    pub name: String,
    pub segment_id: Option<Uuid>,
    pub content: BTreeMap<String, LocaleContent>,
    pub status: CampaignStatus,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub stats: CampaignStats,
}

struct Row {
    id: Uuid,
    name: String,
    segment_id: Option<Uuid>,
    content: Value,
    status: String,
    scheduled_at: Option<DateTime<Utc>>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn to_campaign(r: Row, stats: CampaignStats) -> Result<Campaign, Error> {
    Ok(Campaign {
        id: r.id,
        name: r.name,
        segment_id: r.segment_id,
        content: serde_json::from_value(r.content).map_err(|e| Error::Internal(e.to_string()))?,
        status: CampaignStatus::parse(&r.status),
        scheduled_at: r.scheduled_at,
        started_at: r.started_at,
        finished_at: r.finished_at,
        created_at: r.created_at,
        updated_at: r.updated_at,
        stats,
    })
}

async fn stats(tx: &mut TenantTx, ids: &[Uuid]) -> Result<HashMap<Uuid, CampaignStats>, Error> {
    Ok(sqlx::query!(
        r#"SELECT cs.campaign_id,
                  count(*) AS "recipients!",
                  count(*) FILTER (WHERE cs.status = 'sent') AS "sent!",
                  count(*) FILTER (WHERE cs.status = 'skipped') AS "skipped!",
                  count(*) FILTER (WHERE m.status = 'accepted') AS "accepted!",
                  count(*) FILTER (WHERE m.status = 'failed') AS "failed!",
                  count(*) FILTER (WHERE m.status = 'uncertain') AS "uncertain!",
                  count(*) FILTER (WHERE cs.clicked_at IS NOT NULL) AS "clicked!",
                  coalesce(sum(cs.click_count), 0)::bigint AS "clicks!",
                  count(*) FILTER (WHERE cs.unsubscribed_at IS NOT NULL) AS "unsubscribed!",
                  count(*) FILTER (WHERE cs.bounced_at IS NOT NULL) AS "bounced!",
                  count(*) FILTER (WHERE cs.complained_at IS NOT NULL) AS "complained!"
           FROM campaign_sends cs LEFT JOIN email_messages m ON m.id = cs.message_id
           WHERE cs.campaign_id = ANY($1)
           GROUP BY cs.campaign_id"#,
        ids
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        (
            r.campaign_id,
            CampaignStats {
                recipients: r.recipients,
                sent: r.sent,
                skipped: r.skipped,
                accepted: r.accepted,
                failed: r.failed,
                uncertain: r.uncertain,
                clicked: r.clicked,
                clicks: r.clicks,
                unsubscribed: r.unsubscribed,
                bounced: r.bounced,
                complained: r.complained,
            },
        )
    })
    .collect())
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Campaign, Error> {
    let r = sqlx::query_as!(
        Row,
        "SELECT id, name, segment_id, content, status, scheduled_at, started_at, finished_at,
                created_at, updated_at
         FROM campaigns WHERE id = $1",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let s = stats(tx, &[id]).await?.remove(&id).unwrap_or_default();
    to_campaign(r, s)
}

/// Newest first.
pub async fn list(tx: &mut TenantTx) -> Result<Vec<Campaign>, Error> {
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, name, segment_id, content, status, scheduled_at, started_at, finished_at,
                created_at, updated_at
         FROM campaigns ORDER BY id DESC LIMIT 200"
    )
    .fetch_all(&mut **tx)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut all = stats(tx, &ids).await?;
    rows.into_iter()
        .map(|r| {
            let s = all.remove(&r.id).unwrap_or_default();
            to_campaign(r, s)
        })
        .collect()
}

fn fk_error(e: sqlx::Error) -> Error {
    if e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23503"))
    {
        return invalid("unknown_segment", "the segment does not exist");
    }
    e.into()
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &CampaignInput,
) -> Result<Campaign, Error> {
    let (name, content) = validate(input)?;
    check_references(tx, &content).await?;
    let content_json =
        serde_json::to_value(&content).map_err(|e| Error::Internal(e.to_string()))?;
    let key: [u8; 32] = rand::random();
    let id = sqlx::query_scalar!(
        "INSERT INTO campaigns (tenant_id, name, segment_id, content, link_key, created_by)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        tx.tenant_id(),
        name,
        input.segment_id,
        content_json,
        &key[..],
        actor
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(fk_error)?;
    crate::audit::record(
        tx,
        actor,
        "campaign.create",
        "campaign",
        Some(&id.to_string()),
        &json!({ "name": name, "segment_id": input.segment_id }),
    )
    .await?;
    get(tx, id).await
}

/// Edits a draft (sent or scheduled campaigns are frozen; cancel a scheduled one first).
pub async fn update(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    input: &CampaignInput,
) -> Result<Campaign, Error> {
    let (name, content) = validate(input)?;
    check_references(tx, &content).await?;
    let content_json =
        serde_json::to_value(&content).map_err(|e| Error::Internal(e.to_string()))?;
    let status = lock_status(tx, id).await?;
    if status != CampaignStatus::Draft {
        return Err(not_editable());
    }
    sqlx::query!(
        "UPDATE campaigns SET name = $2, segment_id = $3, content = $4, updated_at = now()
         WHERE id = $1",
        id,
        name,
        input.segment_id,
        content_json
    )
    .execute(&mut **tx)
    .await
    .map_err(fk_error)?;
    crate::audit::record(
        tx,
        actor,
        "campaign.update",
        "campaign",
        Some(&id.to_string()),
        &json!({ "name": name, "segment_id": input.segment_id }),
    )
    .await?;
    get(tx, id).await
}

fn not_editable() -> Error {
    Error::Conflict {
        code: "campaign_not_draft",
        detail: "only draft campaigns can be changed".into(),
    }
}

async fn lock_status(tx: &mut TenantTx, id: Uuid) -> Result<CampaignStatus, Error> {
    let s = sqlx::query_scalar!("SELECT status FROM campaigns WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    Ok(CampaignStatus::parse(&s))
}

pub async fn delete(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    if lock_status(tx, id).await? != CampaignStatus::Draft {
        return Err(not_editable());
    }
    sqlx::query!("DELETE FROM campaigns WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    crate::audit::record(
        tx,
        actor,
        "campaign.delete",
        "campaign",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    Ok(())
}

/// Schedules a draft (or moves a scheduled campaign) to `at` (`None` or the past = now). The
/// first batch job is enqueued for that moment in the same transaction.
pub async fn schedule(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Campaign, Error> {
    let status = lock_status(tx, id).await?;
    if !matches!(status, CampaignStatus::Draft | CampaignStatus::Scheduled) {
        return Err(Error::Conflict {
            code: "campaign_not_schedulable",
            detail: "only draft or scheduled campaigns can be scheduled".into(),
        });
    }
    let at = at.filter(|a| *a > now).unwrap_or(now);
    sqlx::query!(
        "UPDATE campaigns SET status = 'scheduled', scheduled_at = $2, updated_at = now()
         WHERE id = $1",
        id,
        at
    )
    .execute(&mut **tx)
    .await?;
    let mut job = batch_job(tx.tenant_id(), id);
    job.run_at = Some(at);
    job.idempotency_key = Some(format!("{BATCH_JOB}:{id}:start:{}", at.timestamp_micros()));
    queue::enqueue(&mut **tx, &job).await?;
    crate::audit::record(
        tx,
        actor,
        "campaign.schedule",
        "campaign",
        Some(&id.to_string()),
        &json!({ "scheduled_at": at }),
    )
    .await?;
    get(tx, id).await
}

/// Stops a scheduled or sending campaign (batches already sent stay sent).
pub async fn cancel(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<Campaign, Error> {
    let status = lock_status(tx, id).await?;
    if !matches!(status, CampaignStatus::Scheduled | CampaignStatus::Sending) {
        return Err(Error::Conflict {
            code: "campaign_not_active",
            detail: "only scheduled or sending campaigns can be cancelled".into(),
        });
    }
    sqlx::query!(
        "UPDATE campaigns SET status = 'cancelled', finished_at = now(), updated_at = now()
         WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    crate::audit::record(
        tx,
        actor,
        "campaign.cancel",
        "campaign",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    get(tx, id).await
}

fn batch_job(tenant: Uuid, id: Uuid) -> NewJob<'static> {
    let mut job = NewJob::new(BATCH_JOB, json!({ "campaign_id": id }));
    job.tenant_id = Some(tenant);
    job
}

// ---------------------------------------------------------------------------------------
// Rendering

/// Where a recipient's links point.
#[derive(Clone)]
struct Links {
    /// Click tracking: the send token and the campaign's key; `None` = direct links (preview,
    /// test sends).
    track: Option<(String, Vec<u8>)>,
    /// `checkout.<shop>/_p/newsletter/click`.
    click_base: String,
    unsubscribe_url: String,
    preferences_url: String,
}

type HmacSha256 = Hmac<Sha256>;
/// Signature length in bytes (hex: twice as many characters).
const SIG_BYTES: usize = 16;

fn mac(key: &[u8], token: &str, url: &str) -> Result<HmacSha256, Error> {
    let mut m =
        <HmacSha256 as KeyInit>::new_from_slice(key).map_err(|e| Error::Internal(e.to_string()))?;
    m.update(token.as_bytes());
    m.update(b"\n");
    m.update(url.as_bytes());
    Ok(m)
}

/// The signature of a tracked link to `url` for `token` (hex, 128 bits).
pub fn sign(key: &[u8], token: &str, url: &str) -> Result<String, Error> {
    let tag = mac(key, token, url)?.finalize().into_bytes();
    Ok(hex::encode(&tag[..SIG_BYTES]))
}

fn verify(key: &[u8], token: &str, url: &str, sig: &str) -> Result<bool, Error> {
    let Ok(bytes) = hex::decode(sig) else {
        return Ok(false);
    };
    if bytes.len() != SIG_BYTES {
        return Ok(false);
    }
    Ok(mac(key, token, url)?.verify_truncated_left(&bytes).is_ok())
}

impl Links {
    /// The href to put into the email for `url` (already absolute or `mailto:`/`tel:`).
    fn href(&self, url: &str) -> Result<String, Error> {
        let trackable = url.starts_with("https://") || url.starts_with("http://");
        match &self.track {
            Some((token, key)) if trackable => {
                let sig = sign(key, token, url)?;
                reqwest::Url::parse_with_params(
                    &self.click_base,
                    &[("t", token.as_str()), ("u", url), ("s", sig.as_str())],
                )
                .map(String::from)
                .map_err(|e| Error::Internal(e.to_string()))
            }
            _ => Ok(url.to_owned()),
        }
    }
}

/// A shop path (`/c/x`) as an absolute URL in the recipient's language; anything else as is.
fn absolute(ctx: &Context, href: &str) -> String {
    if href.starts_with('/') && !href.starts_with("//") {
        ctx.page_url(href)
    } else {
        href.to_owned()
    }
}

/// Rich text for email: links made absolute and tracked (re-sanitized while rewriting).
fn text_html(ctx: &Context, links: &Links, html: &str) -> String {
    let (ctx, links) = (ctx.clone(), links.clone());
    let mut b = ammonia::Builder::default();
    b.url_schemes(["http", "https", "mailto", "tel"].into_iter().collect());
    b.attribute_filter(move |el, attr, value| {
        if el == "a" && attr == "href" {
            let target = absolute(&ctx, value);
            Some(Cow::Owned(links.href(&target).unwrap_or(target)))
        } else {
            Some(Cow::Borrowed(value))
        }
    });
    b.clean(html).to_string()
}

/// The recipient-specific parts of a render.
#[derive(Debug, Default)]
struct Recipient {
    customer_id: Option<Uuid>,
    /// Set while rendering when the customer's own signals shaped the content (A20: the
    /// `personalization` consent is checked again before SMTP).
    personalized: std::sync::atomic::AtomicBool,
}

struct Renderer<'a> {
    ctx: &'a Context,
    brand: &'a Brand,
    settings: &'a rec_settings::RecommendationSettings,
    /// Image variants per asset (loaded once per campaign).
    assets: &'a HashMap<Uuid, Vec<AssetVariant>>,
}

#[derive(Serialize)]
struct Item {
    name: String,
    url: String,
    image: Option<String>,
    price: String,
}

impl Renderer<'_> {
    fn item(&self, links: &Links, c: &cards::ProductCard) -> Result<Item, Error> {
        Ok(Item {
            name: c.name.clone(),
            url: links.href(&self.ctx.page_url(&format!("/p/{}", c.slug)))?,
            image: c.images.first().map(|i| self.ctx.url(&i.src)),
            price: c.price.price.formatted.clone(),
        })
    }

    async fn products(
        &self,
        tx: &mut TenantTx,
        links: &Links,
        block: &EmailBlock,
        who: &Recipient,
    ) -> Result<Vec<Item>, Error> {
        let found: Vec<cards::ProductCard> = match block {
            EmailBlock::ProductGrid { product_ids, .. } => {
                let mut by_id: HashMap<Uuid, cards::ProductCard> =
                    cards::cards(tx, self.ctx, product_ids)
                        .await?
                        .into_iter()
                        .map(|c| (c.id, c))
                        .collect();
                product_ids
                    .iter()
                    .filter_map(|id| by_id.remove(id))
                    .filter(|c| c.stock.purchasable())
                    .collect()
            }
            EmailBlock::PersonalizedProducts { limit, .. } => {
                // A20: personal signals only while the customer grants `personalization` now.
                let mut visitor = Visitor::default();
                if let Some(customer) = who.customer_id
                    && consent::current(
                        tx,
                        &Subject::Customer(customer),
                        ConsentPurpose::Personalization,
                    )
                    .await?
                {
                    let affinity = engine::customer_affinity(tx, customer).await?;
                    if !affinity.is_empty() {
                        who.personalized
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    visitor.personalization = true;
                    visitor.affinity = Some(affinity);
                }
                engine::recommend(
                    tx,
                    self.ctx,
                    self.settings,
                    &Target::Home,
                    &visitor,
                    usize::from(*limit),
                )
                .await?
                .items
                .into_iter()
                .map(|i| i.product)
                .collect()
            }
            _ => Vec::new(),
        };
        found.iter().map(|c| self.item(links, c)).collect()
    }

    async fn render(
        &self,
        tx: &mut TenantTx,
        content: &LocaleContent,
        links: &Links,
        who: &Recipient,
    ) -> Result<Rendered, Error> {
        let mut blocks = Vec::with_capacity(content.blocks.len());
        for b in &content.blocks {
            let v = match b {
                EmailBlock::Heading { text } => json!({ "type": "heading", "text": text }),
                EmailBlock::Text { html } => json!({
                    "type": "text",
                    "html": text_html(self.ctx, links, html),
                    "plain": storefront::plain_excerpt(html, usize::MAX),
                }),
                EmailBlock::Image {
                    asset_id,
                    alt,
                    href,
                } => {
                    let Some(img) = self
                        .assets
                        .get(asset_id)
                        .and_then(|v| images::from_variants(v, alt.clone()))
                    else {
                        continue; // not processed (yet): leave it out
                    };
                    let href = if href.is_empty() {
                        None
                    } else {
                        Some(links.href(&absolute(self.ctx, href))?)
                    };
                    json!({ "type": "image", "src": self.ctx.url(&img.src), "alt": alt, "href": href })
                }
                EmailBlock::Button { label, href } => json!({
                    "type": "button",
                    "label": label,
                    "href": links.href(&absolute(self.ctx, href))?,
                }),
                EmailBlock::ProductGrid { title, .. }
                | EmailBlock::PersonalizedProducts { title, .. } => {
                    let items = self.products(tx, links, b, who).await?;
                    if items.is_empty() {
                        continue;
                    }
                    let rows: Vec<&[Item]> = items.chunks(2).collect();
                    json!({ "type": "products", "title": title, "rows": rows })
                }
            };
            blocks.push(v);
        }
        let preview = if content.preheader.is_empty() {
            &content.subject
        } else {
            &content.preheader
        };
        let mut ctx = serde_json::Map::new();
        ctx.insert("locale".into(), json!(self.ctx.locale));
        ctx.insert("subject".into(), json!(content.subject));
        ctx.insert("preview".into(), json!(preview));
        ctx.insert("overrides".into(), json!({}));
        ctx.insert(
            "brand".into(),
            serde_json::to_value(self.brand).map_err(|e| Error::Internal(e.to_string()))?,
        );
        ctx.insert("blocks".into(), Value::Array(blocks));
        ctx.insert("unsubscribe_url".into(), json!(links.unsubscribe_url));
        ctx.insert("preferences_url".into(), json!(links.preferences_url));
        notifications::render_files("campaign", &ctx, content.subject.clone())
    }
}

async fn load_assets(
    tx: &mut TenantTx,
    content: &BTreeMap<String, LocaleContent>,
) -> Result<HashMap<Uuid, Vec<AssetVariant>>, Error> {
    let ids: Vec<Uuid> = content
        .values()
        .flat_map(|c| &c.blocks)
        .filter_map(|b| match b {
            EmailBlock::Image { asset_id, .. } => Some(*asset_id),
            _ => None,
        })
        .collect();
    let rows = sqlx::query!(
        "SELECT id, variants FROM assets WHERE id = ANY($1) AND status = 'ready'",
        &ids
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            serde_json::from_value::<Vec<AssetVariant>>(r.variants)
                .ok()
                .map(|v| (r.id, v))
        })
        .collect())
}

/// The content for a recipient: their language, else the market's default, else the first.
fn pick<'a>(
    content: &'a BTreeMap<String, LocaleContent>,
    locale: &str,
    market_default: &str,
) -> Option<(&'a String, &'a LocaleContent)> {
    content
        .get_key_value(locale)
        .or_else(|| content.get_key_value(market_default))
        .or_else(|| content.iter().next())
}

struct Loaded {
    segment: Option<Rules>,
    content: BTreeMap<String, LocaleContent>,
    link_key: Vec<u8>,
}

async fn load(
    tx: &mut TenantTx,
    id: Uuid,
) -> Result<(CampaignStatus, Option<DateTime<Utc>>, Loaded), Error> {
    let r = sqlx::query!(
        "SELECT c.status, c.scheduled_at, c.content, c.link_key, s.rules AS \"rules?\"
         FROM campaigns c LEFT JOIN segments s ON s.id = c.segment_id
         WHERE c.id = $1 FOR UPDATE OF c",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let segment = match r.rules {
        Some(v) => Some(
            serde_json::from_value::<Rules>(v)
                .map_err(|e| Error::Internal(e.to_string()))?
                .normalized()?,
        ),
        None => None,
    };
    Ok((
        CampaignStatus::parse(&r.status),
        r.scheduled_at,
        Loaded {
            segment,
            content: serde_json::from_value(r.content)
                .map_err(|e| Error::Internal(e.to_string()))?,
            link_key: r.link_key,
        },
    ))
}

/// Renders the campaign as `subscriber_id` would get it (their language, market and
/// personalized products), or for a language of the default market; links are not tracked.
pub async fn preview(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    id: Uuid,
    subscriber_id: Option<Uuid>,
    locale: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Rendered, Error> {
    let (_, _, c) = load(tx, id).await?;
    let (market, lang, who) = match subscriber_id {
        Some(s) => {
            let s = subscribers::get(tx, s).await?;
            (
                s.market_id,
                s.locale,
                Recipient {
                    customer_id: s.customer_id,
                    ..Recipient::default()
                },
            )
        }
        None => (
            default_market(tx).await?,
            locale.unwrap_or("cs").to_owned(),
            Recipient::default(),
        ),
    };
    let ctx = storefront::context(tx, urls, market, Some(&lang), now).await?;
    let (_, content) = pick(&c.content, &lang, &ctx.market.default_locale)
        .ok_or_else(|| invalid(CODE, "the campaign has no content"))?;
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    let settings = rec_settings::get(tx).await?;
    let assets = load_assets(tx, &c.content).await?;
    let links = Links {
        track: None,
        click_base: String::new(),
        unsubscribe_url: ctx.checkout_url("/newsletter"),
        preferences_url: ctx.checkout_url("/newsletter"),
    };
    Renderer {
        ctx: &ctx,
        brand: &brand,
        settings: &settings,
        assets: &assets,
    }
    .render(tx, content, &links, &who)
    .await
}

async fn default_market(tx: &mut TenantTx) -> Result<Uuid, Error> {
    sqlx::query_scalar!("SELECT id FROM markets ORDER BY is_default DESC, code LIMIT 1")
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| invalid("no_market", "the shop has no market"))
}

/// Sends the campaign to up to 5 of the shop's staff addresses now (marketing stream, subject marked, no
/// tracking, not counted in the stats). Returns how many were queued.
pub async fn test_send(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    actor: &str,
    id: Uuid,
    emails: &[String],
    locale: Option<&str>,
    now: DateTime<Utc>,
) -> Result<usize, Error> {
    if emails.is_empty() || emails.len() > MAX_TEST_RECIPIENTS {
        return Err(invalid(
            "invalid_recipients",
            format!("1-{MAX_TEST_RECIPIENTS} addresses"),
        ));
    }
    let emails = emails
        .iter()
        .map(|e| crate::staff::normalize_email(e))
        .collect::<Result<Vec<_>, _>>()?;
    // A test send is for the shop's own people: only staff addresses of this tenant, and a
    // quota, so it can never become a way around consent, suppression or the throttle.
    let staff = sqlx::query_scalar!(
        r#"SELECT count(DISTINCT lower(email)) AS "n!" FROM staff_members
           WHERE lower(email) = ANY($1)"#,
        &emails
    )
    .fetch_one(&mut **tx)
    .await?;
    let mut distinct = emails.clone();
    distinct.sort();
    distinct.dedup();
    if usize::try_from(staff).unwrap_or(0) != distinct.len() {
        return Err(invalid(
            "not_staff",
            "test emails go only to staff addresses of this shop",
        ));
    }
    let recent = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM email_messages
           WHERE template = 'campaign_test' AND created_at > now() - interval '1 hour'"#
    )
    .fetch_one(&mut **tx)
    .await?;
    if recent + i64::try_from(distinct.len()).unwrap_or(0) > MAX_TEST_SENDS_PER_HOUR {
        return Err(Error::TooManyRequests {
            code: "too_many_test_sends",
        });
    }
    let emails = distinct;
    let mut r = preview(tx, urls, id, None, locale, now).await?;
    r.subject = format!("[TEST] {}", r.subject);
    let lang = locale.unwrap_or("cs");
    for to in &emails {
        notifications::store(
            tx,
            Stored {
                stream: Stream::Marketing,
                template: "campaign_test",
                to,
                locale: lang,
                rendered: &r,
                idempotency_key: &format!("campaign_test:{id}:{}", Uuid::now_v7()),
                sensitive: false,
                subscriber_id: None,
                list_unsubscribe: None,
            },
        )
        .await?;
    }
    crate::audit::record(
        tx,
        actor,
        "campaign.test_send",
        "campaign",
        Some(&id.to_string()),
        &json!({ "recipients": emails.len() }),
    )
    .await?;
    Ok(emails.len())
}

// ---------------------------------------------------------------------------------------
// Sending

/// What a batch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub sent: usize,
    pub skipped: usize,
    /// The campaign finished (or is not sendable any more).
    pub done: bool,
}

struct Member {
    id: Uuid,
    email: String,
    locale: String,
    market_id: Uuid,
    customer_id: Option<Uuid>,
}

/// Runs one batch of campaign `id` (the `marketing.campaign_batch` job): starts a due
/// scheduled campaign, takes the next members within the tenant's rate window, sends or skips
/// each, and enqueues the next batch in the same transaction.
pub async fn run_batch(
    db: &PgPool,
    urls: &PublicUrls,
    tenant: Uuid,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<Batch, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let (status, scheduled_at, c) = match load(&mut tx, id).await {
        Ok(x) => x,
        Err(Error::NotFound) => {
            return Ok(Batch {
                sent: 0,
                skipped: 0,
                done: true,
            });
        }
        Err(e) => return Err(e),
    };
    let idle = Batch {
        sent: 0,
        skipped: 0,
        done: true,
    };
    match status {
        // A job of an earlier schedule that was moved later: the newer job starts it.
        CampaignStatus::Scheduled if scheduled_at.is_some_and(|at| at > now) => return Ok(idle),
        CampaignStatus::Scheduled => {
            sqlx::query!(
                "UPDATE campaigns SET status = 'sending', started_at = coalesce(started_at, $2),
                     updated_at = now()
                 WHERE id = $1",
                id,
                now
            )
            .execute(&mut *tx)
            .await?;
        }
        CampaignStatus::Sending => {}
        _ => return Ok(idle),
    }

    // Per-tenant rate window.
    sqlx::query!(
        "INSERT INTO email_settings (tenant_id) VALUES ($1) ON CONFLICT DO NOTHING",
        tenant
    )
    .execute(&mut *tx)
    .await?;
    let w = sqlx::query!(
        "SELECT throttle_window_start, throttle_window_count FROM email_settings FOR UPDATE"
    )
    .fetch_one(&mut *tx)
    .await?;
    let (start, used) = match w.throttle_window_start {
        Some(s) if s + Duration::minutes(1) > now => (s, w.throttle_window_count),
        _ => (now, 0),
    };
    let budget = i64::from(RATE_PER_MINUTE - used).min(BATCH_SIZE);
    if budget <= 0 {
        let at = start + Duration::minutes(1);
        let mut job = batch_job(tenant, id);
        job.run_at = Some(at);
        job.idempotency_key = Some(format!("{BATCH_JOB}:{id}:wait:{}", at.timestamp_micros()));
        queue::enqueue(&mut *tx, &job).await?;
        tx.commit().await?;
        return Ok(Batch {
            sent: 0,
            skipped: 0,
            done: false,
        });
    }

    let rules = c.segment.clone().unwrap_or_default();
    let mut qb = segments::members(
        "s.id, s.email, s.locale, s.market_id, s.customer_id",
        &rules,
        now,
    );
    qb.push(" AND NOT EXISTS (SELECT 1 FROM campaign_sends x WHERE x.subscriber_id = s.id AND x.campaign_id = ")
        .push_bind(id)
        .push(") ORDER BY s.id LIMIT ")
        .push_bind(budget);
    let members: Vec<Member> = qb
        .build_query_as::<(Uuid, String, String, Uuid, Option<Uuid>)>()
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(id, email, locale, market_id, customer_id)| Member {
            id,
            email,
            locale,
            market_id,
            customer_id,
        })
        .collect();

    let settings = rec_settings::get(&mut tx).await?;
    let assets = load_assets(&mut tx, &c.content).await?;
    let mut contexts: HashMap<(Uuid, String), Option<(Context, Brand)>> = HashMap::new();
    let (mut sent, mut skipped) = (0, 0);
    for m in &members {
        let reason = match subscribers::may_receive(&mut tx, m.id).await? {
            Some(r) => Some(r),
            None if notifications::is_suppressed(&mut tx, &m.email, Stream::Marketing).await? => {
                Some("suppressed")
            }
            None => None,
        };
        let outcome = match reason {
            Some(r) => Err(r),
            None => {
                send_one(
                    &mut tx,
                    urls,
                    now,
                    id,
                    &c,
                    &settings,
                    &assets,
                    &mut contexts,
                    m,
                )
                .await?
            }
        };
        match outcome {
            Ok(()) => sent += 1,
            Err(reason) => {
                skipped += 1;
                sqlx::query!(
                    "INSERT INTO campaign_sends (tenant_id, campaign_id, subscriber_id, status,
                                                 skip_reason)
                     VALUES ($1, $2, $3, 'skipped', $4)
                     ON CONFLICT ON CONSTRAINT campaign_sends_once DO NOTHING",
                    tenant,
                    id,
                    m.id,
                    reason
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    sqlx::query!(
        "UPDATE email_settings SET throttle_window_start = $1, throttle_window_count = $2,
             updated_at = now()",
        start,
        used + i32::try_from(sent).unwrap_or(i32::MAX)
    )
    .execute(&mut *tx)
    .await?;
    let done = i64::try_from(members.len()).unwrap_or(0) < budget;
    if done {
        sqlx::query!(
            "UPDATE campaigns SET status = 'sent', finished_at = $2, updated_at = now()
             WHERE id = $1",
            id,
            now
        )
        .execute(&mut *tx)
        .await?;
    } else {
        let handled = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM campaign_sends WHERE campaign_id = $1"#,
            id
        )
        .fetch_one(&mut *tx)
        .await?;
        let mut job = batch_job(tenant, id);
        job.idempotency_key = Some(format!("{BATCH_JOB}:{id}:after:{handled}"));
        queue::enqueue(&mut *tx, &job).await?;
    }
    tx.commit().await?;
    Ok(Batch {
        sent,
        skipped,
        done,
    })
}

/// Renders and queues the message of one member (`Err(reason)` = skipped).
#[allow(clippy::too_many_arguments)]
async fn send_one(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    now: DateTime<Utc>,
    campaign: Uuid,
    c: &Loaded,
    settings: &rec_settings::RecommendationSettings,
    assets: &HashMap<Uuid, Vec<AssetVariant>>,
    contexts: &mut HashMap<(Uuid, String), Option<(Context, Brand)>>,
    m: &Member,
) -> Result<Result<(), &'static str>, Error> {
    let key = (m.market_id, m.locale.clone());
    if !contexts.contains_key(&key) {
        let loaded = match storefront::context(tx, urls, m.market_id, Some(&m.locale), now).await {
            Ok(ctx) => {
                let brand = Brand::load(tx, ctx.base_url.clone()).await?;
                Some((ctx, brand))
            }
            // A market without a published domain has no shop to link to.
            Err(Error::NotFound | Error::Forbidden { .. }) => None,
            Err(e) => return Err(e),
        };
        contexts.insert(key.clone(), loaded);
    }
    let Some(Some((ctx, brand))) = contexts.get(&key) else {
        return Ok(Err("no_shop_domain"));
    };
    let Some((lang, content)) = pick(&c.content, &m.locale, &ctx.market.default_locale) else {
        return Ok(Err("no_content"));
    };
    let minted = capability::mint();
    let links = Links {
        track: Some((minted.token.clone(), c.link_key.clone())),
        click_base: ctx.checkout_url("/_p/newsletter/click"),
        unsubscribe_url: ctx.checkout_url(&format!("/newsletter?t={}", minted.token)),
        preferences_url: ctx.checkout_url(&format!("/newsletter?t={}", minted.token)),
    };
    let who = Recipient {
        customer_id: m.customer_id,
        ..Recipient::default()
    };
    let rendered = match (Renderer {
        ctx,
        brand,
        settings,
        assets,
    })
    .render(tx, content, &links, &who)
    .await
    {
        Ok(r) => r,
        // A template/content problem must not stall the whole campaign: skip and log.
        Err(Error::Internal(e)) => {
            tracing::error!(%campaign, subscriber = %m.id, error = %e, "campaign render failed");
            return Ok(Err("render_failed"));
        }
        Err(e) => return Err(e),
    };
    let one_click = ctx.checkout_url(&format!("/_p/newsletter/unsubscribe?t={}", minted.token));
    let message = notifications::store(
        tx,
        Stored {
            stream: Stream::Marketing,
            template: "campaign",
            to: &m.email,
            locale: lang,
            rendered: &rendered,
            idempotency_key: &format!("campaign:{campaign}:{}", m.id),
            sensitive: false,
            subscriber_id: Some(m.id),
            list_unsubscribe: Some(&one_click),
        },
    )
    .await?;
    sqlx::query!(
        "INSERT INTO campaign_sends (tenant_id, campaign_id, subscriber_id, status, message_id,
                                     token_hash, personalized)
         VALUES ($1, $2, $3, 'sent', $4, $5, $6)",
        tx.tenant_id(),
        campaign,
        m.id,
        message,
        minted.hash,
        who.personalized.load(std::sync::atomic::Ordering::Relaxed)
    )
    .execute(&mut **tx)
    .await?;
    Ok(Ok(()))
}

/// Why a queued campaign message must not go out now (checked right before SMTP, next to the
/// subscriber's status and consent): its campaign was cancelled, or it was personalized and the
/// customer no longer grants `personalization` (A20). `None` = send.
pub async fn delivery_refusal(
    tx: &mut TenantTx,
    message_id: Uuid,
) -> Result<Option<&'static str>, Error> {
    let Some(r) = sqlx::query!(
        "SELECT c.status, s.personalized, sub.customer_id
         FROM campaign_sends s
         JOIN campaigns c ON c.id = s.campaign_id
         JOIN subscribers sub ON sub.id = s.subscriber_id
         WHERE s.message_id = $1",
        message_id
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    if r.status == "cancelled" {
        return Ok(Some("campaign_cancelled"));
    }
    if r.personalized {
        let granted = match r.customer_id {
            Some(c) => {
                consent::current(tx, &Subject::Customer(c), ConsentPurpose::Personalization).await?
            }
            None => false,
        };
        if !granted {
            return Ok(Some("personalization_withdrawn"));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------------
// The recipient's token (unsubscribe, preferences, clicks)

struct Sent {
    id: Uuid,
    subscriber_id: Uuid,
}

async fn by_token(tx: &mut TenantTx, token: &str) -> Result<Sent, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    sqlx::query_as!(
        Sent,
        "SELECT id, subscriber_id FROM campaign_sends WHERE token_hash = $1",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)
}

/// The preference page's view of a subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Preferences {
    /// The address, partly masked.
    pub email: String,
    pub status: subscribers::Status,
}

pub async fn preferences(tx: &mut TenantTx, token: &str) -> Result<Preferences, Error> {
    let s = by_token(tx, token).await?;
    let sub = subscribers::get(tx, s.subscriber_id).await?;
    Ok(Preferences {
        email: subscribers::mask(&sub.email),
        status: sub.status,
    })
}

/// One-click unsubscribe (RFC 8058) or the preference page's button. Idempotent.
pub async fn unsubscribe(
    tx: &mut TenantTx,
    token: &str,
    ip_hash: Option<&[u8]>,
) -> Result<Preferences, Error> {
    let s = by_token(tx, token).await?;
    let sub = subscribers::unsubscribe(tx, s.subscriber_id, "unsubscribe", ip_hash)
        .await?
        .ok_or(Error::NotFound)?;
    sqlx::query!(
        "UPDATE campaign_sends SET unsubscribed_at = coalesce(unsubscribed_at, now()) WHERE id = $1",
        s.id
    )
    .execute(&mut **tx)
    .await?;
    Ok(Preferences {
        email: subscribers::mask(&sub.email),
        status: sub.status,
    })
}

/// Subscribing again from the preference page: a fresh double opt-in to the same address.
pub async fn resubscribe(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    ip_hash: Option<&[u8]>,
) -> Result<(), Error> {
    let s = by_token(tx, token).await?;
    let sub = subscribers::get(tx, s.subscriber_id).await?;
    subscribers::subscribe(tx, ctx, &sub.email, ip_hash, "resubscribe").await
}

/// A tracked click: verifies the signature, counts it and returns the target. `404` for an
/// unknown token or a signature that does not match (never an open redirect).
pub async fn click(tx: &mut TenantTx, token: &str, url: &str, sig: &str) -> Result<String, Error> {
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2000 {
        return Err(Error::NotFound);
    }
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    let r = sqlx::query!(
        "SELECT s.id, c.link_key FROM campaign_sends s JOIN campaigns c ON c.id = s.campaign_id
         WHERE s.token_hash = $1",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    if !verify(&r.link_key, token, url, sig)? {
        return Err(Error::NotFound);
    }
    sqlx::query!(
        "UPDATE campaign_sends SET clicked_at = coalesce(clicked_at, now()),
             click_count = click_count + 1
         WHERE id = $1",
        r.id
    )
    .execute(&mut **tx)
    .await?;
    Ok(url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_bind_token_and_target() {
        let key = [7u8; 32];
        let s = sign(&key, "tok", "https://shop.example/p/a").unwrap();
        assert_eq!(s.len(), SIG_BYTES * 2);
        assert!(verify(&key, "tok", "https://shop.example/p/a", &s).unwrap());
        assert!(!verify(&key, "tok", "https://evil.example/", &s).unwrap());
        assert!(!verify(&key, "other", "https://shop.example/p/a", &s).unwrap());
        assert!(!verify(&[8u8; 32], "tok", "https://shop.example/p/a", &s).unwrap());
        assert!(!verify(&key, "tok", "https://shop.example/p/a", "zz").unwrap());
        assert!(!verify(&key, "tok", "https://shop.example/p/a", &s[..10]).unwrap());
    }

    #[test]
    fn tracked_links_wrap_http_only() {
        let links = Links {
            track: Some(("tok".into(), vec![1; 32])),
            click_base: "http://checkout.demo.localhost/_p/newsletter/click".into(),
            unsubscribe_url: String::new(),
            preferences_url: String::new(),
        };
        let h = links.href("https://demo.localhost/p/a?x=1&y=2").unwrap();
        assert!(h.starts_with("http://checkout.demo.localhost/_p/newsletter/click?t=tok&u=https%3A%2F%2Fdemo.localhost%2Fp%2Fa%3Fx%3D1%26y%3D2&s="), "{h}");
        assert_eq!(links.href("mailto:a@b.cz").unwrap(), "mailto:a@b.cz");
    }

    #[test]
    fn content_is_validated() {
        let mut content = BTreeMap::new();
        content.insert(
            "cs".to_owned(),
            LocaleContent {
                subject: " Novinky ".into(),
                preheader: String::new(),
                blocks: vec![
                    EmailBlock::Text {
                        html: "<p onclick=x()>Ahoj<script>alert(1)</script></p>".into(),
                    },
                    EmailBlock::PersonalizedProducts {
                        title: "Pro vás".into(),
                        limit: 4,
                    },
                ],
            },
        );
        let input = CampaignInput {
            name: "Září".into(),
            segment_id: None,
            content,
        };
        let (_, c) = validate(&input).unwrap();
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("onclick") && !s.contains("<script"));
        assert_eq!(c["cs"].subject, "Novinky");

        let mut bad = input.clone();
        bad.content.insert("de".into(), c["cs"].clone());
        assert!(validate(&bad).is_err(), "unknown language");
        let mut bad = input.clone();
        if let Some(cs) = bad.content.get_mut("cs") {
            cs.blocks = vec![EmailBlock::Button {
                label: "x".into(),
                href: "javascript:alert(1)".into(),
            }];
        }
        assert!(validate(&bad).is_err(), "javascript: link");
        let mut bad = input;
        if let Some(cs) = bad.content.get_mut("cs") {
            cs.blocks = vec![EmailBlock::PersonalizedProducts {
                title: String::new(),
                limit: 30,
            }];
        }
        assert!(validate(&bad).is_err(), "limit");
        assert!(
            serde_json::from_value::<EmailBlock>(json!({"type": "html", "raw": "<script>"}))
                .is_err()
        );
    }

    #[test]
    fn content_falls_back_to_the_market_language() {
        let lc = |s: &str| LocaleContent {
            subject: s.into(),
            preheader: String::new(),
            blocks: vec![],
        };
        let mut c = BTreeMap::new();
        c.insert("cs".to_owned(), lc("cs"));
        c.insert("sk".to_owned(), lc("sk"));
        assert_eq!(pick(&c, "sk", "cs").unwrap().1.subject, "sk");
        assert_eq!(pick(&c, "en", "sk").unwrap().1.subject, "sk");
        assert_eq!(pick(&c, "en", "de").unwrap().1.subject, "cs");
    }
}
