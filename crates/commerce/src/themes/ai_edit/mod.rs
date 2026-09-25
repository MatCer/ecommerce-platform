//! AI theme editing (WP24, spec §12.3, A6): a merchant's prompt becomes a run; the worker's
//! agent loop edits an in-memory copy of the base revision's source through contract-bound
//! file tools ([`tools`]) and checks it through the WP23 builder (each `run_checks` is a
//! `change = 'ai'` revision built and gated in the sandbox). The loop runs outside the
//! sandbox and only exchanges files with it.
//!
//! Hard limits per run ([`Limits`]): model turns, check runs (1 + 3 repairs), tokens, cost,
//! wall clock, the tenant's monthly quota; cancellation is honoured between turns and while a
//! build is checked. The run keeps the transcript, the tool steps, the diff and the last check
//! report. The staff accepts a succeeded run (its final revision may then be published through
//! the normal WP23 path) or discards it; an unaccepted AI revision can never be published.
//!
//! See `docs/decisions/ai-theme-editing.md`.

pub mod fake;
pub mod tools;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use platform::Error;
use platform::ai::{AiError, Client, Conversation, Failure};
use platform::db::{TenantTx, tenant_tx};
use platform::queue::{self, NewJob};
use platform::storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

pub use fake::agent as fake_agent;

use super::archive::Source;
use super::revisions::{
    Change, active_id, check_pages, create, ensure_validated_base, revision_source,
};
use crate::ai::{self, Ai};
use crate::audit;
use crate::markets::invalid;

/// Worker job: run the agent loop of `{run_id}` (tenant job).
pub const JOB: &str = "themes.ai_edit";
/// Metering key (`ai_usage.feature`) and fake-agent name.
pub const FEATURE: &str = "theme_edit";
const SYSTEM: &str = include_str!("system.md");
const TASK: &str = "Implement the merchant's request (`request` below) in this shop's theme. \
`locale` is the shop's main language; `pages` are real paths of this shop you can use in the \
functional check.";
const MAX_PROMPT_CHARS: usize = 4_000;
const MAX_DIFF_BYTES: usize = 1_000_000;
/// Check failures handed back to the model (count, characters each).
const MAX_FAILURES: usize = 20;
const MAX_FAILURE_CHARS: usize = 1_500;

/// Hard limits of one run (spec §12.3: 25 tool turns, 3 check-repair cycles).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Model calls.
    pub max_turns: i32,
    /// `run_checks` builds: the first check plus 3 repairs.
    pub max_checks: i32,
    /// Tokens of all calls (input incl. cache reads/writes + output).
    pub max_tokens: i64,
    /// USD micros at the configured list prices.
    pub max_cost_micros: i64,
    /// Wall clock of the whole run, builds included.
    pub timeout: Duration,
    /// How often a running build and cancellation are polled.
    pub poll: Duration,
    /// `max_tokens` of one model turn (a turn may write whole files).
    pub max_output_tokens: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_turns: 25,
            max_checks: 4,
            max_tokens: 3_000_000,
            max_cost_micros: 8_000_000,
            timeout: Duration::from_secs(45 * 60),
            poll: Duration::from_secs(3),
            max_output_tokens: 32_000,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Runs (API side)

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRun {
    /// What the merchant wants changed (1–4000 characters).
    pub prompt: String,
    /// The revision to start from; default: the active one. Must have passed the checks.
    pub base_revision_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunSummary {
    pub id: Uuid,
    pub prompt: String,
    /// `queued`, `running`, `succeeded`, `failed`, `cancelled`, `accepted` or `discarded`.
    pub status: String,
    pub cancel_requested: bool,
    pub base_revision_id: Uuid,
    /// The latest checked revision (the one accept makes publishable).
    pub revision_id: Option<Uuid>,
    pub revision_number: Option<i32>,
    pub model: Option<String>,
    pub turns: i32,
    pub checks_run: i32,
    pub tokens: i64,
    pub cost_micros: i64,
    /// Why the run failed (`turn_limit: …`, `checks_failed: …`), empty otherwise.
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// One tool call of the agent, as shown in the admin.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Step {
    pub at: DateTime<Utc>,
    /// `list_files`, `read_file`, `write_file`, `delete_file` or `run_checks`.
    pub tool: String,
    pub path: Option<String>,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunLimits {
    pub max_turns: i32,
    pub max_checks: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunDetail {
    pub run: RunSummary,
    /// The agent's closing message for the merchant.
    pub summary: Option<String>,
    pub steps: Vec<Step>,
    /// Unified diff from the base revision's source to the run's result.
    pub diff: Option<String>,
    /// The builder's report of the last check (`steps`, `failures`, …).
    #[schema(value_type = Option<Object>)]
    pub report: Option<Value>,
    pub limits: RunLimits,
}

struct Row {
    summary: RunSummary,
    summary_text: Option<String>,
    steps: Value,
    diff: Option<String>,
    report: Option<Value>,
}

macro_rules! select_run {
    ($tail:literal) => {
        concat!(
            "SELECT r.id, r.prompt, r.status, r.cancel_requested, r.base_revision_id,
                    r.revision_id, v.number AS revision_number, r.model, r.turns, r.checks_run,
                    r.tokens, r.cost_micros, r.error, r.created_by, r.created_at, r.updated_at,
                    r.finished_at, r.summary, r.steps, r.diff, r.report
             FROM ai_theme_runs r LEFT JOIN theme_revisions v ON v.id = r.revision_id ",
            $tail
        )
    };
}

fn row(r: &sqlx::postgres::PgRow) -> Result<Row, sqlx::Error> {
    use sqlx::Row as _;
    Ok(Row {
        summary: RunSummary {
            id: r.try_get("id")?,
            prompt: r.try_get("prompt")?,
            status: r.try_get("status")?,
            cancel_requested: r.try_get("cancel_requested")?,
            base_revision_id: r.try_get("base_revision_id")?,
            revision_id: r.try_get("revision_id")?,
            revision_number: r.try_get("revision_number")?,
            model: r.try_get("model")?,
            turns: r.try_get("turns")?,
            checks_run: r.try_get("checks_run")?,
            tokens: r.try_get("tokens")?,
            cost_micros: r.try_get("cost_micros")?,
            error: r.try_get("error")?,
            created_by: r.try_get("created_by")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            finished_at: r.try_get("finished_at")?,
        },
        summary_text: r.try_get("summary")?,
        steps: r.try_get("steps")?,
        diff: r.try_get("diff")?,
        report: r.try_get("report")?,
    })
}

async fn fetch(tx: &mut TenantTx, id: Uuid, lock: bool) -> Result<Row, Error> {
    let sql = if lock {
        select_run!("WHERE r.id = $1 FOR UPDATE OF r")
    } else {
        select_run!("WHERE r.id = $1")
    };
    let r = sqlx::query(sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    Ok(row(&r)?)
}

/// Starts a run: validates the prompt and base, checks the AI quota, queues the agent loop.
pub async fn start(
    tx: &mut TenantTx,
    ai: &Ai,
    actor: &str,
    input: &NewRun,
) -> Result<RunSummary, Error> {
    let prompt = input.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(invalid(
            "invalid_prompt",
            format!("describe the change in 1 to {MAX_PROMPT_CHARS} characters"),
        ));
    }
    ai::ensure_quota(tx, ai).await?;
    let base = match input.base_revision_id {
        Some(id) => id,
        None => active_id(tx).await?.ok_or(Error::Conflict {
            code: "no_active_theme",
            detail: "the shop has no active theme revision yet".into(),
        })?,
    };
    ensure_validated_base(tx, base).await?;
    let busy = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ai_theme_runs WHERE status IN ('queued', 'running'))
           AS "busy!""#
    )
    .fetch_one(&mut **tx)
    .await?;
    if busy {
        return Err(Error::Conflict {
            code: "ai_run_in_progress",
            detail: "an AI edit is already running for this shop; wait for it or cancel it".into(),
        });
    }
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO ai_theme_runs (id, tenant_id, base_revision_id, prompt, created_by)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        tx.tenant_id(),
        base,
        prompt,
        actor
    )
    .execute(&mut **tx)
    .await?;
    let mut job = NewJob::new(JOB, json!({ "run_id": id }));
    job.tenant_id = Some(tx.tenant_id());
    // A second attempt only marks a run whose worker died as interrupted (never re-spends).
    job.max_attempts = 2;
    job.idempotency_key = Some(format!("theme-ai:{id}"));
    queue::enqueue(&mut **tx, &job).await?;
    audit::record(
        tx,
        actor,
        "theme.ai_run_started",
        "ai_theme_run",
        Some(&id.to_string()),
        &json!({ "base_revision_id": base }),
    )
    .await?;
    Ok(fetch(tx, id, false).await?.summary)
}

/// The newest 50 runs.
pub async fn list(tx: &mut TenantTx) -> Result<Vec<RunSummary>, Error> {
    let rows = sqlx::query(select_run!("ORDER BY r.id DESC LIMIT 50"))
        .fetch_all(&mut **tx)
        .await?;
    rows.iter()
        .map(|r| Ok(row(r)?.summary))
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Error::from)
}

pub async fn detail(tx: &mut TenantTx, id: Uuid) -> Result<RunDetail, Error> {
    let r = fetch(tx, id, false).await?;
    let limits = Limits::default();
    Ok(RunDetail {
        run: r.summary,
        summary: r.summary_text,
        steps: serde_json::from_value(r.steps).unwrap_or_default(),
        diff: r.diff,
        report: r.report,
        limits: RunLimits {
            max_turns: limits.max_turns,
            max_checks: limits.max_checks,
        },
    })
}

/// A queued run is cancelled at once; a running one stops at its next turn or build poll.
pub async fn cancel(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<RunSummary, Error> {
    let r = fetch(tx, id, true).await?;
    match r.summary.status.as_str() {
        "queued" => {
            sqlx::query!(
                "UPDATE ai_theme_runs SET status = 'cancelled', cancel_requested = true,
                        finished_at = now(), updated_at = now() WHERE id = $1",
                id
            )
            .execute(&mut **tx)
            .await?;
        }
        "running" => {
            sqlx::query!(
                "UPDATE ai_theme_runs SET cancel_requested = true, updated_at = now() WHERE id = $1",
                id
            )
            .execute(&mut **tx)
            .await?;
        }
        other => return Err(transition(other, "cancelled")),
    }
    audit::record(
        tx,
        actor,
        "theme.ai_run_cancelled",
        "ai_theme_run",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    Ok(fetch(tx, id, false).await?.summary)
}

fn transition(from: &str, to: &str) -> Error {
    Error::Conflict {
        code: "invalid_transition",
        detail: format!("the AI edit is {from}; it cannot be {to}"),
    }
}

/// The staff reviewed the diff and report: the run's final (ready) revision may be published.
pub async fn accept(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<RunSummary, Error> {
    let r = fetch(tx, id, true).await?;
    if r.summary.status != "succeeded" {
        return Err(transition(&r.summary.status, "accepted"));
    }
    let revision = r.summary.revision_id.ok_or(Error::Internal(
        "succeeded AI run without a revision".into(),
    ))?;
    let status = sqlx::query_scalar!("SELECT status FROM theme_revisions WHERE id = $1", revision)
        .fetch_one(&mut **tx)
        .await?;
    if status != "ready" {
        return Err(Error::Conflict {
            code: "revision_not_ready",
            detail: format!("the checked revision is {status}"),
        });
    }
    sqlx::query!(
        "UPDATE ai_theme_runs SET status = 'accepted', updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "theme.ai_run_accepted",
        "ai_theme_run",
        Some(&id.to_string()),
        &json!({ "revision_id": revision }),
    )
    .await?;
    Ok(fetch(tx, id, false).await?.summary)
}

/// The staff does not want the change: its revisions stay unpublishable.
pub async fn discard(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<RunSummary, Error> {
    let r = fetch(tx, id, true).await?;
    if r.summary.status != "succeeded" {
        return Err(transition(&r.summary.status, "discarded"));
    }
    sqlx::query!(
        "UPDATE ai_theme_runs SET status = 'discarded', updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "theme.ai_run_discarded",
        "ai_theme_run",
        Some(&id.to_string()),
        &json!({}),
    )
    .await?;
    Ok(fetch(tx, id, false).await?.summary)
}

// ---------------------------------------------------------------------------------------
// Diffs

/// Unified diff of two theme sources (text files line by line, binaries by size).
pub fn diff(old: &BTreeMap<String, Vec<u8>>, new: &BTreeMap<String, Vec<u8>>) -> String {
    let mut paths: Vec<&String> = old.keys().chain(new.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut out = String::new();
    for path in paths {
        let (a, b) = (old.get(path), new.get(path));
        if a == b {
            continue;
        }
        fn text(f: Option<&Vec<u8>>) -> Option<&str> {
            match f {
                None => Some(""),
                Some(bytes) => std::str::from_utf8(bytes)
                    .ok()
                    .filter(|t| !t.contains('\0')),
            }
        }
        let (from, to) = (
            if a.is_some() {
                format!("a/{path}")
            } else {
                "/dev/null".into()
            },
            if b.is_some() {
                format!("b/{path}")
            } else {
                "/dev/null".into()
            },
        );
        match (text(a), text(b)) {
            (Some(x), Some(y)) => {
                let d = similar::TextDiff::configure()
                    .timeout(Duration::from_secs(1))
                    .diff_lines(x, y);
                out.push_str(
                    &d.unified_diff()
                        .context_radius(3)
                        .header(&from, &to)
                        .to_string(),
                );
            }
            _ => out.push_str(&format!(
                "Binary file {path}: {} → {} bytes\n",
                a.map_or(0, Vec::len),
                b.map_or(0, Vec::len)
            )),
        }
        if out.len() > MAX_DIFF_BYTES {
            let mut cut = MAX_DIFF_BYTES;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str("\n… diff truncated\n");
            break;
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevisionDiff {
    /// The revision compared against (the parent), if any.
    pub base_revision_id: Option<Uuid>,
    pub diff: String,
}

/// What a revision changed compared to its parent (empty diff for the first revision).
pub async fn revision_diff(
    tx: &mut TenantTx,
    storage: &Storage,
    id: Uuid,
) -> Result<RevisionDiff, Error> {
    let parent = sqlx::query_scalar!("SELECT parent_id FROM theme_revisions WHERE id = $1", id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::NotFound)?;
    let new = revision_source(tx, storage, id).await?;
    let old = match parent {
        Some(p) => revision_source(tx, storage, p).await?,
        None => new.clone(),
    };
    Ok(RevisionDiff {
        base_revision_id: parent,
        diff: diff(&old.files, &new.files),
    })
}

// ---------------------------------------------------------------------------------------
// The agent loop (worker)

/// How the loop ended.
#[derive(Debug)]
enum End {
    /// The model stopped with a message for the merchant.
    Stopped(String),
    Failed(&'static str, String),
    Cancelled,
}

struct Checked {
    files: BTreeMap<String, Vec<u8>>,
    ready: bool,
    report: Value,
    outcome: tools::Outcome,
}

struct Agent<'a> {
    db: &'a PgPool,
    storage: &'a Storage,
    ai: &'a Ai,
    tenant: Uuid,
    id: Uuid,
    actor: String,
    prompt: String,
    base_revision: Uuid,
    limits: Limits,
    started: Instant,
    base: BTreeMap<String, Vec<u8>>,
    files: BTreeMap<String, Vec<u8>>,
    messages: Vec<Value>,
    steps: Vec<Step>,
    turns: i32,
    checks_run: i32,
    tokens: i64,
    cost: i64,
    model: Option<String>,
    last_check: Option<Checked>,
}

/// Runs the agent loop of a run (the `themes.ai_edit` job). Errors are infrastructure trouble
/// (database, storage); everything the model or the gates do ends the run with a status.
pub async fn run(
    db: &PgPool,
    storage: &Storage,
    ai: &Ai,
    tenant: Uuid,
    id: Uuid,
    limits: Limits,
) -> Result<(), Error> {
    let mut tx = tenant_tx(db, tenant).await?;
    let r = fetch(&mut tx, id, true).await?;
    match r.summary.status.as_str() {
        "queued" => {}
        "running" => {
            // The worker that ran it died (the job was retried): never resume and re-spend.
            finish_failed(
                &mut tx,
                id,
                "interrupted: the worker stopped during the run; start it again",
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
        _ => return Ok(()),
    }
    sqlx::query!(
        "UPDATE ai_theme_runs SET status = 'running', updated_at = now() WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await?;
    let source = revision_source(&mut tx, storage, r.summary.base_revision_id).await?;
    let pages = check_pages(&mut tx).await?;
    let locale =
        sqlx::query_scalar!("SELECT default_locale FROM markets ORDER BY created_at LIMIT 1")
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;

    let first = json!({
        "role": "user",
        "content": platform::ai::user_content(
            TASK,
            &json!({ "request": r.summary.prompt, "locale": locale, "pages": pages }),
        ),
    });
    let mut agent = Agent {
        db,
        storage,
        ai,
        tenant,
        id,
        actor: r.summary.created_by,
        prompt: r.summary.prompt,
        base_revision: r.summary.base_revision_id,
        limits,
        started: Instant::now(),
        base: source.files.clone(),
        files: source.files,
        messages: vec![first],
        steps: Vec::new(),
        turns: 0,
        checks_run: 0,
        tokens: 0,
        cost: 0,
        model: None,
        last_check: None,
    };
    let end = match ai.client.clone() {
        Some(client) => agent.run_loop(&client).await?,
        None => End::Failed("ai_unavailable", "AI is not configured".into()),
    };
    agent.finish(end).await
}

async fn finish_failed(tx: &mut TenantTx, id: Uuid, error: &str) -> Result<(), Error> {
    sqlx::query!(
        "UPDATE ai_theme_runs SET status = 'failed', error = $2, finished_at = now(),
                updated_at = now() WHERE id = $1",
        id,
        error
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn failure_code(e: &AiError) -> &'static str {
    match e {
        AiError::Unavailable(_) | AiError::Rejected(_) => "ai_unavailable",
        AiError::Refused => "ai_refused",
        AiError::Truncated => "ai_truncated",
        AiError::InvalidOutput(_) => "ai_invalid_output",
    }
}

fn cap(s: &str, chars: usize) -> String {
    if s.chars().count() <= chars {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(chars.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

impl Agent<'_> {
    async fn run_loop(&mut self, client: &Client) -> Result<End, Error> {
        let tool_defs = tools::definitions();
        loop {
            if let Some(end) = self.interrupted().await? {
                return Ok(end);
            }
            if self.turns >= self.limits.max_turns {
                return Ok(End::Failed(
                    "turn_limit",
                    format!(
                        "the agent used all {} turns without finishing",
                        self.limits.max_turns
                    ),
                ));
            }
            if self.tokens >= self.limits.max_tokens || self.cost >= self.limits.max_cost_micros {
                return Ok(End::Failed(
                    "budget",
                    "the run used its token/cost budget".into(),
                ));
            }
            {
                let mut tx = tenant_tx(self.db, self.tenant).await?;
                let (used, quota, _) = ai::quota_state(&mut tx, self.ai).await?;
                tx.commit().await?;
                if used >= quota {
                    return Ok(End::Failed(
                        "ai_quota_exceeded",
                        "the monthly AI allowance of this shop is used up".into(),
                    ));
                }
            }
            let result = client
                .converse(&Conversation {
                    feature: FEATURE,
                    model: &self.ai.theme_model,
                    system: SYSTEM,
                    tools: &tool_defs,
                    messages: &self.messages,
                    max_tokens: self.limits.max_output_tokens,
                    effort: "high",
                })
                .await;
            let (usage, model) = match &result {
                Ok(t) => (t.usage, t.model.clone()),
                Err(Failure { usage, model, .. }) => (*usage, model.clone()),
            };
            let model = if model.is_empty() {
                self.ai.theme_model.clone()
            } else {
                model
            };
            self.cost += ai::record_usage(
                self.db,
                self.ai,
                self.tenant,
                &self.actor,
                FEATURE,
                &model,
                &usage,
            )
            .await?;
            self.tokens += i64::try_from(usage.total()).unwrap_or(i64::MAX);
            self.turns += 1;
            self.model = Some(model);
            let turn = match result {
                Ok(t) => t,
                Err(f) => {
                    self.save().await?;
                    return Ok(End::Failed(failure_code(&f.error), f.error.to_string()));
                }
            };
            self.messages
                .push(json!({ "role": "assistant", "content": turn.content }));
            let calls: Vec<Value> = turn.tool_uses().cloned().collect();
            if turn.stop_reason != "tool_use" || calls.is_empty() {
                self.save().await?;
                return Ok(End::Stopped(turn.text()));
            }
            // Every tool_use gets its result, all in one user message.
            let mut results = Vec::with_capacity(calls.len());
            let mut ended = None;
            for call in &calls {
                let name = call["name"].as_str().unwrap_or_default();
                let input = &call["input"];
                let out = if ended.is_some() {
                    tools::Outcome::err("not run: the run is stopping")
                } else if name == "run_checks" {
                    match self.run_checks().await? {
                        Ok(out) => out,
                        Err(end) => {
                            ended = Some(end);
                            tools::Outcome::err("not run: the run is stopping")
                        }
                    }
                } else {
                    tools::run(&mut self.files, name, input)
                };
                self.steps.push(Step {
                    at: Utc::now(),
                    tool: cap(name, 40),
                    path: input["path"].as_str().map(|p| cap(p, 240)),
                    ok: !out.is_error,
                    detail: cap(
                        if name == "read_file" && !out.is_error {
                            "read"
                        } else {
                            &out.content
                        },
                        300,
                    ),
                });
                results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call["id"],
                    "content": out.content,
                    "is_error": out.is_error,
                }));
            }
            self.messages
                .push(json!({ "role": "user", "content": results }));
            self.save().await?;
            if let Some(end) = ended {
                return Ok(end);
            }
            if self.checks_run >= self.limits.max_checks
                && self.last_check.as_ref().is_some_and(|c| !c.ready)
            {
                return Ok(End::Failed(
                    "repair_limit",
                    format!(
                        "the checks still failed after {} runs",
                        self.limits.max_checks
                    ),
                ));
            }
        }
    }

    /// Cancellation or the wall clock.
    async fn interrupted(&self) -> Result<Option<End>, Error> {
        if self.started.elapsed() >= self.limits.timeout {
            return Ok(Some(End::Failed(
                "timeout",
                format!(
                    "the run took longer than {} minutes",
                    self.limits.timeout.as_secs() / 60
                ),
            )));
        }
        let mut tx = tenant_tx(self.db, self.tenant).await?;
        let cancel = sqlx::query_scalar!(
            "SELECT cancel_requested FROM ai_theme_runs WHERE id = $1",
            self.id
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(cancel.then_some(End::Cancelled))
    }

    /// `run_checks`: the workspace becomes an AI revision, built and gated by the WP23 builder.
    /// `Err(end)`: the run was cancelled or timed out while waiting.
    async fn run_checks(&mut self) -> Result<Result<tools::Outcome, End>, Error> {
        if self.files == self.base {
            return Ok(Ok(tools::Outcome::err(
                "nothing changed yet: edit the theme before running the checks",
            )));
        }
        let has_check = self.files.iter().any(|(p, b)| {
            p.starts_with("checks/") && p.ends_with(".spec.ts") && self.base.get(p) != Some(b)
        });
        if !has_check {
            return Ok(Ok(tools::Outcome::err(
                "write a functional check for this change at checks/<name>.spec.ts first",
            )));
        }
        if let Some(c) = &self.last_check
            && c.files == self.files
        {
            return Ok(Ok(c.outcome.clone()));
        }
        if self.checks_run >= self.limits.max_checks {
            return Ok(Ok(tools::Outcome::err("no check runs left")));
        }
        let source = Source {
            files: self.files.clone(),
        };
        let revision = loop {
            if let Some(end) = self.interrupted().await? {
                return Ok(Err(end));
            }
            let mut tx = tenant_tx(self.db, self.tenant).await?;
            match create(
                &mut tx,
                self.storage,
                &self.actor,
                Change::Ai,
                Some(self.base_revision),
                &source,
                Some((self.id, &self.prompt)),
            )
            .await
            {
                Ok(r) => {
                    sqlx::query!(
                        "UPDATE ai_theme_runs SET checks_run = checks_run + 1, revision_id = $2,
                                updated_at = now() WHERE id = $1",
                        self.id,
                        r.id
                    )
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    break r;
                }
                // The tenant's build slots are taken (e.g. a manual edit): wait for one.
                Err(Error::Conflict {
                    code: "builds_in_progress",
                    ..
                }) => {
                    tx.rollback().await?;
                    tokio::time::sleep(self.limits.poll).await;
                }
                Err(e) => return Err(e),
            }
        };
        self.checks_run += 1;
        loop {
            tokio::time::sleep(self.limits.poll).await;
            if let Some(end) = self.interrupted().await? {
                return Ok(Err(end));
            }
            let mut tx = tenant_tx(self.db, self.tenant).await?;
            let r = sqlx::query!(
                "SELECT status, checks FROM theme_revisions WHERE id = $1",
                revision.id
            )
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            if !matches!(r.status.as_str(), "ready" | "failed") {
                continue;
            }
            let ready = r.status == "ready";
            let failures: Vec<String> = r.checks["failures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(MAX_FAILURES)
                .map(|f| cap(f, MAX_FAILURE_CHARS))
                .collect();
            let content = json!({
                "status": r.status,
                "revision": revision.number,
                "failures": failures,
            })
            .to_string();
            let outcome = if ready {
                tools::Outcome::ok(content)
            } else {
                tools::Outcome::err(content)
            };
            self.last_check = Some(Checked {
                files: self.files.clone(),
                ready,
                report: r.checks,
                outcome: outcome.clone(),
            });
            return Ok(Ok(outcome));
        }
    }

    async fn save(&self) -> Result<(), Error> {
        let mut tx = tenant_tx(self.db, self.tenant).await?;
        sqlx::query!(
            "UPDATE ai_theme_runs SET transcript = $2, steps = $3, turns = $4, tokens = $5,
                    cost_micros = $6, model = $7, updated_at = now() WHERE id = $1",
            self.id,
            Value::Array(self.messages.clone()),
            serde_json::to_value(&self.steps).unwrap_or_default(),
            self.turns,
            self.tokens,
            self.cost,
            self.model
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    fn checked(&self) -> bool {
        self.last_check
            .as_ref()
            .is_some_and(|c| c.files == self.files)
    }

    /// Final status, diff, report and summary. Changes the model left unchecked get one last
    /// check if a check run is left.
    async fn finish(mut self, mut end: End) -> Result<(), Error> {
        let summary = match &end {
            End::Stopped(text) => Some(text.clone()),
            _ => None,
        };
        if matches!(end, End::Stopped(_)) && self.files != self.base && !self.checked() {
            match self.run_checks().await? {
                Ok(out) if !self.checked() => end = End::Failed("unverified", out.content),
                Ok(_) => {}
                Err(stopped) => end = stopped,
            }
        }
        let ready = self.checked() && self.last_check.as_ref().is_some_and(|c| c.ready);
        let (status, error) = match end {
            End::Cancelled => ("cancelled", None),
            End::Failed(code, detail) => ("failed", Some(format!("{code}: {detail}"))),
            End::Stopped(_) if self.files == self.base => (
                "failed",
                Some("no_changes: the agent changed nothing".to_owned()),
            ),
            End::Stopped(_) if ready => ("succeeded", None),
            End::Stopped(_) => (
                "failed",
                Some("checks_failed: the last check did not pass".to_owned()),
            ),
        };
        let diff = diff(&self.base, &self.files);
        let report = self.last_check.as_ref().map(|c| c.report.clone());
        self.save().await?;
        let mut tx = tenant_tx(self.db, self.tenant).await?;
        sqlx::query!(
            "UPDATE ai_theme_runs SET status = $2, summary = $3, error = $4, diff = $5, report = $6,
                    finished_at = now(), updated_at = now() WHERE id = $1",
            self.id,
            status,
            summary.map(|s| cap(&s, 4_000)),
            error.map(|e| cap(&e, 1_000)),
            (!diff.is_empty()).then_some(diff),
            report
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(
            tenant = %self.tenant, run = %self.id, status, turns = self.turns,
            checks = self.checks_run, tokens = self.tokens, cost_micros = self.cost,
            "ai theme run finished"
        );
        Ok(())
    }
}
