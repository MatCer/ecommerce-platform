use chrono::{Duration, Utc};
use platform::Error;
use platform::db::TenantTx;
use sqlx::PgPool;
use uuid::Uuid;

use crate::capability;
use crate::cart::{self, CartRef};
use crate::storefront::Context;
use crate::tenancy::{self, CheckoutMode};

pub const HANDOFF_SECS: i64 = 60;
pub const SAME_ORIGIN_IDLE_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopOrigin(String);

impl ShopOrigin {
    /// `https://<host>[:port]`, or `http://` for `*.localhost` only; lowercase, no default
    /// port, no path, query or userinfo.
    pub fn parse(s: &str) -> Option<Self> {
        let url = reqwest::Url::parse(s).ok()?;
        let host = url.host_str()?;
        let scheme_ok = match url.scheme() {
            "https" => true,
            "http" => host.ends_with(".localhost"),
            _ => false,
        };
        (scheme_ok
            && url.port() != Some(0)
            && url.origin().ascii_serialization() == s
            && tenancy::normalize_host(host).ok()? == host)
            .then(|| Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

struct Policy<'a> {
    origin: &'a str,
    generation: i64,
    mode: CheckoutMode,
}

pub(crate) async fn fence(tx: &mut TenantTx) -> Result<(), Error> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock_shared(platform.checkout_fence_key($1))",
        tx.tenant_id()
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn enter<'a>(tx: &mut TenantTx, ctx: &'a Context) -> Result<Policy<'a>, Error> {
    let origin = ctx.origin.as_ref().ok_or(Error::Forbidden {
        code: "shop_origin_required",
    })?;
    let row = sqlx::query!(
        r#"SELECT generation AS "generation!", mode AS "mode!", origin_tenant, origin_market
           FROM platform.enter_checkout_fence($1, $2)"#,
        tx.tenant_id(),
        origin.as_str()
    )
    .fetch_one(&mut **tx)
    .await?;
    if row.origin_tenant.is_some_and(|t| t != tx.tenant_id())
        || row.origin_market.is_some_and(|m| m != ctx.market.id)
    {
        return Err(Error::Forbidden {
            code: "shop_origin_mismatch",
        });
    }
    let mode = CheckoutMode::parse(&row.mode)
        .ok_or_else(|| Error::Internal(format!("unknown checkout mode {}", row.mode)))?;
    Ok(Policy {
        origin: origin.as_str(),
        generation: row.generation,
        mode,
    })
}

fn idle(mode: CheckoutMode) -> Duration {
    match mode {
        CheckoutMode::Isolated => Duration::days(cart::IDLE_DAYS),
        CheckoutMode::SameOrigin => Duration::hours(SAME_ORIGIN_IDLE_HOURS),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Open { refresh: bool },
    Placement,
    Receipt,
}

impl Reach {
    fn statuses(self) -> &'static [&'static str] {
        match self {
            Reach::Open { .. } => &["open"],
            Reach::Placement => &["open", "converted"],
            Reach::Receipt => &["converted"],
        }
    }

    fn through_replaced(self) -> bool {
        self == Reach::Receipt
    }

    fn refreshes(self) -> bool {
        self == Reach::Open { refresh: true }
    }
}

/// Every condition sits in the locking `WHERE`, so a request queued behind a rotation
/// re-checks the committed row and finds none.
async fn guard(
    tx: &mut TenantTx,
    ctx: &Context,
    p: &Policy<'_>,
    token: &str,
    reach: Reach,
) -> Result<(CartRef, bool), Error> {
    if !capability::well_formed(token) {
        return Err(Error::NotFound);
    }
    let hash = capability::hash(token);
    let replaced = reach.through_replaced().then_some(hash.as_slice());
    let row = sqlx::query!(
        r#"SELECT id, market_id, status = 'open' AS "open!" FROM carts
           WHERE (checkout_token_hash = $1 OR replaced_token_hash = $2)
             AND checkout_token_hash IS NOT NULL
             AND status = ANY($3)
             AND market_id = $4 AND checkout_origin = $5 AND checkout_generation = $6
             AND last_activity_at > $7
           FOR UPDATE"#,
        hash.as_slice(),
        replaced,
        reach.statuses() as &[&str],
        ctx.market.id,
        p.origin,
        p.generation,
        Utc::now() - idle(p.mode)
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    if reach.refreshes() {
        sqlx::query!(
            "UPDATE carts SET last_activity_at = now() WHERE id = $1",
            row.id
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok((
        CartRef {
            id: row.id,
            market_id: row.market_id,
        },
        row.open,
    ))
}

async fn bind(tx: &mut TenantTx, cart_id: Uuid, p: &Policy<'_>) -> Result<Option<String>, Error> {
    let minted = capability::mint();
    let updated = sqlx::query!(
        "UPDATE carts SET checkout_token_hash = $2, checkout_origin = $3, checkout_generation = $4,
             last_activity_at = now(), updated_at = now()
         WHERE id = $1 AND status = 'open'",
        cart_id,
        minted.hash,
        p.origin,
        p.generation
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok((updated == 1).then_some(minted.token))
}

fn require(p: &Policy<'_>, mode: CheckoutMode) -> Result<(), Error> {
    if p.mode == mode {
        Ok(())
    } else {
        Err(Error::Conflict {
            code: "checkout_mode_changed",
            detail: "the shop's checkout mode changed; start checkout again".into(),
        })
    }
}

async fn require_lines(tx: &mut TenantTx, cart_id: Uuid) -> Result<(), Error> {
    let lines = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM cart_lines WHERE cart_id = $1",
        cart_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if lines == 0 {
        return Err(Error::Conflict {
            code: "cart_empty",
            detail: "the cart is empty".into(),
        });
    }
    Ok(())
}

async fn revoke_earlier_checkouts(
    tx: &mut TenantTx,
    p: &Policy<'_>,
    source: &CartRef,
    previous: Option<&str>,
) -> Result<(), Error> {
    let previous = previous
        .filter(|t| capability::well_formed(t))
        .map(capability::hash);
    sqlx::query!(
        "UPDATE carts SET checkout_token_hash = NULL,
             status = CASE WHEN status = 'open' THEN 'abandoned' ELSE status END,
             updated_at = now()
         WHERE checkout_token_hash IS NOT NULL
           AND (source_cart_id = $1
                OR ((checkout_token_hash = $2 OR replaced_token_hash = $2)
                    AND checkout_origin = $3))",
        source.id,
        previous,
        p.origin
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The open checkout behind `token`, locked until commit; refreshes its idle expiry.
pub async fn open(tx: &mut TenantTx, ctx: &Context, token: &str) -> Result<CartRef, Error> {
    let p = enter(tx, ctx).await?;
    Ok(guard(tx, ctx, &p, token, Reach::Open { refresh: true })
        .await?
        .0)
}

pub(crate) async fn open_in(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
    mode: CheckoutMode,
) -> Result<CartRef, Error> {
    let p = enter(tx, ctx).await?;
    require(&p, mode)?;
    Ok(guard(tx, ctx, &p, token, Reach::Open { refresh: true })
        .await?
        .0)
}

/// The shop capability's cart, else the checkout as in [`open`] (`GET /cart`).
pub async fn open_any(tx: &mut TenantTx, ctx: &Context, token: &str) -> Result<CartRef, Error> {
    match cart::find(tx, ctx, token).await {
        Err(Error::NotFound) => open(tx, ctx, token).await,
        found => found,
    }
}

/// The narrow recovery of a converted checkout for placement and its idempotent replay
/// (A12): open or converted, no refresh. Returns whether the cart is still open.
pub async fn open_for_order(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<(CartRef, bool), Error> {
    let p = enter(tx, ctx).await?;
    lock_source_before_checkout(tx, token).await?;
    guard(tx, ctx, &p, token, Reach::Placement).await
}

pub(crate) async fn converted(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<CartRef, Error> {
    let p = enter(tx, ctx).await?;
    if p.mode != CheckoutMode::SameOrigin {
        return Err(Error::NotFound);
    }
    Ok(guard(tx, ctx, &p, token, Reach::Receipt).await?.0)
}

async fn lock_source_before_checkout(tx: &mut TenantTx, token: &str) -> Result<(), Error> {
    sqlx::query!(
        "SELECT 1 AS one FROM carts
         WHERE id = (SELECT source_cart_id FROM carts WHERE checkout_token_hash = $1)
         FOR UPDATE",
        capability::hash(token)
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(())
}

async fn replace_keeping_receipt(tx: &mut TenantTx, cart_id: Uuid) -> Result<String, Error> {
    let minted = capability::mint();
    let updated = sqlx::query!(
        "UPDATE carts SET replaced_token_hash = checkout_token_hash, checkout_token_hash = $2,
             last_activity_at = now(), updated_at = now()
         WHERE id = $1 AND status = 'open'",
        cart_id,
        minted.hash
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if updated != 1 {
        return Err(Error::Internal("a placing checkout is not open".into()));
    }
    Ok(minted.token)
}

pub(crate) async fn convert(
    tx: &mut TenantTx,
    ctx: &Context,
    checkout: &CartRef,
    note_written: bool,
) -> Result<Option<String>, Error> {
    let replaced = if note_written {
        let p = enter(tx, ctx).await?;
        match p.mode {
            CheckoutMode::Isolated => None,
            CheckoutMode::SameOrigin => Some(replace_keeping_receipt(tx, checkout.id).await?),
        }
    } else {
        None
    };
    let source = sqlx::query_scalar!(
        "UPDATE carts SET status = 'converted', updated_at = now() WHERE id = $1
         RETURNING source_cart_id",
        checkout.id
    )
    .fetch_one(&mut **tx)
    .await?;
    let Some(source) = source else {
        return Ok(replaced);
    };
    let shop_cart = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM carts
                          WHERE id = $1 AND status = 'open' AND shop_token_hash IS NOT NULL)
           AS "shop_cart!""#,
        source
    )
    .fetch_one(&mut **tx)
    .await?;
    if shop_cart {
        cart::empty(tx, source).await?;
    }
    Ok(replaced)
}

/// Replaces the capability `token` (re-authorized here) with a fresh one (C6, C10).
pub async fn rotate(tx: &mut TenantTx, ctx: &Context, token: &str) -> Result<String, Error> {
    let p = enter(tx, ctx).await?;
    let (c, _) = guard(tx, ctx, &p, token, Reach::Open { refresh: false }).await?;
    bind(tx, c.id, &p).await?.ok_or(Error::NotFound)
}

/// C10, same-origin mode only: copies the shop cart behind `shop_token` into a new checkout
/// snapshot linked to it (lines in order, coupon, locale, currency; no contact, address,
/// customer or selection) and returns the snapshot's capability, bound to the origin and
/// generation. The shop cart and its capability stay. In the same transaction every
/// capability of that shop cart's earlier checkouts and the browser's `previous` one on this
/// origin are revoked; open ones become `abandoned`. `previous` is a revocation hint, never
/// authorization: a malformed or unknown one revokes nothing. Starts on one shop cart
/// serialize on its row lock, so the last to commit wins. `409 checkout_mode_changed` on an
/// isolated origin, `409 cart_empty`, `404` for an unknown shop capability.
pub async fn start(
    tx: &mut TenantTx,
    ctx: &Context,
    shop_token: &str,
    previous: Option<&str>,
) -> Result<String, Error> {
    let p = enter(tx, ctx).await?;
    require(&p, CheckoutMode::SameOrigin)?;
    let source = cart::find(tx, ctx, shop_token).await?;
    require_lines(tx, source.id).await?;
    revoke_earlier_checkouts(tx, &p, &source, previous).await?;
    let snapshot = cart::snapshot(tx, &source).await?;
    bind(tx, snapshot.id, &p)
        .await?
        .ok_or_else(|| Error::Internal("a new snapshot is not open".into()))
}

/// A1, isolated mode only: revokes the shop capability of a non-empty cart and mints a
/// single-use handoff token bound to the origin and generation.
pub async fn start_handoff(
    tx: &mut TenantTx,
    ctx: &Context,
    shop_token: &str,
) -> Result<String, Error> {
    let p = enter(tx, ctx).await?;
    require(&p, CheckoutMode::Isolated)?;
    let c = cart::find(tx, ctx, shop_token).await?;
    require_lines(tx, c.id).await?;
    sqlx::query!("DELETE FROM checkout_handoffs WHERE expires_at < now() - interval '1 day'")
        .execute(&mut **tx)
        .await?;
    let minted = capability::mint();
    sqlx::query!(
        "INSERT INTO checkout_handoffs
             (token_hash, tenant_id, cart_id, market_id, expires_at, origin, generation)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6, $7)",
        minted.hash,
        tx.tenant_id(),
        c.id,
        c.market_id,
        HANDOFF_SECS as f64,
        p.origin,
        p.generation
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE carts SET shop_token_hash = NULL, updated_at = now() WHERE id = $1",
        c.id
    )
    .execute(&mut **tx)
    .await?;
    Ok(minted.token)
}

/// A1, isolated mode only: consumes the handoff atomically and mints the checkout capability.
/// `None` for unknown, used or expired tokens and for another origin, market or generation; a
/// mismatch leaves the handoff unspent.
pub async fn redeem_handoff(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<Option<String>, Error> {
    let p = enter(tx, ctx).await?;
    if p.mode != CheckoutMode::Isolated || !capability::well_formed(token) {
        return Ok(None);
    }
    let Some(cart_id) = sqlx::query_scalar!(
        "UPDATE checkout_handoffs SET used_at = now()
         WHERE token_hash = $1 AND market_id = $2 AND origin = $3 AND generation = $4
           AND used_at IS NULL AND expires_at > now()
         RETURNING cart_id",
        capability::hash(token),
        ctx.market.id,
        p.origin,
        p.generation
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    bind(tx, cart_id, &p).await
}

/// Isolated mode only: consumes an abandoned-cart restore link (WP19) and mints the checkout
/// capability of its open cart in the context's market.
pub async fn restore_cart(
    tx: &mut TenantTx,
    ctx: &Context,
    token: &str,
) -> Result<Option<String>, Error> {
    let p = enter(tx, ctx).await?;
    require(&p, CheckoutMode::Isolated)?;
    if !capability::well_formed(token) {
        return Ok(None);
    }
    let Some(cart_id) = sqlx::query_scalar!(
        "UPDATE flow_restore_tokens SET used_at = $3
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $3
           AND cart_id IN (SELECT id FROM carts WHERE market_id = $2 AND status = 'open')
         RETURNING cart_id",
        capability::hash(token),
        ctx.market.id,
        ctx.now
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    bind(tx, cart_id, &p).await
}

/// Switches one origin of the tenant to `mode` in its own transaction, revoking every checkout
/// capability and handoff of the tenant. Waits for in-flight capability transactions of the
/// tenant. `false` when the origin already had `mode`; an unknown history refuses same-origin
/// (`409 origin_history_unknown`).
pub async fn switch_mode(
    db: &PgPool,
    tenant_id: Uuid,
    origin: &ShopOrigin,
    mode: CheckoutMode,
) -> Result<bool, Error> {
    let mut tx = db.begin().await?;
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(platform.checkout_fence_key($1))",
        tenant_id
    )
    .execute(&mut *tx)
    .await?;
    let changed = sqlx::query!(
        "UPDATE platform.checkout_origins SET mode = $3
         WHERE origin = $1 AND tenant_id = $2 AND mode <> $3",
        origin.as_str(),
        tenant_id,
        mode.as_str()
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if e.as_database_error()
            .is_some_and(|d| d.code().as_deref() == Some("23514"))
        {
            Error::Conflict {
                code: "origin_history_unknown",
                detail: "same-origin checkout needs a known hostname history".into(),
            }
        } else {
            e.into()
        }
    })?
    .rows_affected()
        == 1;
    if !changed {
        sqlx::query_scalar!(
            "SELECT 1 AS \"one!\" FROM platform.checkout_origins WHERE origin = $1 AND tenant_id = $2",
            origin.as_str(),
            tenant_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(Error::NotFound)?;
    }
    tx.commit().await?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::ShopOrigin;

    #[test]
    fn shop_origin_is_the_exact_browser_serialization() {
        for ok in [
            "https://shop.example.cz",
            "https://shop.example.cz:8443",
            "http://demo.localhost:8080",
            "http://demo.localhost",
            "https://demo.localhost",
        ] {
            assert_eq!(
                ShopOrigin::parse(ok).map(|o| o.0),
                Some(ok.to_owned()),
                "{ok}"
            );
        }
        for bad in [
            "",
            "shop.example.cz",
            "http://shop.example.cz",
            "ftp://shop.example.cz",
            "HTTPS://shop.example.cz",
            "https://Shop.example.cz",
            "https://shop.example.cz.",
            "https://shop.example.cz:443",
            "http://demo.localhost:80",
            "https://shop.example.cz:0",
            "https://shop.example.cz:08443",
            "https://shop.example.cz:+8443",
            "https://shop.example.cz:",
            "https://shop.example.cz:70000",
            "https://shop.example.cz/",
            "https://shop.example.cz/path",
            "https://shop.example.cz?q=1",
            "https://user@shop.example.cz",
            "https://user:pw@shop.example.cz",
            "https://localhost",
            "https://shop example.cz",
        ] {
            assert_eq!(ShopOrigin::parse(bad), None, "{bad}");
        }
    }
}
