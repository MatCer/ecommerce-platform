use std::collections::BTreeMap;

use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::catalog::check_text;
use crate::checkout::i18n_text;
use crate::consent::{self, Subject};
use crate::content::legal;
use crate::markets::invalid;
use crate::storefront::Context;
use crate::storefront::pages::HomePage;

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

impl Assignment {
    /// The edge's `x-experiment` value: `<id>:<version>:<arm>`.
    pub fn parse(v: &str) -> Option<Self> {
        let mut parts = v.splitn(3, ':');
        Some(Self {
            experiment_id: parts.next()?.parse().ok()?,
            version: parts.next()?.parse().ok()?,
            arm: parts.next()?.to_owned(),
        })
    }
}

type I18n = BTreeMap<String, String>;

/// What an arm changes, from its frozen `config` (SHOP-98). `{}` changes nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmConfig {
    hero: Option<HeroOverride>,
}

/// Home hero texts by locale (`{"cs": "..."}`), picked like other i18n texts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeroOverride {
    title: Option<I18n>,
    subtitle: Option<I18n>,
    cta: Option<I18n>,
}

const CONFIG: &str = "invalid_experiment_config";

impl ArmConfig {
    fn parse(config: &serde_json::Value) -> Result<Self, Error> {
        let c: Self =
            serde_json::from_value(config.clone()).map_err(|e| invalid(CONFIG, e.to_string()))?;
        if let Some(h) = &c.hero {
            for (field, texts) in [
                ("hero title", &h.title),
                ("hero subtitle", &h.subtitle),
                ("hero cta", &h.cta),
            ] {
                for text in texts.iter().flat_map(BTreeMap::values) {
                    check_text(field, CONFIG, text, 1, 200)?;
                }
            }
        }
        Ok(c)
    }

    pub fn apply_home(&self, ctx: &Context, home: &mut HomePage) {
        let Some(h) = &self.hero else { return };
        let text = |t: &Option<I18n>| t.as_ref().and_then(|t| i18n_text(ctx, t));
        if let Some(t) = text(&h.title) {
            home.hero.title = t;
        }
        if let Some(t) = text(&h.subtitle) {
            home.hero.subtitle = t;
        }
        if let Some(t) = text(&h.cta) {
            home.hero.cta.label = t;
        }
    }
}

/// The config of the arm `a` names when it is an arm of the tenant's running experiment at that
/// version; `None` (control) for anything else, and for a config the page model cannot apply.
pub async fn arm_config(tx: &mut TenantTx, a: &Assignment) -> Result<Option<ArmConfig>, Error> {
    let Some((exp, arms)) = running_with_arms(tx).await? else {
        return Ok(None);
    };
    if (exp.id, exp.version) != (a.experiment_id, a.version) {
        return Ok(None);
    }
    let Some(arm) = arms
        .as_array()
        .into_iter()
        .flatten()
        .find(|arm| arm["key"] == a.arm.as_str())
    else {
        return Ok(None);
    };
    match ArmConfig::parse(&arm["config"]) {
        Ok(c) => Ok(Some(c)),
        Err(e) => {
            tracing::warn!(experiment = %exp.id, arm = %a.arm, error = %e, "experiment arm config ignored; serving control");
            Ok(None)
        }
    }
}

#[derive(Clone)]
pub struct ExperimentKey {
    secret: Vec<u8>,
}

impl ExperimentKey {
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

pub async fn start(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    let status = sqlx::query_scalar!(
        "SELECT status FROM experiments WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    if status != "draft" {
        return Err(Error::Conflict {
            code: "experiment_not_draft",
            detail: format!("the experiment is {status}; only a draft can start"),
        });
    }
    if !legal::cookie_policy_covers_experiments(tx).await? {
        return Err(invalid(
            "cookie_policy_outdated",
            "publish the cookie page installed from the current legal template, in every market \
             locale, before starting an experiment",
        ));
    }
    sqlx::query!(
        "UPDATE experiments SET status = 'running' WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        if e.as_database_error().and_then(|d| d.constraint()) == Some("experiments_one_running") {
            Error::Conflict {
                code: "experiment_running",
                detail: "another experiment is running".into(),
            }
        } else {
            e.into()
        }
    })?;
    audit::record(
        tx,
        actor,
        "experiment.started",
        "experiment",
        Some(&id.to_string()),
        &serde_json::json!({}),
    )
    .await?;
    Ok(())
}

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
    if !legal::cookie_policy_covers_experiments(&mut tx).await? {
        return Ok(None);
    }
    consent::lock_anon(&mut tx, subject).await?;
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
