use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use platform::queue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::catalog::code_valid;
use crate::consent::{self, Subject};
use crate::content::legal;
use crate::markets::invalid;
use crate::storefront::Context;
use crate::storefront::pages::{self, HeroCopy, HomePage};
use crate::themes;

mod readout;
mod stats;

pub use stats::{Basis, Need, ReadoutState, Side};
use stats::{Guardrail, Pair, Snapshot};

/// The tenant's running experiment, as the edge sees it in `/internal/v1/resolve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ExperimentRef {
    pub id: Uuid,
    pub version: i32,
    /// What theme code names the experiment by when it renders an arm's components (SHOP-100).
    pub name: String,
    /// The arm that renders another revision of the theme (a whole-theme test, SHOP-100).
    pub theme_arm: Option<ThemeArm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ThemeArm {
    pub arm: String,
    /// The revision's artifact, which the edge renders for this arm's visitors.
    pub artifact: String,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmConfig {
    hero: Option<HeroCopy>,
}

pub(crate) const CONFIG: &str = "invalid_experiment_config";

impl ArmConfig {
    fn parse(config: &serde_json::Value) -> Result<Self, Error> {
        let c: Self =
            serde_json::from_value(config.clone()).map_err(|e| invalid(CONFIG, e.to_string()))?;
        if let Some(h) = &c.hero {
            h.validate()?;
        }
        Ok(c)
    }

    pub fn apply_home(&self, ctx: &Context, home: &mut HomePage) {
        if let Some(h) = &self.hero {
            h.apply(ctx, home);
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
            tracing::warn!(
                experiment = %exp.id,
                arm = %a.arm,
                error = %e,
                "experiment arm config ignored; serving control"
            );
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Arm {
    key: String,
    weight: u32,
}

fn pick(arms: &[Arm], hash: u64) -> Option<&str> {
    let total: u64 = arms.iter().map(|a| u64::from(a.weight)).sum();
    let mut point = hash.checked_rem(total)?;
    arms.iter()
        .find(|a| {
            let inside = point < u64::from(a.weight);
            point = point.saturating_sub(u64::from(a.weight));
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
    Ok(sqlx::query!(
        r#"SELECT e.id, e.version, e.name, e.arms, e.theme_arm, r.artifact_id AS "theme_artifact?"
           FROM experiments e LEFT JOIN theme_revisions r ON r.id = e.theme_revision_id
           WHERE e.status = 'running'"#
    )
    .fetch_optional(&mut **tx)
    .await?
    .map(|r| {
        (
            ExperimentRef {
                id: r.id,
                version: r.version,
                name: r.name,
                theme_arm: r
                    .theme_arm
                    .zip(r.theme_artifact)
                    .map(|(arm, artifact)| ThemeArm { arm, artifact }),
            },
            r.arms,
        )
    }))
}

pub const GUARDRAILS_JOB: &str = "experiments.guardrails";
pub const CHANGED_EVENT: &str = "experiment.changed";
pub const GUARDRAIL_ACTOR: &str = "system:experiments";
const CONTROL: &str = "control";
const VARIANT: &str = "variant";

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NewExperiment {
    /// `^[a-z0-9][a-z0-9_-]{0,63}$`; a name used before gets its next version.
    pub name: String,
    pub change: NewChange,
}

/// What the variant arm changes; the control arm is the shop as it is.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NewChange {
    Hero {
        hero: HeroCopy,
    },
    /// A ready, published or superseded revision of the shop's theme that has its artifact.
    Theme {
        revision_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Experiment {
    pub id: Uuid,
    pub name: String,
    pub version: i32,
    pub status: Status,
    /// Set exactly when `status` is `stopped`.
    pub stop_reason: Option<StopReason>,
    /// When the variant became the shop's own: at its stop, or after it.
    pub promoted_at: Option<DateTime<Utc>>,
    pub change: Change,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    arms: Pair<Arm>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schema(as = ExperimentChange)]
pub enum Change {
    Hero {
        hero: HeroCopy,
    },
    Theme {
        revision_id: Uuid,
        revision_number: i32,
    },
    /// Theme code renders the arm (`experimentArm()`); it can be stopped, not promoted.
    Component,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ExperimentStatus)]
pub enum Status {
    Draft,
    Running,
    Stopped,
}

impl Status {
    fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "draft" => Ok(Self::Draft),
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            other => Err(Error::Internal(format!("stored experiment status {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Manual,
    Promoted,
    Srm,
    Harm,
    MaxDuration,
}

impl StopReason {
    const ALL: [Self; 5] = [
        Self::Manual,
        Self::Promoted,
        Self::Srm,
        Self::Harm,
        Self::MaxDuration,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Promoted => "promoted",
            Self::Srm => "srm",
            Self::Harm => "harm",
            Self::MaxDuration => "max_duration",
        }
    }

    fn parse(s: &str) -> Result<Self, Error> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| Error::Internal(format!("stored stop reason {s}")))
    }
}

struct Row {
    id: Uuid,
    name: String,
    version: i32,
    arms: Value,
    status: String,
    stop_reason: Option<String>,
    promoted_at: Option<DateTime<Utc>>,
    theme_arm: Option<String>,
    theme_revision_id: Option<Uuid>,
    revision_number: Option<i32>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
}

impl TryFrom<Row> for Experiment {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self, Error> {
        let change = match r.theme_revision_id.zip(r.revision_number) {
            Some((revision_id, revision_number)) => Change::Theme {
                revision_id,
                revision_number,
            },
            None => match r.arms.get(1).map(|arm| ArmConfig::parse(&arm["config"])) {
                Some(Ok(ArmConfig { hero: Some(hero) })) => Change::Hero { hero },
                _ => Change::Component,
            },
        };
        let [first, second]: [Arm; 2] = serde_json::from_value(r.arms)
            .map_err(|e| Error::Internal(format!("experiment {} arms: {e}", r.id)))?;
        let arms = if r.theme_arm.as_deref() == Some(first.key.as_str()) {
            Pair {
                control: second,
                variant: first,
            }
        } else {
            Pair {
                control: first,
                variant: second,
            }
        };
        Ok(Self {
            id: r.id,
            name: r.name,
            version: r.version,
            status: Status::parse(&r.status)?,
            stop_reason: r
                .stop_reason
                .as_deref()
                .map(StopReason::parse)
                .transpose()?,
            promoted_at: r.promoted_at,
            change,
            created_at: r.created_at,
            started_at: r.started_at,
            stopped_at: r.stopped_at,
            arms,
        })
    }
}

pub(crate) async fn fetch(tx: &mut TenantTx, id: Uuid) -> Result<Experiment, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT e.id, e.name, e.version, e.arms, e.status, e.stop_reason, e.promoted_at,
                  e.theme_arm, e.theme_revision_id, r.number AS "revision_number?", e.created_at,
                  e.started_at, e.stopped_at
           FROM experiments e LEFT JOIN theme_revisions r ON r.id = e.theme_revision_id
           WHERE e.id = $1"#,
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?
    .try_into()
}

/// An experiment and its readout; a draft has none.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ExperimentDetail {
    pub experiment: Experiment,
    pub readout: Option<Readout>,
}

/// The daily snapshot of a started experiment.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Readout {
    /// The UTC midnight the snapshot counts up to; once stopped, at most the stop plus 7 days.
    pub as_of: DateTime<Utc>,
    /// The first snapshot that can name a winner or no difference; absent when the experiment
    /// stopped less than 14 days after its start, so it can never decide.
    pub decides_from: Option<DateTime<Utc>>,
    /// Stopped, and every attribution window has closed.
    #[serde(rename = "final")]
    pub final_: bool,
    /// The converters each arm needs before the readout decides.
    pub min_converters: u64,
    pub state: ReadoutState,
    pub control: ArmReadout,
    pub variant: ArmReadout,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ArmReadout {
    /// Subjects assigned before the snapshot.
    pub assigned: u64,
    /// Assigned subjects whose 7-day attribution window has closed; only they are counted.
    pub subjects: u64,
    /// Counted subjects who bought within 7 days of their assignment.
    pub converters: u64,
    /// `converters / subjects`; null without subjects.
    pub conversion_rate: Option<f64>,
    /// The chance that this arm converts best.
    pub p_best: f64,
    /// The conversion rate lost, in expectation, by keeping this arm.
    pub expected_loss: f64,
}

impl ArmReadout {
    fn new(c: stats::Counts, a: stats::ArmStats) -> Self {
        Self {
            assigned: c.assigned,
            subjects: c.subjects,
            converters: c.converters,
            conversion_rate: (c.subjects > 0).then(|| c.converters as f64 / c.subjects as f64),
            p_best: a.p_best,
            expected_loss: a.expected_loss,
        }
    }
}

pub async fn get(
    tx: &mut TenantTx,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<ExperimentDetail, Error> {
    let experiment = fetch(tx, id).await?;
    let readout = snapshot(tx, &experiment, now).await?.map(|(w, s)| {
        let v = stats::readout(&s);
        Readout {
            as_of: w.as_of,
            decides_from: w.decides_from,
            final_: w.phase.is_final(),
            min_converters: stats::RULE.min_converters,
            state: v.state,
            control: ArmReadout::new(s.counts.control, v.arms.control),
            variant: ArmReadout::new(s.counts.variant, v.arms.variant),
        }
    });
    Ok(ExperimentDetail {
        experiment,
        readout,
    })
}

async fn snapshot(
    tx: &mut TenantTx,
    e: &Experiment,
    now: DateTime<Utc>,
) -> Result<Option<(stats::Window, Snapshot)>, Error> {
    let Some(started_at) = e.started_at else {
        return Ok(None);
    };
    let w = stats::window(started_at, e.stopped_at, now);
    let counts = readout::counts(tx, e, started_at, w.as_of).await?;
    Ok(Some((
        w,
        Snapshot {
            counts,
            weights: Pair {
                control: e.arms.control.weight,
                variant: e.arms.variant.weight,
            },
            mature_days: w.mature_days,
            phase: w.phase,
            trusted: e.stop_reason != Some(StopReason::Srm),
        },
    )))
}

pub async fn list(tx: &mut TenantTx) -> Result<Vec<Experiment>, Error> {
    sqlx::query_as!(
        Row,
        r#"SELECT e.id, e.name, e.version, e.arms, e.status, e.stop_reason, e.promoted_at,
                  e.theme_arm, e.theme_revision_id, r.number AS "revision_number?", e.created_at,
                  e.started_at, e.stopped_at
           FROM experiments e LEFT JOIN theme_revisions r ON r.id = e.theme_revision_id
           ORDER BY e.created_at DESC, e.id DESC LIMIT 100"#
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(Experiment::try_from)
    .collect()
}

pub async fn create(
    tx: &mut TenantTx,
    actor: &str,
    input: &NewExperiment,
) -> Result<Experiment, Error> {
    if !code_valid(&input.name) {
        return Err(invalid(
            "invalid_experiment_name",
            "the name is 1-64 lowercase letters, digits, '-' or '_', starting with a letter or \
             a digit",
        ));
    }
    let (kind, variant, revision) = match &input.change {
        NewChange::Hero { hero } => {
            hero.validate()?;
            let locales = crate::ai::market_locales(tx).await?;
            for (field, texts) in [
                ("title", &hero.title),
                ("subtitle", &hero.subtitle),
                ("cta", &hero.cta),
            ] {
                let Some(texts) = texts else { continue };
                let missing: Vec<&str> = locales
                    .iter()
                    .filter(|l| !texts.contains_key(*l))
                    .map(String::as_str)
                    .collect();
                if !missing.is_empty() {
                    return Err(invalid(
                        CONFIG,
                        format!(
                            "the hero {field} needs a text in every market language; missing: {}",
                            missing.join(", ")
                        ),
                    ));
                }
            }
            ("hero", json!({ "hero": hero }), None)
        }
        NewChange::Theme { revision_id } => {
            let r = sqlx::query!(
                "SELECT number, status, artifact_id FROM theme_revisions WHERE id = $1",
                revision_id
            )
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| {
                invalid(
                    "unknown_theme_revision",
                    "the shop has no theme revision with that id",
                )
            })?;
            if r.artifact_id.is_none()
                || !matches!(r.status.as_str(), "ready" | "published" | "superseded")
            {
                return Err(invalid(
                    "theme_revision_not_ready",
                    format!(
                        "revision #{} is {}; test a revision that passed its checks",
                        r.number, r.status
                    ),
                ));
            }
            ("theme", json!({}), Some(*revision_id))
        }
    };
    let arms = json!([
        { "key": CONTROL, "weight": 50, "config": {} },
        { "key": VARIANT, "weight": 50, "config": variant },
    ]);
    let created = sqlx::query!(
        "INSERT INTO experiments (tenant_id, name, version, arms, theme_arm, theme_revision_id)
         SELECT $1::uuid, $2::text, coalesce(max(version), 0) + 1, $3::jsonb, $4::text, $5::uuid
         FROM experiments WHERE name = $2
         RETURNING id, version",
        tx.tenant_id(),
        input.name,
        arms,
        revision.map(|_| VARIANT),
        revision
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| {
        if crate::unique_violation(&e) {
            Error::Conflict {
                code: "experiment_exists",
                detail: format!("{} was created at the same time; try again", input.name),
            }
        } else {
            e.into()
        }
    })?;
    audit::record(
        tx,
        actor,
        "experiment.created",
        "experiment",
        Some(&created.id.to_string()),
        &json!({ "name": input.name, "version": created.version, "kind": kind }),
    )
    .await?;
    fetch(tx, created.id).await
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
    .map_err(
        |e| match e.as_database_error().map(|d| (d.code(), d.constraint())) {
            Some((_, Some("experiments_one_running"))) => Error::Conflict {
                code: "experiment_running",
                detail: "another experiment is running".into(),
            },
            Some((Some(code), None)) if code == "23514" => Error::Conflict {
                code: "theme_revision_not_ready",
                detail: "the theme revision lost its artifact; create the experiment again with \
                         a revision that passed its checks"
                    .into(),
            },
            _ => e.into(),
        },
    )?;
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

pub async fn stop(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    stop_locked(tx, actor, id, StopReason::Manual, serde_json::Map::new()).await?;
    Ok(())
}

pub async fn promote(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<Experiment, Error> {
    lock(tx, id).await?;
    let e = fetch(tx, id).await?;
    if e.promoted_at.is_some() {
        return Ok(e);
    }
    if e.status == Status::Draft {
        return Err(Error::Conflict {
            code: "experiment_not_started",
            detail: "start the experiment before promoting its variant".into(),
        });
    }
    let (key, value) = match &e.change {
        Change::Component => {
            return Err(Error::Conflict {
                code: "experiment_not_promotable",
                detail: "theme code renders this variant; change the theme to keep it".into(),
            });
        }
        Change::Theme { revision_id, .. } => {
            themes::publish(tx, actor, *revision_id).await?;
            ("revision_id", json!(revision_id))
        }
        Change::Hero { hero } => {
            pages::merge_hero(tx, hero).await?;
            ("hero", json!(hero))
        }
    };
    let evidence = serde_json::Map::from_iter([(key.to_owned(), value)]);
    if e.status == Status::Running {
        stop_locked(tx, actor, id, StopReason::Promoted, evidence.clone()).await?;
    } else {
        sqlx::query!(
            "UPDATE experiments SET promoted_at = now() WHERE id = $1",
            id
        )
        .execute(&mut **tx)
        .await?;
        queue::publish(&mut **tx, CHANGED_EVENT, &json!({ "experiment_id": id })).await?;
    }
    audit::record(
        tx,
        actor,
        "experiment.promoted",
        "experiment",
        Some(&id.to_string()),
        &Value::Object(evidence),
    )
    .await?;
    fetch(tx, id).await
}

async fn lock(tx: &mut TenantTx, id: Uuid) -> Result<Status, Error> {
    let status = sqlx::query_scalar!(
        "SELECT status FROM experiments WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Status::parse(&status)
}

enum Stopped {
    Now,
    Already,
}

async fn stop_locked(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    reason: StopReason,
    mut evidence: serde_json::Map<String, Value>,
) -> Result<Stopped, Error> {
    match lock(tx, id).await? {
        Status::Draft => Err(Error::Conflict {
            code: "experiment_not_running",
            detail: "the experiment has not started".into(),
        }),
        Status::Stopped => Ok(Stopped::Already),
        Status::Running => {
            sqlx::query!(
                "UPDATE experiments SET status = 'stopped', stop_reason = $2::text,
                        promoted_at = CASE WHEN $2::text = 'promoted' THEN now() END
                 WHERE id = $1",
                id,
                reason.as_str()
            )
            .execute(&mut **tx)
            .await?;
            evidence.insert("reason".into(), json!(reason));
            audit::record(
                tx,
                actor,
                "experiment.stopped",
                "experiment",
                Some(&id.to_string()),
                &Value::Object(evidence),
            )
            .await?;
            queue::publish(&mut **tx, CHANGED_EVENT, &json!({ "experiment_id": id })).await?;
            Ok(Stopped::Now)
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GuardrailReport {
    pub stopped: u64,
}

pub async fn guardrails(db: &PgPool, now: DateTime<Utc>) -> Result<GuardrailReport, Error> {
    let tenants = sqlx::query_scalar!("SELECT id FROM platform.tenants ORDER BY id")
        .fetch_all(db)
        .await?;
    let mut report = GuardrailReport::default();
    let mut failed = 0;
    for tenant in tenants {
        match check_tenant(db, tenant, now).await {
            Ok(stopped) => report.stopped += u64::from(stopped),
            Err(e) => {
                failed += 1;
                tracing::error!(%tenant, error = %e, "experiment guardrails failed");
            }
        }
    }
    if failed > 0 {
        return Err(Error::Internal(format!(
            "experiment guardrails failed for {failed} tenants"
        )));
    }
    Ok(report)
}

async fn check_tenant(db: &PgPool, tenant: Uuid, now: DateTime<Utc>) -> Result<bool, Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let Some(id) = sqlx::query_scalar!("SELECT id FROM experiments WHERE status = 'running'")
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(false);
    };
    let e = fetch(&mut tx, id).await?;
    let Some(guardrail) = snapshot(&mut tx, &e, now)
        .await?
        .and_then(|(_, s)| stats::guardrail(&s))
    else {
        return Ok(false);
    };
    let (reason, evidence) = match guardrail {
        Guardrail::Srm { chi2 } => (StopReason::Srm, Some(("chi2", chi2))),
        Guardrail::Harm { p_worse } => (StopReason::Harm, Some(("p_worse", p_worse))),
        Guardrail::MaxDuration => (StopReason::MaxDuration, None),
    };
    let evidence = evidence
        .into_iter()
        .map(|(k, v)| (k.to_owned(), json!(v)))
        .collect();
    let stopped = stop_locked(&mut tx, GUARDRAIL_ACTOR, id, reason, evidence).await?;
    tx.commit().await?;
    Ok(matches!(stopped, Stopped::Now))
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

    #[sqlx::test(migrations = "../../migrations")]
    async fn every_stop_reason_is_stored_by_the_stop(db: PgPool) {
        let runtime = testkit::runtime_pool(&db, 2).await;
        let (tenant, _) = testkit::tenant(&runtime, "stop-reasons").await;
        for (version, reason) in (1..).zip(StopReason::ALL) {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO experiments (tenant_id, name, version, arms, status, started_at)
                 VALUES ($1, 'reasons', $2, $3, 'running', now()) RETURNING id",
            )
            .bind(tenant)
            .bind(version)
            .bind(json!([
                {"key": CONTROL, "weight": 1, "config": {}},
                {"key": VARIANT, "weight": 1, "config": {}},
            ]))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            stop_locked(&mut tx, "test", id, reason, serde_json::Map::new())
                .await
                .unwrap();
            assert_eq!(fetch(&mut tx, id).await.unwrap().stop_reason, Some(reason));
            tx.commit().await.unwrap();
        }
    }

    fn arms(weights: &[(&str, u32)]) -> Vec<Arm> {
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
