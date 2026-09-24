//! Idempotency keys (spec §8.1, A12).
//!
//! The key row is inserted first, in the same tenant transaction as the mutation, and the
//! response is stored before commit. A concurrent request with the same key blocks on the
//! primary key until the first transaction ends, then replays its stored response (or, if the
//! first one rolled back, runs normally). The same key with a different request is a 409.

use platform::Error;
use platform::db::TenantTx;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A response recorded for an earlier request with the same key.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub status: u16,
    pub body: Value,
}

/// Validates a client-supplied `Idempotency-Key`: 1-255 visible ASCII characters.
pub fn validate_key(key: &str) -> Result<(), Error> {
    if (1..=255).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(Error::BadRequest {
            code: "invalid_idempotency_key",
            detail: "Idempotency-Key must be 1-255 visible ASCII characters".into(),
        })
    }
}

/// Fingerprint of the request the key was first used with.
pub fn request_hash(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// Claims `key` for `operation`. `Ok(None)`: go ahead, then call [`finish`] before commit.
/// `Ok(Some(stored))`: the request already ran; answer with `stored`.
pub async fn begin(
    tx: &mut TenantTx,
    operation: &str,
    key: &str,
    hash: &str,
) -> Result<Option<Stored>, Error> {
    validate_key(key)?;
    let tenant_id = tx.tenant_id();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO idempotency_keys (tenant_id, operation, key, request_hash, status)
           VALUES ($1, $2, $3, $4, 'in_progress')
           ON CONFLICT (tenant_id, operation, key) DO NOTHING
           RETURNING true AS "inserted!""#,
        tenant_id,
        operation,
        key,
        hash
    )
    .fetch_optional(&mut **tx)
    .await?;
    if inserted.is_some() {
        return Ok(None);
    }

    let existing = sqlx::query!(
        "SELECT request_hash, status, response FROM idempotency_keys
         WHERE tenant_id = $1 AND operation = $2 AND key = $3",
        tenant_id,
        operation,
        key
    )
    .fetch_one(&mut **tx)
    .await?;
    if existing.request_hash != hash {
        return Err(Error::Conflict {
            code: "idempotency_conflict",
            detail: "this Idempotency-Key was used with a different request".into(),
        });
    }
    let stored = existing.response.as_ref().and_then(|r| {
        Some(Stored {
            status: u16::try_from(r.get("status")?.as_u64()?).ok()?,
            body: r.get("body")?.clone(),
        })
    });
    match (existing.status.as_str(), stored) {
        ("completed", Some(stored)) => Ok(Some(stored)),
        _ => Err(Error::Conflict {
            code: "idempotency_in_progress",
            detail: "a request with this Idempotency-Key is still in progress".into(),
        }),
    }
}

/// Stores the response for `key`; call in the same transaction as [`begin`], before commit.
pub async fn finish(
    tx: &mut TenantTx,
    operation: &str,
    key: &str,
    status: u16,
    body: &Value,
) -> Result<(), Error> {
    let tenant_id = tx.tenant_id();
    sqlx::query!(
        "UPDATE idempotency_keys SET status = 'completed', response = $4
         WHERE tenant_id = $1 AND operation = $2 AND key = $3",
        tenant_id,
        operation,
        key,
        json!({ "status": status, "body": body })
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert!(validate_key("7c1f9a3e-order-1").is_ok());
        for bad in ["", "has space", "ünicode", &"k".repeat(256)] {
            assert!(validate_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hash_is_stable_sha256() {
        assert_eq!(
            request_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
