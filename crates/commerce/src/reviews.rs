//! Product reviews (spec §7.6, §11.6, §11.7, §14).
//!
//! - Review tokens are capabilities issued per delivered order line ([`issue_tokens`], the
//!   entry point of WP19's review invites): single use, [`TOKEN_DAYS`] valid, only the SHA-256
//!   stored. Only a buyer can therefore write a review, and every such review is `verified`
//!   (linked to a delivered order line), which is what the shop's "how we verify reviews"
//!   legal page and the storefront disclosure say (Omnibus).
//! - Submissions are plain text (control and bidi characters dropped, whitespace normalized,
//!   length limits), rate-limited per IP, and wait in the moderation queue as `pending`.
//! - Staff publish, reject or hide reviews and may reply. Only `published` reviews leave the
//!   admin API: product page model, rating summary and JSON-LD.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::capability;
use crate::markets::invalid;
use crate::storefront::Context;

/// How long a review link works.
pub const TOKEN_DAYS: i64 = 90;
/// Submissions one (hashed) IP may make per hour before `429`.
pub const MAX_SUBMISSIONS_PER_IP_HOUR: i64 = 10;
/// Published reviews a product page shows (newest first).
pub const PAGE_REVIEWS: i64 = 20;
/// Of those, how many go into the JSON-LD (the aggregate covers all).
pub const JSON_LD_REVIEWS: usize = 5;
/// Outbox event after every visible change (purges the product's cached pages).
pub const CHANGED_EVENT: &str = "review.changed";

pub const NAME_MAX: usize = 60;
pub const TITLE_MAX: usize = 120;
pub const BODY_MAX: usize = 4000;
pub const REPLY_MAX: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ReviewStatus)]
pub enum Status {
    Pending,
    Published,
    Rejected,
    Hidden,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Published => "published",
            Self::Rejected => "rejected",
            Self::Hidden => "hidden",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "published" => Self::Published,
            "rejected" => Self::Rejected,
            "hidden" => Self::Hidden,
            _ => Self::Pending,
        }
    }

    /// Moderation transitions: publish a pending, rejected or hidden review; reject a pending
    /// one; hide a published one. Nothing goes back to `pending`.
    pub fn can_become(self, to: Self) -> bool {
        matches!(
            (self, to),
            (
                Self::Pending | Self::Rejected | Self::Hidden,
                Self::Published
            ) | (Self::Pending, Self::Rejected)
                | (Self::Published, Self::Hidden)
        )
    }
}

// ---------------------------------------------------------------------------------------
// Tokens

/// A review link for one delivered order line (the raw token exists only here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    pub order_line_id: Uuid,
    pub product_id: Uuid,
    /// Product name as bought (order line snapshot).
    pub product_name: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Issues review tokens for a delivered order: one per product bought (its first line), not
/// yet reviewed and still in the catalog. Calling it again rotates the unused tokens (the old
/// links stop working) and skips lines already reviewed. `404` for an unknown order, `409
/// order_not_delivered` before delivery.
///
/// WP19 sends the invite mail with [`review_url`] for each token.
pub async fn issue_tokens(
    tx: &mut TenantTx,
    order_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<IssuedToken>, Error> {
    let status = sqlx::query_scalar!("SELECT status FROM orders WHERE id = $1", order_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    if status != "delivered" {
        return Err(Error::Conflict {
            code: "order_not_delivered",
            detail: "reviews can be requested only for delivered orders".into(),
        });
    }
    let lines = sqlx::query!(
        r#"SELECT DISTINCT ON (l.product_id) l.id, l.product_id AS "product_id!", l.name
           FROM order_lines l
           WHERE l.order_id = $1 AND l.product_id IS NOT NULL
             AND NOT EXISTS (SELECT 1 FROM reviews r WHERE r.order_line_id = l.id)
             AND NOT EXISTS (SELECT 1 FROM review_tokens t
                             WHERE t.order_line_id = l.id AND t.used_at IS NOT NULL)
           ORDER BY l.product_id, l.position"#,
        order_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let expires_at = now + Duration::days(TOKEN_DAYS);
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        let minted = capability::mint();
        sqlx::query!(
            "INSERT INTO review_tokens (tenant_id, order_line_id, order_id, product_id,
                                        token_hash, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, order_line_id) DO UPDATE
                 SET token_hash = EXCLUDED.token_hash, expires_at = EXCLUDED.expires_at,
                     created_at = now()
                 WHERE review_tokens.used_at IS NULL",
            tx.tenant_id(),
            l.id,
            order_id,
            l.product_id,
            minted.hash,
            expires_at
        )
        .execute(&mut **tx)
        .await?;
        out.push(IssuedToken {
            order_line_id: l.id,
            product_id: l.product_id,
            product_name: l.name,
            token: minted.token,
            expires_at,
        });
    }
    Ok(out)
}

/// The review form on the checkout origin for a token.
pub fn review_url(ctx: &Context, token: &str) -> String {
    ctx.checkout_url(&format!("/review?token={token}"))
}

/// What the review form shows for a token (a read, no change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReviewInvitation {
    /// Product name and options as bought.
    pub product_name: String,
    pub options_label: String,
    pub expires_at: DateTime<Utc>,
}

/// The unused, unexpired token's order line; `404` otherwise.
pub async fn invitation(tx: &mut TenantTx, token: &str) -> Result<ReviewInvitation, Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    let r = sqlx::query!(
        "SELECT l.name, l.options_label, t.expires_at
         FROM review_tokens t JOIN order_lines l ON l.id = t.order_line_id
         WHERE t.token_hash = $1 AND t.used_at IS NULL AND t.expires_at > now()",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(ReviewInvitation {
        product_name: r.name,
        options_label: r.options_label,
        expires_at: r.expires_at,
    })
}

// ---------------------------------------------------------------------------------------
// Submission

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    /// The capability from the review link.
    pub token: String,
    /// 1-5 stars.
    pub rating: i16,
    /// Shown publicly (1-60 characters).
    pub name: String,
    /// Optional headline (≤ 120 characters).
    #[serde(default)]
    pub title: String,
    /// 1-4000 characters of plain text.
    pub body: String,
}

/// Plain text: drops control characters (keeping line breaks when `multiline`) and bidi
/// overrides, normalizes line endings and whitespace, trims. Never HTML: output is escaped
/// wherever it is shown.
pub fn plain_text(s: &str, multiline: bool) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let kept: String = s
        .chars()
        .filter_map(|c| match c {
            '\n' if multiline => Some('\n'),
            '\t' | '\n' => Some(' '),
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}' | '\u{FEFF}' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    if !multiline {
        return kept.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    // Lines trimmed of trailing/inner runs of spaces, at most one blank line in a row.
    let mut out = String::with_capacity(kept.len());
    let mut blank = 0;
    for line in kept.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim().to_owned()
}

fn bounded(
    field: &'static str,
    code: &'static str,
    v: &str,
    min: usize,
    max: usize,
) -> Result<(), Error> {
    let n = v.chars().count();
    if n < min || n > max {
        return Err(invalid(
            code,
            format!("{field} must be {min}-{max} characters"),
        ));
    }
    Ok(())
}

struct Clean {
    rating: i16,
    name: String,
    title: String,
    body: String,
}

fn validate(input: &ReviewInput) -> Result<Clean, Error> {
    if !(1..=5).contains(&input.rating) {
        return Err(invalid("invalid_rating", "rating must be 1-5"));
    }
    let name = plain_text(&input.name, false);
    let title = plain_text(&input.title, false);
    let body = plain_text(&input.body, true);
    bounded("name", "invalid_name", &name, 1, NAME_MAX)?;
    bounded("title", "invalid_title", &title, 0, TITLE_MAX)?;
    bounded("body", "invalid_body", &body, 1, BODY_MAX)?;
    Ok(Clean {
        rating: input.rating,
        name,
        title,
        body,
    })
}

/// Submits a review with a token: consumed atomically (a second submission gets `404`), the
/// review waits for moderation. `422` for invalid input (the token stays usable), `429` when
/// the IP submitted too often.
pub async fn submit(
    tx: &mut TenantTx,
    ctx: &Context,
    input: &ReviewInput,
    ip_hash: Option<&[u8]>,
) -> Result<Uuid, Error> {
    if !capability::well_formed(&input.token) {
        return Err(Error::NotFound);
    }
    let clean = validate(input)?;
    if let Some(ip) = ip_hash {
        let recent = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM customer_auth_attempts
               WHERE kind = 'review' AND ip_hash = $1 AND at > now() - interval '1 hour'"#,
            ip
        )
        .fetch_one(&mut **tx)
        .await?;
        if recent >= MAX_SUBMISSIONS_PER_IP_HOUR {
            return Err(Error::TooManyRequests {
                code: "too_many_reviews",
            });
        }
        sqlx::query!(
            "INSERT INTO customer_auth_attempts (tenant_id, kind, email, ip_hash)
             VALUES ($1, 'review', '', $2)",
            tx.tenant_id(),
            ip
        )
        .execute(&mut **tx)
        .await?;
    }
    let used = sqlx::query!(
        "UPDATE review_tokens SET used_at = now()
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING order_line_id, product_id",
        capability::hash(&input.token)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let id = sqlx::query_scalar!(
        "INSERT INTO reviews (tenant_id, product_id, order_line_id, customer_name, rating,
                              title, body, verified, locale, ip_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, true, $8, $9)
         RETURNING id",
        tx.tenant_id(),
        used.product_id,
        used.order_line_id,
        clean.name,
        clean.rating,
        clean.title,
        clean.body,
        ctx.locale,
        ip_hash
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

// ---------------------------------------------------------------------------------------
// Moderation (admin)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Review {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product_name: String,
    /// The order behind a verified review (absent once erased).
    pub order_id: Option<Uuid>,
    pub order_number: Option<i64>,
    pub customer_name: String,
    pub rating: i16,
    pub title: String,
    pub body: String,
    pub status: Status,
    pub verified: bool,
    pub locale: String,
    pub reply: Option<String>,
    pub replied_at: Option<DateTime<Utc>>,
    pub moderated_at: Option<DateTime<Utc>>,
    pub moderated_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewPage {
    pub items: Vec<Review>,
    /// Pass as `cursor` for the next (older) page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

struct Row {
    id: Uuid,
    product_id: Uuid,
    product_name: Option<String>,
    order_id: Option<Uuid>,
    order_number: Option<i64>,
    customer_name: String,
    rating: i16,
    title: String,
    body: String,
    status: String,
    verified: bool,
    locale: String,
    reply: Option<String>,
    replied_at: Option<DateTime<Utc>>,
    moderated_at: Option<DateTime<Utc>>,
    moderated_by: Option<String>,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<Row> for Review {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            product_id: r.product_id,
            product_name: r.product_name.unwrap_or_default(),
            order_id: r.order_id,
            order_number: r.order_number,
            customer_name: r.customer_name,
            rating: r.rating,
            title: r.title,
            body: r.body,
            status: Status::parse(&r.status),
            verified: r.verified,
            locale: r.locale,
            reply: r.reply,
            replied_at: r.replied_at,
            moderated_at: r.moderated_at,
            moderated_by: r.moderated_by,
            published_at: r.published_at,
            created_at: r.created_at,
        }
    }
}

/// The moderation queue, newest first; optionally one status and/or one product.
pub async fn list(
    tx: &mut TenantTx,
    status: Option<Status>,
    product_id: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<ReviewPage, Error> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT r.id, r.product_id,
                  (SELECT pt.name FROM product_translations pt WHERE pt.product_id = r.product_id
                   ORDER BY (pt.locale = r.locale) DESC, pt.locale LIMIT 1) AS product_name,
                  o.id AS "order_id?", o.number AS "order_number?",
                  r.customer_name, r.rating, r.title, r.body, r.status, r.verified, r.locale,
                  r.reply, r.replied_at, r.moderated_at, r.moderated_by, r.published_at,
                  r.created_at
           FROM reviews r
           LEFT JOIN order_lines l ON l.id = r.order_line_id
           LEFT JOIN orders o ON o.id = l.order_id
           WHERE ($1::text IS NULL OR r.status = $1)
             AND ($2::uuid IS NULL OR r.product_id = $2)
             AND ($3::uuid IS NULL OR r.id < $3)
           ORDER BY r.id DESC LIMIT $4"#,
        status.map(Status::as_str),
        product_id,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items: Vec<Review> = rows.into_iter().map(Review::from).collect();
    let limit = usize::try_from(limit).unwrap_or(100);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(ReviewPage { items, next_cursor })
}

pub async fn get(tx: &mut TenantTx, id: Uuid) -> Result<Review, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT r.id, r.product_id,
                  (SELECT pt.name FROM product_translations pt WHERE pt.product_id = r.product_id
                   ORDER BY (pt.locale = r.locale) DESC, pt.locale LIMIT 1) AS product_name,
                  o.id AS "order_id?", o.number AS "order_number?",
                  r.customer_name, r.rating, r.title, r.body, r.status, r.verified, r.locale,
                  r.reply, r.replied_at, r.moderated_at, r.moderated_by, r.published_at,
                  r.created_at
           FROM reviews r
           LEFT JOIN order_lines l ON l.id = r.order_line_id
           LEFT JOIN orders o ON o.id = l.order_id
           WHERE r.id = $1"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(Review::from)
    .ok_or(Error::NotFound)
}

async fn changed(tx: &mut TenantTx, product_id: Uuid) -> Result<(), Error> {
    platform::queue::publish(
        &mut **tx,
        CHANGED_EVENT,
        &json!({ "product_id": product_id }),
    )
    .await?;
    Ok(())
}

/// Publishes, rejects or hides a review (see [`Status::can_become`]); `409
/// invalid_transition` otherwise. Setting the current status again is a no-op.
pub async fn set_status(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    to: Status,
) -> Result<Review, Error> {
    let current = sqlx::query_scalar!("SELECT status FROM reviews WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(&mut **tx)
        .await?
        .map(|s| Status::parse(&s))
        .ok_or(Error::NotFound)?;
    if current == to {
        return get(tx, id).await;
    }
    if !current.can_become(to) {
        return Err(Error::Conflict {
            code: "invalid_transition",
            detail: format!(
                "a {} review cannot become {}",
                current.as_str(),
                to.as_str()
            ),
        });
    }
    let product_id = sqlx::query_scalar!(
        "UPDATE reviews SET status = $2, moderated_at = now(), moderated_by = $3,
             published_at = CASE WHEN $2 = 'published' THEN coalesce(published_at, now())
                                 ELSE published_at END,
             updated_at = now()
         WHERE id = $1 RETURNING product_id",
        id,
        to.as_str(),
        actor
    )
    .fetch_one(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "review.moderated",
        "review",
        Some(&id.to_string()),
        &json!({ "from": current.as_str(), "to": to.as_str() }),
    )
    .await?;
    // Only changes that touch what the shop shows purge it.
    if current == Status::Published || to == Status::Published {
        changed(tx, product_id).await?;
    }
    get(tx, id).await
}

/// Sets (or with `None` / blank, removes) the merchant's public reply.
pub async fn set_reply(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    reply: Option<&str>,
) -> Result<Review, Error> {
    let reply = reply.map(|r| plain_text(r, true)).filter(|r| !r.is_empty());
    if let Some(r) = &reply {
        bounded("reply", "invalid_reply", r, 1, REPLY_MAX)?;
    }
    let row = sqlx::query!(
        "UPDATE reviews SET reply = $2,
             replied_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END,
             updated_at = now()
         WHERE id = $1 RETURNING product_id, status",
        id,
        reply
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    audit::record(
        tx,
        actor,
        if reply.is_some() {
            "review.replied"
        } else {
            "review.reply_removed"
        },
        "review",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    if row.status == Status::Published.as_str() {
        changed(tx, row.product_id).await?;
    }
    get(tx, id).await
}

// ---------------------------------------------------------------------------------------
// Storefront

/// A published review as the shop shows it (plain text; escape on output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PublicReview {
    pub id: Uuid,
    pub customer_name: String,
    pub rating: i16,
    pub title: String,
    pub body: String,
    /// Written through a review link of a delivered order line.
    pub verified: bool,
    /// Language the review was written in (`lang` attribute).
    pub locale: String,
    pub published_on: NaiveDate,
    /// The merchant's public reply.
    pub reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RatingSummary {
    /// Mean rating, one decimal.
    pub average: f64,
    pub count: i64,
    /// Review counts for 1..5 stars (index 0 = 1 star).
    pub histogram: [i64; 5],
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ProductReviews {
    /// Absent while the product has no published review.
    pub summary: Option<RatingSummary>,
    /// Newest published reviews (at most 20).
    pub items: Vec<PublicReview>,
    /// The shop's published "how we verify reviews" page (Omnibus disclosure).
    pub verification_url: Option<String>,
}

/// Published reviews and their summary for a product page.
pub async fn for_product(
    tx: &mut TenantTx,
    ctx: &Context,
    product_id: Uuid,
) -> Result<ProductReviews, Error> {
    let counts = sqlx::query!(
        r#"SELECT rating, count(*) AS "n!" FROM reviews
           WHERE product_id = $1 AND status = 'published' GROUP BY rating"#,
        product_id
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut histogram = [0_i64; 5];
    for c in &counts {
        if let Some(slot) = usize::try_from(c.rating - 1)
            .ok()
            .and_then(|i| histogram.get_mut(i))
        {
            *slot = c.n;
        }
    }
    let count: i64 = histogram.iter().sum();
    let summary = (count > 0).then(|| {
        let total: i64 = histogram
            .iter()
            .zip(1_i64..)
            .map(|(n, stars)| n * stars)
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let average = (total as f64 / count as f64 * 10.0).round() / 10.0;
        RatingSummary {
            average,
            count,
            histogram,
        }
    });
    let items = if count == 0 {
        Vec::new()
    } else {
        sqlx::query!(
            r#"SELECT id, customer_name, rating, title, body, verified, locale, reply,
                      published_at AS "published_at!"
               FROM reviews WHERE product_id = $1 AND status = 'published'
               ORDER BY published_at DESC, id DESC LIMIT $2"#,
            product_id,
            PAGE_REVIEWS
        )
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|r| PublicReview {
            id: r.id,
            customer_name: r.customer_name,
            rating: r.rating,
            title: r.title,
            body: r.body,
            verified: r.verified,
            locale: r.locale,
            published_on: r.published_at.date_naive(),
            reply: r.reply,
        })
        .collect()
    };
    let slug = sqlx::query_scalar!(
        "SELECT t.slug FROM pages p JOIN page_translations t ON t.page_id = p.id
         WHERE p.legal_type = 'reviews' AND p.status = 'published' AND p.published_at <= $1
         ORDER BY (t.locale = $2) DESC, (t.locale = $3) DESC LIMIT 1",
        ctx.now,
        ctx.locale,
        ctx.market.default_locale
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(ProductReviews {
        summary,
        items,
        verification_url: slug.map(|s| ctx.path(&format!("/pages/{s}"))),
    })
}

/// `aggregateRating` + `review` for the Product JSON-LD (Google review snippets): only
/// published reviews, nothing without any.
pub fn json_ld(reviews: &ProductReviews) -> Option<(Value, Value)> {
    let s = reviews.summary.as_ref()?;
    let aggregate = json!({
        "@type": "AggregateRating",
        "ratingValue": format!("{:.1}", s.average),
        "reviewCount": s.count,
        "bestRating": 5,
        "worstRating": 1,
    });
    let items: Vec<Value> = reviews
        .items
        .iter()
        .take(JSON_LD_REVIEWS)
        .map(|r| {
            let mut v = json!({
                "@type": "Review",
                "author": { "@type": "Person", "name": r.customer_name },
                "datePublished": r.published_on.to_string(),
                "reviewBody": r.body,
                "reviewRating": {
                    "@type": "Rating",
                    "ratingValue": r.rating,
                    "bestRating": 5,
                    "worstRating": 1,
                },
            });
            if !r.title.is_empty() {
                v["name"] = json!(r.title);
            }
            v
        })
        .collect();
    Some((aggregate, Value::Array(items)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions() {
        use Status::*;
        let all = [Pending, Published, Rejected, Hidden];
        let allowed = [
            (Pending, Published),
            (Rejected, Published),
            (Hidden, Published),
            (Pending, Rejected),
            (Published, Hidden),
        ];
        for from in all {
            for to in all {
                assert_eq!(
                    from.can_become(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn plain_text_strips_controls_and_normalizes() {
        assert_eq!(plain_text("  Jan\t\u{7}  N.\n ", false), "Jan N.");
        assert_eq!(plain_text("a\u{202E}b\u{200B}c", false), "abc");
        assert_eq!(
            plain_text("Great!\r\n\r\n\r\n\r\nWould  buy\u{0} again.\n\n", true),
            "Great!\n\nWould buy again."
        );
        // Markup stays literal text (escaped on output), never interpreted.
        assert_eq!(
            plain_text("<script>alert(1)</script>", false),
            "<script>alert(1)</script>"
        );
    }

    fn input(rating: i16, name: &str, body: &str) -> ReviewInput {
        ReviewInput {
            token: String::new(),
            rating,
            name: name.into(),
            title: String::new(),
            body: body.into(),
        }
    }

    #[test]
    fn validation_limits() {
        assert!(validate(&input(5, "Jan", "Good")).is_ok());
        for bad in [
            input(0, "Jan", "Good"),
            input(6, "Jan", "Good"),
            input(5, "  \u{7} ", "Good"),
            input(5, "Jan", " \n "),
            input(5, &"x".repeat(NAME_MAX + 1), "Good"),
            input(5, "Jan", &"x".repeat(BODY_MAX + 1)),
        ] {
            assert_eq!(validate(&bad).err().map(|e| e.status().as_u16()), Some(422));
        }
        let mut long_title = input(5, "Jan", "Good");
        long_title.title = "t".repeat(TITLE_MAX + 1);
        assert!(validate(&long_title).is_err());
    }

    #[test]
    fn json_ld_only_with_reviews() {
        let empty = ProductReviews {
            summary: None,
            items: vec![],
            verification_url: None,
        };
        assert!(json_ld(&empty).is_none());
        let r = PublicReview {
            id: Uuid::nil(),
            customer_name: "Jan".into(),
            rating: 4,
            title: String::new(),
            body: "Good".into(),
            verified: true,
            locale: "cs".into(),
            published_on: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            reply: None,
        };
        let some = ProductReviews {
            summary: Some(RatingSummary {
                average: 4.0,
                count: 7,
                histogram: [0, 0, 0, 7, 0],
            }),
            items: vec![r; 7],
            verification_url: None,
        };
        let (agg, items) = json_ld(&some).unwrap();
        assert_eq!(agg["ratingValue"], "4.0");
        assert_eq!(agg["reviewCount"], 7);
        assert_eq!(items.as_array().map(Vec::len), Some(JSON_LD_REVIEWS));
        assert_eq!(items[0]["reviewRating"]["ratingValue"], 4);
        assert!(items[0].get("name").is_none(), "no empty headline");
    }
}
