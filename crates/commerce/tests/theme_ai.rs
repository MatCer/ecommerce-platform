//! WP24 AI theme editing against a real Postgres as the runtime role: the agent loop with the
//! fake provider and scripted agents, a simulated builder answering the WP23 callbacks,
//! limits, cancellation, accept/discard, the publish gate and tenant isolation.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use commerce::ai::Ai;
use commerce::themes::ai_edit::{self, Limits, NewRun};
use commerce::themes::archive::{self, Source};
use commerce::themes::{self, StatusUpdate, ThemeKeys};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::Error;
use platform::ai::{Client, Fake};
use platform::db::tenant_tx;
use platform::storage::Storage;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const DEFAULT: &str = "d0000000000000000000000000000000";

fn keys() -> ThemeKeys {
    ThemeKeys::new(&[42u8; 32])
}

fn tokens() -> Value {
    json!({ "colors": { "buy": "#ff8800" }, "fonts": { "sans": "system-ui" }, "radius": { "md": "0.5rem" } })
}

fn default_source() -> Source {
    let mut s = Source::default();
    s.files
        .insert("theme.tokens.json".into(), archive::tokens_file(&tokens()));
    s.files.insert(
        "src/pages/index.astro".into(),
        b"---\nconst shop = {};\n---\n\n<Base shop={shop}>\n  <h1>Default</h1>\n</Base>\n".to_vec(),
    );
    s.files.insert("package.json".into(), b"{}\n".to_vec());
    s
}

fn artifact(id: &str) -> Vec<(String, Vec<u8>)> {
    let manifest = json!({
        "schema": 1, "id": id, "kind": "theme",
        "runtime": { "compatibility_date": "2026-09-21", "compatibility_flags": [],
                     "main": "entry.mjs", "modules": ["entry.mjs"] },
        "assets": { "/_astro/app.js": { "sha256": "00", "size": 2 } },
        "tokens": tokens(),
        "csp": { "script_hashes": [], "style_hashes": [] }
    });
    vec![
        (
            "manifest.json".into(),
            serde_json::to_vec(&manifest).unwrap(),
        ),
        ("server/entry.mjs".into(), b"export default {}".to_vec()),
        ("client/_astro/app.js".into(), b"1;".to_vec()),
    ]
}

async fn publish_default(runtime: &PgPool, storage: &Storage) {
    themes::register_artifact(
        runtime,
        storage,
        DEFAULT,
        themes::ArtifactKind::Theme,
        None,
        artifact(DEFAULT),
    )
    .await
    .unwrap();
    let key = "theme-sources/default/abc.tar.gz";
    storage
        .private
        .put(
            &Path::from(key),
            PutPayload::from(archive::write(&default_source())),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE platform.theme_artifacts SET source_key = $2 WHERE id = $1")
        .bind(DEFAULT)
        .bind(key)
        .execute(runtime)
        .await
        .unwrap();
    themes::publish_default(runtime, "platform", DEFAULT)
        .await
        .unwrap();
}

type Verdict = Arc<dyn Fn(usize, &Source) -> bool + Send + Sync>;

/// The WP23 builder for one tenant: every draft revision is built; `verdict(n, source)` decides
/// whether the n-th build (from 1) passes the gates.
fn spawn_builder(
    runtime: PgPool,
    storage: Storage,
    tenant: Uuid,
    verdict: Verdict,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut n = 0;
        loop {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM theme_revisions WHERE status = 'draft' ORDER BY number",
            )
            .fetch_all(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            for id in ids {
                n += 1;
                let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
                let gz = themes::source_archive(&mut tx, &storage, id).await.unwrap();
                let source = archive::read(&gz).unwrap();
                let status = |s: &str, checks: Value| StatusUpdate {
                    status: s.into(),
                    checks,
                };
                themes::builder_status(&mut tx, id, &status("building", json!({})))
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                if verdict(n, &source) {
                    let artifact_id = format!("a{n:031x}");
                    themes::attach_artifact(
                        &runtime,
                        &storage,
                        &keys(),
                        tenant,
                        id,
                        &artifact_id,
                        artifact(&artifact_id),
                        Utc::now(),
                    )
                    .await
                    .unwrap();
                    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
                    themes::builder_status(
                        &mut tx,
                        id,
                        &status("ready", json!({ "failures": [], "steps": [] })),
                    )
                    .await
                    .unwrap();
                    tx.commit().await.unwrap();
                } else {
                    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
                    themes::builder_status(
                        &mut tx,
                        id,
                        &status(
                            "failed",
                            json!({ "failures": [format!("functional checks: build {n} failed")] }),
                        ),
                    )
                    .await
                    .unwrap();
                    tx.commit().await.unwrap();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

fn fast() -> Limits {
    Limits {
        poll: Duration::from_millis(20),
        ..Limits::default()
    }
}

/// An `Ai` whose theme agent is `agent`.
fn scripted(agent: impl Fn(&[Value]) -> Value + Send + Sync + 'static) -> Ai {
    let fake = Fake::new([])
        .unwrap()
        .with_agent(ai_edit::FEATURE, Arc::new(agent));
    Ai::fake_with_client(Some(Client::Fake(Arc::new(fake))))
}

fn tool_turn(calls: &[(&str, Value)]) -> Value {
    let content: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, input))| json!({"type": "tool_use", "id": format!("t{i}"), "name": name, "input": input}))
        .collect();
    json!({ "stop_reason": "tool_use", "content": content })
}

fn end_turn(text: &str) -> Value {
    json!({ "stop_reason": "end_turn", "content": [{"type": "text", "text": text}] })
}

async fn setup(db: &PgPool) -> (PgPool, Storage, Shop) {
    let runtime = testkit::runtime_pool(db, 8).await;
    let storage = testkit::memory_storage();
    let shop = testkit::storefront::shop(&runtime, "alpha").await;
    publish_default(&runtime, &storage).await;
    (runtime, storage, shop)
}

async fn start(
    runtime: &PgPool,
    ai: &Ai,
    tenant: Uuid,
    prompt: &str,
) -> Result<ai_edit::RunSummary, Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let r = ai_edit::start(
        &mut tx,
        ai,
        "owner",
        &NewRun {
            prompt: prompt.into(),
            base_revision_id: None,
        },
    )
    .await;
    if r.is_ok() {
        tx.commit().await.unwrap();
    }
    r
}

/// SQL as the tenant (the owner role is bound by FORCE RLS too).
async fn as_tenant(runtime: &PgPool, tenant: Uuid, sql: &'static str, id: Uuid) {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let r = sqlx::query(sql).bind(id).execute(&mut *tx).await.unwrap();
    assert_eq!(r.rows_affected(), 1);
    tx.commit().await.unwrap();
}

async fn detail(runtime: &PgPool, tenant: Uuid, id: Uuid) -> ai_edit::RunDetail {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    ai_edit::detail(&mut tx, id).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn fake_agent_edits_checks_and_is_accepted_before_publish(db: PgPool) {
    let (runtime, storage, shop) = setup(&db).await;
    let ai = Ai::fake();
    let builder = spawn_builder(
        runtime.clone(),
        storage.clone(),
        shop.tenant,
        Arc::new(|_, _| true),
    );
    let run = start(
        &runtime,
        &ai,
        shop.tenant,
        "Přidej lištu {s} <dopravou> zdarma",
    )
    .await
    .unwrap();
    assert_eq!(run.status, "queued");
    // One active run per shop.
    let busy = start(&runtime, &ai, shop.tenant, "another")
        .await
        .unwrap_err();
    assert_eq!(busy.code(), "ai_run_in_progress");

    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    builder.abort();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.status, "succeeded", "{:?}", d.run.error);
    assert_eq!((d.run.turns, d.run.checks_run), (5, 1));
    assert!(d.run.tokens > 0);
    assert_eq!(d.run.model.as_deref(), Some("fake"));
    assert!(d.summary.unwrap().contains("All checks passed"));
    let tools: Vec<&str> = d.steps.iter().map(|s| s.tool.as_str()).collect();
    assert_eq!(
        tools,
        [
            "list_files",
            "read_file",
            "write_file",
            "write_file",
            "run_checks"
        ]
    );
    assert!(d.steps.iter().all(|s| s.ok));
    let diff = d.diff.unwrap();
    assert!(
        diff.contains("+++ b/src/pages/index.astro") && diff.contains("data-ai-edit"),
        "{diff}"
    );
    assert!(
        diff.contains("--- /dev/null\n+++ b/checks/ai-edit.spec.ts"),
        "{diff}"
    );
    assert_eq!(d.report.unwrap()["failures"], json!([]));

    // The checked revision: change `ai`, the prompt, ready, but unpublishable before accept.
    let rev = d.run.revision_id.unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let list = themes::list(&mut tx).await.unwrap();
    let r = list.iter().find(|r| r.id == rev).unwrap();
    assert_eq!((r.change.as_str(), r.status.as_str()), ("ai", "ready"));
    assert_eq!(
        themes::publish(&mut tx, "owner", rev)
            .await
            .unwrap_err()
            .code(),
        "ai_run_not_accepted"
    );
    let tok = themes::TokensInput {
        base_revision_id: Some(rev),
        tokens: tokens(),
    };
    assert_eq!(
        themes::edit_tokens(&mut tx, &storage, "owner", &tok)
            .await
            .unwrap_err()
            .code(),
        "ai_run_not_accepted"
    );
    drop(tx);

    // Metered under its own feature.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let usage: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE feature = 'theme_edit'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(usage, 5);
    // The stored transcript is append-only API history: user, then assistant/user pairs.
    let transcript: Value =
        sqlx::query_scalar("SELECT transcript FROM ai_theme_runs WHERE id = $1")
            .bind(run.id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    drop(tx);
    let roles: Vec<&str> = transcript
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "assistant"
        ]
    );

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let accepted = ai_edit::accept(&mut tx, "owner", run.id).await.unwrap();
    assert_eq!(accepted.status, "accepted");
    assert_eq!(
        ai_edit::discard(&mut tx, "owner", run.id)
            .await
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    let published = themes::publish(&mut tx, "owner", rev).await.unwrap();
    assert_eq!(published.status, "published");
    // What the revision changed against its parent (the diff view of WP23's follow-up).
    let rd = ai_edit::revision_diff(&mut tx, &storage, rev)
        .await
        .unwrap();
    assert!(rd.diff.contains("data-ai-edit"));
    tx.commit().await.unwrap();
    // A new run may start from the accepted (now published) AI revision.
    start(&runtime, &ai, shop.tenant, "next").await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_checks_are_repaired_within_the_cycle_limit(db: PgPool) {
    let (runtime, storage, shop) = setup(&db).await;
    // Writes a change + check, runs the checks; on a failure writes a fix and runs them again.
    let ai = scripted(|messages| {
        let turn = messages.iter().filter(|m| m["role"] == "assistant").count();
        match turn {
            0 => tool_turn(&[
                (
                    "write_file",
                    json!({"path": "src/components/Bar.astro", "content": "<p>v1</p>\n"}),
                ),
                (
                    "write_file",
                    json!({"path": "checks/bar.spec.ts", "content": "// check\n"}),
                ),
                ("run_checks", json!({})),
            ]),
            1 => tool_turn(&[
                (
                    "write_file",
                    json!({"path": "src/components/Bar.astro", "content": "<p>v2</p>\n"}),
                ),
                ("run_checks", json!({})),
            ]),
            _ => end_turn("Fixed."),
        }
    });
    let builder = spawn_builder(
        runtime.clone(),
        storage.clone(),
        shop.tenant,
        Arc::new(|n, _| n >= 2),
    );
    let run = start(&runtime, &ai, shop.tenant, "bar").await.unwrap();
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    builder.abort();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.status, "succeeded", "{:?}", d.run.error);
    assert_eq!(d.run.checks_run, 2);
    let checks: Vec<(bool, &str)> = d
        .steps
        .iter()
        .filter(|s| s.tool == "run_checks")
        .map(|s| (s.ok, s.detail.as_str()))
        .collect();
    assert!(
        !checks[0].0 && checks[0].1.contains("build 1 failed"),
        "{checks:?}"
    );
    assert!(checks[1].0);
    assert!(d.diff.unwrap().contains("+<p>v2</p>"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn limits_end_runs(db: PgPool) {
    let (runtime, storage, shop) = setup(&db).await;
    let builder = spawn_builder(
        runtime.clone(),
        storage.clone(),
        shop.tenant,
        Arc::new(|_, _| false),
    );

    // Turn limit: an agent that never stops.
    let ai = scripted(|_| tool_turn(&[("list_files", json!({"prefix": ""}))]));
    let run = start(&runtime, &ai, shop.tenant, "loop").await.unwrap();
    let limits = Limits {
        max_turns: 3,
        ..fast()
    };
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, limits)
        .await
        .unwrap();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.status, "failed");
    assert!(
        d.run.error.as_deref().unwrap().starts_with("turn_limit"),
        "{:?}",
        d.run.error
    );
    assert_eq!(d.run.turns, 3);

    // Repair limit: every check fails; after max_checks builds the run stops.
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let ai = scripted(move |_| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        tool_turn(&[
            (
                "write_file",
                json!({"path": "src/x.astro", "content": format!("<p>{n}</p>\n")}),
            ),
            (
                "write_file",
                json!({"path": "checks/x.spec.ts", "content": "// x\n"}),
            ),
            ("run_checks", json!({})),
        ])
    });
    let run = start(&runtime, &ai, shop.tenant, "never passes")
        .await
        .unwrap();
    let limits = Limits {
        max_checks: 2,
        ..fast()
    };
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, limits)
        .await
        .unwrap();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert!(
        d.run.error.as_deref().unwrap().starts_with("repair_limit"),
        "{:?}",
        d.run.error
    );
    assert_eq!((d.run.checks_run, d.run.turns), (2, 2));

    // Budget: a run over its token budget stops before the next call.
    let ai = scripted(|_| tool_turn(&[("list_files", json!({"prefix": ""}))]));
    let run = start(&runtime, &ai, shop.tenant, "budget").await.unwrap();
    let limits = Limits {
        max_tokens: 1,
        ..fast()
    };
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, limits)
        .await
        .unwrap();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert!(
        d.run.error.as_deref().unwrap().starts_with("budget"),
        "{:?}",
        d.run.error
    );
    assert_eq!(d.run.turns, 1);

    // run_checks without a functional check or without changes is refused, not built; the
    // model then stops, leaving nothing verified.
    let ai = scripted(|messages| {
        let turn = messages.iter().filter(|m| m["role"] == "assistant").count();
        match turn {
            0 => tool_turn(&[("run_checks", json!({}))]),
            1 => tool_turn(&[
                (
                    "write_file",
                    json!({"path": "src/y.astro", "content": "<p>y</p>\n"}),
                ),
                (
                    "write_file",
                    json!({"path": "package.json", "content": "{\"scripts\":{}}"}),
                ),
                ("read_file", json!({"path": "../../etc/passwd"})),
                ("run_checks", json!({})),
            ]),
            _ => end_turn("Done."),
        }
    });
    let run = start(&runtime, &ai, shop.tenant, "unchecked")
        .await
        .unwrap();
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    builder.abort();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.checks_run, 0);
    assert!(
        d.run.error.as_deref().unwrap().starts_with("unverified"),
        "{:?}",
        d.run.error
    );
    let refused: Vec<bool> = d.steps.iter().map(|s| s.ok).collect();
    assert_eq!(refused, [false, true, false, false, false]);
    assert!(d.steps[0].detail.contains("nothing changed"));
    assert!(d.steps[4].detail.contains("functional check"));
    assert!(!d.diff.unwrap().contains("package.json"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn runs_are_cancelled_interrupted_and_quota_bound(db: PgPool) {
    let (runtime, storage, shop) = setup(&db).await;
    let ai = scripted(|_| tool_turn(&[("list_files", json!({"prefix": ""}))]));

    // Queued: cancelled at once.
    let run = start(&runtime, &ai, shop.tenant, "a").await.unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    assert_eq!(
        ai_edit::cancel(&mut tx, "owner", run.id)
            .await
            .unwrap()
            .status,
        "cancelled"
    );
    assert_eq!(
        ai_edit::cancel(&mut tx, "owner", run.id)
            .await
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    tx.commit().await.unwrap();
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    assert_eq!(detail(&runtime, shop.tenant, run.id).await.run.turns, 0);

    // Running: stops at the next turn.
    let run = start(&runtime, &ai, shop.tenant, "b").await.unwrap();
    as_tenant(
        &runtime,
        shop.tenant,
        "UPDATE ai_theme_runs SET cancel_requested = true WHERE id = $1",
        run.id,
    )
    .await;
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    assert_eq!(
        detail(&runtime, shop.tenant, run.id).await.run.status,
        "cancelled"
    );

    // A retried job of a run that was already running fails it (never re-spends).
    let run = start(&runtime, &ai, shop.tenant, "c").await.unwrap();
    as_tenant(
        &runtime,
        shop.tenant,
        "UPDATE ai_theme_runs SET status = 'running' WHERE id = $1",
        run.id,
    )
    .await;
    ai_edit::run(&runtime, &storage, &ai, shop.tenant, run.id, fast())
        .await
        .unwrap();
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.status, "failed");
    assert!(d.run.error.unwrap().starts_with("interrupted"));

    // Quota: refused at start (402) and between turns.
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 0 WHERE id = $1")
        .bind(shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        start(&runtime, &ai, shop.tenant, "d")
            .await
            .unwrap_err()
            .code(),
        "ai_quota_exceeded"
    );

    // Prompt validation, disabled AI.
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = NULL WHERE id = $1")
        .bind(shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        start(&runtime, &ai, shop.tenant, "   ")
            .await
            .unwrap_err()
            .code(),
        "invalid_prompt"
    );
    let long = "x".repeat(4001);
    assert_eq!(
        start(&runtime, &ai, shop.tenant, &long)
            .await
            .unwrap_err()
            .code(),
        "invalid_prompt"
    );
    let off = Ai::fake_with_client(None);
    assert_eq!(
        start(&runtime, &off, shop.tenant, "e")
            .await
            .unwrap_err()
            .code(),
        "service_unavailable"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn runs_are_tenant_isolated_and_discard_keeps_revisions_unpublishable(db: PgPool) {
    let (runtime, storage, a) = setup(&db).await;
    let b = testkit::storefront::shop(&runtime, "beta").await;
    let ai = Ai::fake();
    let builder = spawn_builder(
        runtime.clone(),
        storage.clone(),
        a.tenant,
        Arc::new(|_, _| true),
    );
    let run = start(&runtime, &ai, a.tenant, "note").await.unwrap();
    ai_edit::run(&runtime, &storage, &ai, a.tenant, run.id, fast())
        .await
        .unwrap();
    builder.abort();

    // Beta sees nothing of alpha's run (RLS + FORCE).
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    assert!(ai_edit::list(&mut tx).await.unwrap().is_empty());
    assert!(matches!(
        ai_edit::detail(&mut tx, run.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        ai_edit::accept(&mut tx, "owner", run.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        ai_edit::cancel(&mut tx, "owner", run.id).await,
        Err(Error::NotFound)
    ));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM ai_theme_runs")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // Nor can beta insert a run for alpha.
    let err = sqlx::query(
        "INSERT INTO ai_theme_runs (tenant_id, base_revision_id, prompt, created_by)
         SELECT $1, base_revision_id, 'x', 'y' FROM ai_theme_runs LIMIT 1",
    )
    .bind(a.tenant)
    .execute(&mut *tx)
    .await;
    assert!(err.is_ok_and(|r| r.rows_affected() == 0));
    let direct = sqlx::query("INSERT INTO ai_theme_runs (tenant_id, base_revision_id, prompt, created_by) VALUES ($1, $2, 'x', 'y')")
        .bind(a.tenant)
        .bind(run.base_revision_id)
        .execute(&mut *tx)
        .await;
    assert!(direct.is_err());
    drop(tx);

    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    assert_eq!(ai_edit::list(&mut tx).await.unwrap().len(), 1);
    let discarded = ai_edit::discard(&mut tx, "owner", run.id).await.unwrap();
    assert_eq!(discarded.status, "discarded");
    assert_eq!(
        ai_edit::accept(&mut tx, "owner", run.id)
            .await
            .unwrap_err()
            .code(),
        "invalid_transition"
    );
    let rev = discarded.revision_id.unwrap();
    assert_eq!(
        themes::publish(&mut tx, "owner", rev)
            .await
            .unwrap_err()
            .code(),
        "ai_run_not_accepted"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn maintenance_fails_runs_whose_worker_died(db: PgPool) {
    let (runtime, storage, shop) = setup(&db).await;
    let ai = Ai::fake();
    let run = start(&runtime, &ai, shop.tenant, "stuck").await.unwrap();
    // A run that is still moving is left alone.
    let report = themes::maintenance(&runtime, &storage).await.unwrap();
    assert_eq!(report.expired_ai_runs, 0);
    as_tenant(
        &runtime,
        shop.tenant,
        "UPDATE ai_theme_runs SET status = 'running', updated_at = now() - interval '61 minutes' WHERE id = $1",
        run.id,
    )
    .await;
    // It blocks new runs until the sweep fails it.
    assert_eq!(
        start(&runtime, &ai, shop.tenant, "next")
            .await
            .unwrap_err()
            .code(),
        "ai_run_in_progress"
    );
    let report = themes::maintenance(&runtime, &storage).await.unwrap();
    assert_eq!(report.expired_ai_runs, 1);
    let d = detail(&runtime, shop.tenant, run.id).await;
    assert_eq!(d.run.status, "failed");
    assert!(d.run.error.unwrap().starts_with("interrupted"));
    start(&runtime, &ai, shop.tenant, "next").await.unwrap();
}
