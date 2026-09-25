//! Inventory (spec §7.2, A13): stock levels per variant and the movement ledger.
//!
//! Every change is a movement with a unique identity `(kind, ref_type, ref_id, variant)`,
//! written in the same transaction as the level update: replaying a movement is a no-op
//! (idempotent order placement, cancel, shipment and return handlers). The level update is a
//! guarded `UPDATE … WHERE`, so two transactions racing for the last unit are serialized by
//! the row lock and the loser gets `409 insufficient_stock`; a CHECK constraint backs it up.
//!
//! Lifecycle: reserve on order placement, release on cancel/expiry, commit on shipment,
//! restock on a merchant-confirmed return, adjust for manual corrections.

use chrono::{DateTime, Utc};
use platform::Error;
use platform::db::TenantTx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::markets::invalid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MovementKind {
    Reserve,
    Release,
    Commit,
    Restock,
    Adjust,
}

impl MovementKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reserve => "reserve",
            Self::Release => "release",
            Self::Commit => "commit",
            Self::Restock => "restock",
            Self::Adjust => "adjust",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "reserve" => Self::Reserve,
            "release" => Self::Release,
            "commit" => Self::Commit,
            "restock" => Self::Restock,
            _ => Self::Adjust,
        }
    }

    /// (on_hand delta, reserved delta) for `qty` units.
    fn deltas(self, qty: i32) -> (i32, i32) {
        match self {
            Self::Reserve => (0, qty),
            Self::Release => (0, -qty),
            Self::Commit => (-qty, -qty),
            Self::Restock | Self::Adjust => (qty, 0),
        }
    }
}

/// What a movement belongs to: `("order", "<order id>")`, `("manual", "<idempotency key>")`, ...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovementRef<'a> {
    pub ref_type: &'a str,
    pub ref_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Level {
    pub variant_id: Uuid,
    pub on_hand: i32,
    pub reserved: i32,
    /// on_hand − reserved.
    pub available: i32,
    /// Untracked stock is never checked (services, dropshipping).
    pub track: bool,
    /// Sell below zero.
    pub allow_backorder: bool,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Movement {
    pub id: Uuid,
    pub variant_id: Uuid,
    pub kind: MovementKind,
    /// Units; a signed delta for `adjust`.
    pub quantity: i32,
    pub ref_type: String,
    pub ref_id: String,
    pub on_hand_after: i32,
    pub reserved_after: i32,
    pub actor: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The outcome of a movement request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub level: Level,
    /// False when this movement had already been recorded (idempotent replay).
    pub applied: bool,
}

pub const MAX_QUANTITY: i32 = 1_000_000;

fn check_ref(r: &MovementRef<'_>) -> Result<(), Error> {
    let type_ok = (1..=32).contains(&r.ref_type.len())
        && r.ref_type.as_bytes()[0].is_ascii_lowercase()
        && r.ref_type
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_');
    if !type_ok || !(1..=255).contains(&r.ref_id.len()) {
        return Err(invalid("invalid_reference", "invalid movement reference"));
    }
    Ok(())
}

async fn level(tx: &mut TenantTx, variant_id: Uuid) -> Result<Level, Error> {
    let r = sqlx::query!(
        r#"SELECT v.id, l.on_hand AS "on_hand?", l.reserved AS "reserved?", l.track AS "track?",
                  l.allow_backorder AS "allow_backorder?", l.updated_at AS "updated_at?"
           FROM variants v LEFT JOIN inventory_levels l ON l.variant_id = v.id
           WHERE v.id = $1"#,
        variant_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    let (on_hand, reserved) = (r.on_hand.unwrap_or(0), r.reserved.unwrap_or(0));
    Ok(Level {
        variant_id: r.id,
        on_hand,
        reserved,
        available: on_hand - reserved,
        track: r.track.unwrap_or(true),
        allow_backorder: r.allow_backorder.unwrap_or(false),
        updated_at: r.updated_at,
    })
}

/// The variant's product: `inventory.changed` carries it for per-product consumers (search).
async fn product_of(tx: &mut TenantTx, variant_id: Uuid) -> Result<Uuid, Error> {
    Ok(
        sqlx::query_scalar!("SELECT product_id FROM variants WHERE id = $1", variant_id)
            .fetch_one(&mut **tx)
            .await?,
    )
}

pub async fn get(tx: &mut TenantTx, variant_id: Uuid) -> Result<Level, Error> {
    level(tx, variant_id).await
}

/// Creates the variant's level row if needed and locks it. All stock changes of a variant are
/// serialized here, so replay, availability and absolute-count checks see settled data.
async fn lock_level(tx: &mut TenantTx, variant_id: Uuid) -> Result<(), Error> {
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "INSERT INTO inventory_levels (tenant_id, variant_id) SELECT $1, id FROM variants WHERE id = $2
         ON CONFLICT DO NOTHING",
        tenant_id,
        variant_id
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "SELECT variant_id FROM inventory_levels WHERE variant_id = $1 FOR UPDATE",
        variant_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(())
}

fn insufficient(kind: MovementKind) -> Error {
    match kind {
        MovementKind::Release | MovementKind::Commit => Error::Conflict {
            code: "insufficient_reserved",
            detail: "fewer units are reserved than requested".into(),
        },
        _ => Error::Conflict {
            code: "insufficient_stock",
            detail: "not enough stock".into(),
        },
    }
}

/// Records one movement and applies it to the level. Replays of the same identity return the
/// current level with `applied: false`. Errors leave the transaction to be rolled back.
pub async fn apply(
    tx: &mut TenantTx,
    actor: &str,
    kind: MovementKind,
    r: &MovementRef<'_>,
    variant_id: Uuid,
    quantity: i32,
    note: Option<&str>,
) -> Result<Moved, Error> {
    check_ref(r)?;
    let qty_ok = match kind {
        MovementKind::Adjust => quantity != 0 && (-MAX_QUANTITY..=MAX_QUANTITY).contains(&quantity),
        _ => (1..=MAX_QUANTITY).contains(&quantity),
    };
    if !qty_ok {
        return Err(invalid("invalid_quantity", "quantity out of range"));
    }
    if note.is_some_and(|n| n.chars().count() > 500) {
        return Err(invalid(
            "invalid_note",
            "note must be at most 500 characters",
        ));
    }
    let tenant_id = tx.tenant_id();
    lock_level(tx, variant_id).await?;
    let replay = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM stock_movements WHERE kind = $1 AND ref_type = $2
                          AND ref_id = $3 AND variant_id = $4) AS "x!""#,
        kind.as_str(),
        r.ref_type,
        r.ref_id,
        variant_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if replay {
        return Ok(Moved {
            level: level(tx, variant_id).await?,
            applied: false,
        });
    }
    let (d_on_hand, d_reserved) = kind.deltas(quantity);
    let after = sqlx::query!(
        "UPDATE inventory_levels SET on_hand = on_hand + $2, reserved = reserved + $3,
                                     updated_at = now()
         WHERE variant_id = $1
           AND reserved + $3 >= 0
           AND (NOT track OR allow_backorder
                OR (on_hand + $2 >= 0 AND reserved + $3 <= on_hand + $2))
         RETURNING on_hand, reserved",
        variant_id,
        d_on_hand,
        d_reserved
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| insufficient(kind))?;
    sqlx::query!(
        "INSERT INTO stock_movements (id, tenant_id, variant_id, kind, quantity, ref_type, ref_id,
                                      on_hand_after, reserved_after, actor, note)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        crate::id::new_id(),
        tenant_id,
        variant_id,
        kind.as_str(),
        quantity,
        r.ref_type,
        r.ref_id,
        after.on_hand,
        after.reserved,
        actor,
        note
    )
    .execute(&mut **tx)
    .await?;
    let now = level(tx, variant_id).await?;
    let before = json!({
        "on_hand": now.on_hand - d_on_hand,
        "reserved": now.reserved - d_reserved,
        "available": (now.on_hand - d_on_hand) - (now.reserved - d_reserved),
    });
    let product_id = product_of(tx, variant_id).await?;
    platform::queue::publish(
        &mut **tx,
        "inventory.changed",
        &json!({
            "variant_id": variant_id,
            "product_id": product_id,
            "kind": kind,
            "quantity": quantity,
            "ref_type": r.ref_type,
            "ref_id": r.ref_id,
            "before": before,
            "after": { "on_hand": now.on_hand, "reserved": now.reserved, "available": now.available },
        }),
    )
    .await?;
    Ok(Moved {
        level: now,
        applied: true,
    })
}

pub async fn reserve(
    tx: &mut TenantTx,
    r: &MovementRef<'_>,
    variant_id: Uuid,
    qty: i32,
) -> Result<Moved, Error> {
    apply(
        tx,
        "system",
        MovementKind::Reserve,
        r,
        variant_id,
        qty,
        None,
    )
    .await
}

pub async fn release(
    tx: &mut TenantTx,
    r: &MovementRef<'_>,
    variant_id: Uuid,
    qty: i32,
) -> Result<Moved, Error> {
    apply(
        tx,
        "system",
        MovementKind::Release,
        r,
        variant_id,
        qty,
        None,
    )
    .await
}

pub async fn commit(
    tx: &mut TenantTx,
    r: &MovementRef<'_>,
    variant_id: Uuid,
    qty: i32,
) -> Result<Moved, Error> {
    apply(tx, "system", MovementKind::Commit, r, variant_id, qty, None).await
}

pub async fn restock(
    tx: &mut TenantTx,
    actor: &str,
    r: &MovementRef<'_>,
    variant_id: Uuid,
    qty: i32,
) -> Result<Moved, Error> {
    apply(tx, actor, MovementKind::Restock, r, variant_id, qty, None).await
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Adjustment {
    /// Units to add (negative: remove). Exactly one of `delta` and `on_hand`.
    pub delta: Option<i32>,
    /// Set the counted stock; the difference is recorded as the delta.
    pub on_hand: Option<i32>,
    pub note: Option<String>,
}

/// A manual correction (Admin API), audited. `ref_id` should be the request's idempotency key.
pub async fn adjust(
    tx: &mut TenantTx,
    actor: &str,
    variant_id: Uuid,
    ref_id: &str,
    input: &Adjustment,
) -> Result<Moved, Error> {
    // Lock first: an absolute count computes its delta from the settled level.
    lock_level(tx, variant_id).await?;
    let current = level(tx, variant_id).await?;
    let delta = match (input.delta, input.on_hand) {
        (Some(d), None) => d,
        (None, Some(target)) if (0..=MAX_QUANTITY).contains(&target) => target
            .checked_sub(current.on_hand)
            .ok_or_else(|| invalid("invalid_quantity", "adjustment out of range"))?,
        (None, Some(_)) => return Err(invalid("invalid_quantity", "on_hand out of range")),
        _ => {
            return Err(invalid(
                "invalid_adjustment",
                "give exactly one of delta and on_hand",
            ));
        }
    };
    if delta == 0 {
        return Ok(Moved {
            level: current,
            applied: false,
        });
    }
    let r = MovementRef {
        ref_type: "manual",
        ref_id,
    };
    let moved = apply(
        tx,
        actor,
        MovementKind::Adjust,
        &r,
        variant_id,
        delta,
        input.note.as_deref(),
    )
    .await
    .map_err(|e| match e {
        Error::Conflict {
            code: "insufficient_stock",
            ..
        } => Error::Conflict {
            code: "below_reserved",
            detail: "on hand cannot go below zero or below the reserved units".into(),
        },
        e => e,
    })?;
    if moved.applied {
        audit::record(
            tx,
            actor,
            "inventory.adjusted",
            "variant",
            Some(&variant_id.to_string()),
            &json!({ "delta": delta, "note": input.note, "before": current, "after": moved.level }),
        )
        .await?;
    }
    Ok(moved)
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelSettings {
    pub track: bool,
    pub allow_backorder: bool,
}

/// Changes whether a variant's stock is tracked and may go below zero. `409 below_reserved`
/// when turning checks on while stock is already oversold.
pub async fn update_settings(
    tx: &mut TenantTx,
    actor: &str,
    variant_id: Uuid,
    input: &LevelSettings,
) -> Result<Level, Error> {
    let before = level(tx, variant_id).await?;
    sqlx::query!(
        "INSERT INTO inventory_levels (tenant_id, variant_id, track, allow_backorder)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (tenant_id, variant_id) DO UPDATE SET track = $3, allow_backorder = $4,
                                                          updated_at = now()",
        tx.tenant_id(),
        variant_id,
        input.track,
        input.allow_backorder
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        if e.as_database_error().and_then(|d| d.constraint())
            == Some("inventory_levels_no_oversell")
        {
            Error::Conflict {
                code: "below_reserved",
                detail: "stock is oversold; adjust on hand before enabling checks".into(),
            }
        } else {
            e.into()
        }
    })?;
    let after = level(tx, variant_id).await?;
    audit::record(
        tx,
        actor,
        "inventory.settings_updated",
        "variant",
        Some(&variant_id.to_string()),
        &json!({ "before": before, "after": after }),
    )
    .await?;
    let product_id = product_of(tx, variant_id).await?;
    platform::queue::publish(
        &mut **tx,
        "inventory.changed",
        &json!({
            "variant_id": variant_id, "product_id": product_id, "kind": "settings",
            "before": { "track": before.track, "allow_backorder": before.allow_backorder,
                        "available": before.available },
            "after": { "track": after.track, "allow_backorder": after.allow_backorder,
                       "available": after.available },
        }),
    )
    .await?;
    Ok(after)
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LevelRow {
    pub product_id: Uuid,
    pub sku: String,
    #[serde(flatten)]
    pub level: Level,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LevelPage {
    pub items: Vec<LevelRow>,
    pub next_cursor: Option<Uuid>,
}

/// Every variant (with default levels where none are stored), by variant id.
pub async fn list(
    tx: &mut TenantTx,
    product_id: Option<Uuid>,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<LevelPage, Error> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    let rows = sqlx::query!(
        r#"SELECT v.id, v.product_id, v.sku, l.on_hand AS "on_hand?", l.reserved AS "reserved?",
                  l.track AS "track?", l.allow_backorder AS "allow_backorder?",
                  l.updated_at AS "updated_at?"
           FROM variants v LEFT JOIN inventory_levels l ON l.variant_id = v.id
           WHERE ($1::uuid IS NULL OR v.product_id = $1) AND ($2::uuid IS NULL OR v.id > $2)
           ORDER BY v.id LIMIT $3"#,
        product_id,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items: Vec<LevelRow> = rows
        .into_iter()
        .map(|r| {
            let (on_hand, reserved) = (r.on_hand.unwrap_or(0), r.reserved.unwrap_or(0));
            LevelRow {
                product_id: r.product_id,
                sku: r.sku,
                level: Level {
                    variant_id: r.id,
                    on_hand,
                    reserved,
                    available: on_hand - reserved,
                    track: r.track.unwrap_or(true),
                    allow_backorder: r.allow_backorder.unwrap_or(false),
                    updated_at: r.updated_at,
                },
            }
        })
        .collect();
    let limit = usize::try_from(limit).unwrap_or(100);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].level.variant_id);
    items.truncate(limit);
    Ok(LevelPage { items, next_cursor })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MovementPage {
    pub items: Vec<Movement>,
    pub next_cursor: Option<Uuid>,
}

/// A variant's movements, newest first.
pub async fn movements(
    tx: &mut TenantTx,
    variant_id: Uuid,
    cursor: Option<Uuid>,
    limit: i64,
) -> Result<MovementPage, Error> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("invalid_limit", "limit must be between 1 and 100"));
    }
    level(tx, variant_id).await?;
    let rows = sqlx::query!(
        "SELECT id, variant_id, kind, quantity, ref_type, ref_id, on_hand_after, reserved_after,
                actor, note, created_at
         FROM stock_movements WHERE variant_id = $1 AND ($2::uuid IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3",
        variant_id,
        cursor,
        limit + 1
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut items: Vec<Movement> = rows
        .into_iter()
        .map(|r| Movement {
            id: r.id,
            variant_id: r.variant_id,
            kind: MovementKind::parse(&r.kind),
            quantity: r.quantity,
            ref_type: r.ref_type,
            ref_id: r.ref_id,
            on_hand_after: r.on_hand_after,
            reserved_after: r.reserved_after,
            actor: r.actor,
            note: r.note,
            created_at: r.created_at,
        })
        .collect();
    let limit = usize::try_from(limit).unwrap_or(100);
    let next_cursor = (items.len() > limit).then(|| items[limit - 1].id);
    items.truncate(limit);
    Ok(MovementPage { items, next_cursor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas() {
        assert_eq!(MovementKind::Reserve.deltas(2), (0, 2));
        assert_eq!(MovementKind::Release.deltas(2), (0, -2));
        assert_eq!(MovementKind::Commit.deltas(2), (-2, -2));
        assert_eq!(MovementKind::Restock.deltas(2), (2, 0));
        assert_eq!(MovementKind::Adjust.deltas(-3), (-3, 0));
    }

    #[test]
    fn references() {
        assert!(
            check_ref(&MovementRef {
                ref_type: "order",
                ref_id: "1"
            })
            .is_ok()
        );
        assert!(
            check_ref(&MovementRef {
                ref_type: "return_line",
                ref_id: "x"
            })
            .is_ok()
        );
        for (t, id) in [("", "1"), ("Order", "1"), ("order", ""), ("_x", "1")] {
            assert!(
                check_ref(&MovementRef {
                    ref_type: t,
                    ref_id: id
                })
                .is_err(),
                "{t}"
            );
        }
    }
}
