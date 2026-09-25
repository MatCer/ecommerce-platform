//! Order emails after placement (WP12): shipped, delivered, cancelled, refunded, invoices and
//! credit notes. Each is rendered in the order's locale with the order summary and a fresh
//! order-page link, and enqueued in the caller's transaction (it exists exactly when the
//! change that caused it commits).

use chrono::Utc;
use platform::Error;
use platform::db::TenantTx;
use platform::mail::Stream;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::checkout::order_vars;
use crate::notifications::{self, AttachmentRef, Brand, Email, Template};
use crate::storefront::{self, PublicUrls};

/// Enqueues `template` for the order (idempotent per `key`).
pub async fn send(
    tx: &mut TenantTx,
    urls: &PublicUrls,
    order_id: Uuid,
    template: Template,
    extra: Value,
    key: String,
    attachments: &[AttachmentRef],
) -> Result<(), Error> {
    let o = super::view(tx, order_id).await?;
    let market = sqlx::query_scalar!("SELECT market_id FROM orders WHERE id = $1", order_id)
        .fetch_one(&mut **tx)
        .await?;
    let ctx = storefront::context(tx, urls, market, Some(&o.locale), Utc::now()).await?;
    let token = super::issue_token(tx, order_id).await?;
    let mut vars = json!({
        "order": order_vars(&o, ctx.checkout_url(&format!("/o/{token}"))),
        "withdraw_url": ctx.checkout_url("/withdraw"),
    });
    if let (Value::Object(v), Value::Object(e)) = (&mut vars, extra) {
        v.extend(e);
    }
    let brand = Brand::load(tx, ctx.base_url.clone()).await?;
    notifications::enqueue_with_attachments(
        tx,
        &brand,
        Email {
            template,
            stream: Stream::Transactional,
            to: &o.email,
            locale: &o.locale,
            vars,
            idempotency_key: key,
            // The body carries the order capability link.
            sensitive: true,
        },
        attachments,
    )
    .await?;
    Ok(())
}
