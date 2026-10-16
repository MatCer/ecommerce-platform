//! A/B experiments (SHOP-96, `docs/research/ab-testing.md` slice 1). The edge asks [`enrol`]
//! before render and cache; a visitor is enrolled only with an analytics grant whose text
//! covers testing page variants ([`consent::covers_experiments`]). The arm is a keyed hash of
//! the experiment version and the analytics pseudonym, stored on first enrolment so it sticks.

use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::consent::{self, Subject};
use crate::markets::invalid;

/// The tenant's running experiment, as the edge sees it in `/internal/v1/resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExperimentRef {
    pub id: Uuid,
    pub version: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Assignment {
    pub experiment_id: Uuid,
    pub version: i32,
    pub arm: String,
}

/// `EXPERIMENTS_SECRET`, the arm hash key. Not `Debug`: never logged.
#[derive(Clone)]
pub struct ExperimentKey {
    secret: Vec<u8>,
}

impl ExperimentKey {
    /// `secret` must be at least 32 bytes (checked by the config loader).
    pub fn new(secret: &[u8]) -> Self {
        Self {
            secret: secret.to_vec(),
        }
    }

    fn hash(&self, experiment: Uuid, version: i32, anon_id: &str) -> u64 {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
        mac.update(format!("experiment-arm:{experiment}:{version}:{anon_id}").as_bytes());
        let digest = mac.finalize().into_bytes();
        let mut head = [0u8; 8];
        head.copy_from_slice(&digest[..8]);
        u64::from_be_bytes(head)
    }
}

#[derive(Deserialize)]
struct Arm {
    key: String,
    weight: u64,
}

/// The arm whose cumulative weight range (in stored order) holds `hash % total weight`.
fn pick(arms: &[Arm], hash: u64) -> Option<&str> {
    let total: u64 = arms.iter().map(|a| a.weight).sum();
    let mut point = hash.checked_rem(total)?;
    arms.iter()
        .find(|a| {
            let inside = point < a.weight;
            point = point.saturating_sub(a.weight);
            inside
        })
        .map(|a| a.key.as_str())
}

pub async fn running(tx: &mut TenantTx) -> Result<Option<ExperimentRef>, Error> {
    Ok(running_with_arms(tx).await?.map(|(r, _)| r))
}

async fn running_with_arms(
    tx: &mut TenantTx,
) -> Result<Option<(ExperimentRef, serde_json::Value)>, Error> {
    Ok(
        sqlx::query!("SELECT id, version, arms FROM experiments WHERE status = 'running'")
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| {
                (
                    ExperimentRef {
                        id: r.id,
                        version: r.version,
                    },
                    r.arms,
                )
            }),
    )
}

/// Enrols the anonymous consent subject `subject` (the `__Secure-consent_id` cookie value) in
/// the tenant's running experiment. `None` = control: no key, no running experiment, or no
/// analytics grant covering experiments; nothing is written then.
pub async fn enrol(
    db: &PgPool,
    key: Option<&ExperimentKey>,
    tenant_id: Uuid,
    subject: &str,
) -> Result<Option<Assignment>, Error> {
    if !consent::well_formed_anon(subject) {
        return Err(invalid(
            "invalid_subject",
            "subject must be 32 lowercase hex characters",
        ));
    }
    let Some(key) = key else {
        return Ok(None);
    };
    let mut tx = tenant_tx(db, tenant_id).await?;
    let Some((exp, arms)) = running_with_arms(&mut tx).await? else {
        return Ok(None);
    };
    if !consent::analytics_covers_experiments(&mut tx, &Subject::Anon(subject.into())).await? {
        return Ok(None);
    }
    let arms: Vec<Arm> = serde_json::from_value(arms)
        .map_err(|e| Error::Internal(format!("experiment {} arms: {e}", exp.id)))?;
    let anon_id = crate::analytics::anon_id(tenant_id, subject);
    let arm = pick(&arms, key.hash(exp.id, exp.version, &anon_id))
        .ok_or_else(|| Error::Internal(format!("experiment {} has no weight", exp.id)))?;
    sqlx::query!(
        "INSERT INTO experiment_assignments (tenant_id, experiment_id, anon_id, arm)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        tenant_id,
        exp.id,
        anon_id,
        arm
    )
    .execute(&mut *tx)
    .await?;
    let stored = sqlx::query_scalar!(
        "SELECT arm FROM experiment_assignments WHERE experiment_id = $1 AND anon_id = $2",
        exp.id,
        anon_id
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(Assignment {
        experiment_id: exp.id,
        version: exp.version,
        arm: stored,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arms(weights: &[(&str, u64)]) -> Vec<Arm> {
        weights
            .iter()
            .map(|(k, w)| Arm {
                key: (*k).into(),
                weight: *w,
            })
            .collect()
    }

    #[test]
    fn pick_walks_the_weights_in_stored_order() {
        let a = arms(&[("control", 50), ("b", 30), ("c", 20)]);
        let picked: Vec<_> = [0, 49, 50, 79, 80, 99, 100, 150]
            .into_iter()
            .map(|h| pick(&a, h).unwrap())
            .collect();
        assert_eq!(
            picked,
            ["control", "control", "b", "b", "c", "c", "control", "b"]
        );
        assert_eq!(pick(&a, u64::MAX), Some("control"), "u64::MAX % 100 = 15");
        assert_eq!(pick(&[], 7), None);
    }

    #[test]
    fn hashed_picks_are_deterministic_and_follow_the_weights() {
        let key = ExperimentKey::new(b"unit-test-experiments-secret-0123456789");
        let experiment = Uuid::nil();
        let a = arms(&[("control", 1), ("b", 3)]);
        let mut b = 0;
        for i in 0..4000 {
            let anon = format!("{i:032x}");
            let first = pick(&a, key.hash(experiment, 1, &anon)).unwrap();
            assert_eq!(pick(&a, key.hash(experiment, 1, &anon)), Some(first));
            if first == "b" {
                b += 1;
            }
        }
        assert!((2850..=3150).contains(&b), "b got {b} of 4000");
        assert_ne!(key.hash(experiment, 1, "x"), key.hash(experiment, 2, "x"));
    }
}
