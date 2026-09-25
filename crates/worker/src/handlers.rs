//! Job handlers. Every handler is idempotent (spec §13).

use std::sync::Arc;
use std::time::Duration;

use commerce::ai::{Ai, Outcome, plan, proposals};
use commerce::feeds::{export, import};
use commerce::media::{self, Processed};
use commerce::notifications::{self, Step};
use commerce::payments::{bank, stripe};
use commerce::pricing::intervals;
use commerce::search::{self, Meili, index::Rebuilt};
use commerce::storefront::PublicUrls;
use commerce::storefront::purge::{self, Purge};
use commerce::themes;
use platform::auth_service::AuthService;
use platform::crypto::SecretBox;
use platform::edge::EdgePurge;
use platform::http::SafeClient;
use platform::mail::Mailer;
use platform::queue::{self, Job};
use platform::storage::Storage;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::runner::{Ctx, Handlers, JobError};

/// Structured log line per outbox event: the default subscriber of every event type.
pub const EVENTS_LOG: &str = "events.log";
/// Hourly retention: finished jobs, dispatched events, expired idempotency keys (A12),
/// zero-result search queries older than 90 days (A20), expired customer sessions and sign-in
/// links, day-old rate-limit rows and IP salts (§14).
pub const MAINTENANCE_CLEANUP: &str = "maintenance.cleanup";

const JOB_RETENTION: Duration = Duration::from_secs(7 * 86_400);

/// Image encoding is CPU-heavy (each encode is single-threaded): one at a time per worker
/// process keeps the other job loops responsive. ponytail: fixed at 1; make it configurable
/// when a dedicated media worker gets more cores.
const MEDIA_CONCURRENCY: usize = 1;

/// Staff invitation email for a `staff.invited` event (spec §11.4, WP9).
pub const STAFF_INVITE_MAIL: &str = "staff.invite_mail";
/// Purges the edge's cached pages an outbox event invalidates (A2, WP13a).
pub const EDGE_PURGE: &str = "edge.purge";

/// Services of the WP13a/WP14 jobs: edge purges, the SSRF-safe fetcher (imports), the public
/// storefront URLs (export feeds) and webhook delivery (`None` without `SECRETS_KEY`); the
/// AI helpers (WP22).
#[derive(Clone)]
pub struct Extra {
    pub edge: EdgePurge,
    pub fetch: SafeClient,
    pub urls: PublicUrls,
    pub ai: Ai,
    pub webhooks: Option<commerce::webhooks::Webhooks>,
    /// WP11: the Fio API poller (token key, API base URL).
    pub fio: Option<Fio>,
    /// WP23: the theme builder (`THEME_BUILDER_URL` + `THEME_BUILDER_TOKEN`).
    pub theme_builder: Option<ThemeBuilder>,
}

/// Hands revisions to the theme builder (WP23). Not `Debug`: it holds the service token.
#[derive(Clone)]
pub struct ThemeBuilder {
    pub url: reqwest::Url,
    pub token: String,
    pub http: reqwest::Client,
}

/// Fio API polling (WP11). Not `Debug`: it holds the key for stored tokens.
#[derive(Clone)]
pub struct Fio {
    pub secrets: Arc<SecretBox>,
    pub base_url: String,
    pub http: reqwest::Client,
}

impl Extra {
    /// No edge, no allowlisted hosts, default URLs, no Fio polling, the fake AI provider
    /// (tests, tools).
    pub fn disabled() -> Result<Self, platform::http::FetchError> {
        Ok(Self {
            edge: EdgePurge::disabled(),
            fetch: SafeClient::new(Vec::<String>::new())?,
            urls: PublicUrls::default(),
            ai: Ai::fake(),
            webhooks: None,
            fio: None,
            theme_builder: None,
        })
    }
}

/// `customer.email_verified` → link the guest orders placed with that email (A5, WP10).
pub const LINK_GUEST_ORDERS: &str = "orders.link_guest";
/// Payment timeouts (A10): cancel unpaid orders whose payment window closed. Every minute.
pub const PAYMENTS_EXPIRE: &str = "payments.expire";
/// Bank-transfer reminders on day 3 and 6 (spec §10.3). Hourly.
pub const PAYMENTS_REMIND: &str = "payments.remind";
/// Downloads new transactions of accounts with a Fio API token and matches them (A25).
pub const PAYMENTS_FIO_POLL: &str = "payments.fio_poll";
pub use commerce::analytics::{PARTITIONS_JOB, ROLLUP_JOB};
pub use commerce::ops::SWEEP_JOB;
pub use commerce::webhooks::{DELIVER_JOB, FANOUT_JOB};

pub fn all(
    storage: Storage,
    meili: Meili,
    mailer: Option<Mailer>,
    auth: Option<AuthService>,
    extra: Extra,
) -> Handlers {
    let encode_slots = Arc::new(Semaphore::new(MEDIA_CONCURRENCY));
    let purge_storage = storage.clone();
    let (import_storage, export_storage) = (storage.clone(), storage.clone());
    let sweep_storage = storage.clone();
    let (m1, m3, m4, m5) = (meili.clone(), meili.clone(), meili.clone(), meili);
    let webhooks = extra.webhooks.clone();
    let (ai1, ai2) = (extra.ai.clone(), extra.ai.clone());
    let theme_builder = extra.theme_builder.clone();
    let theme_storage = storage.clone();
    let (e1, e2, e3, e4) = (extra.clone(), extra.clone(), extra.clone(), extra);
    let urls = e4.urls.clone();
    Handlers::default()
        .register(EDGE_PURGE, move |_ctx, job| {
            edge_purge(job, e1.edge.clone())
        })
        .register(import::JOB, move |ctx, job| {
            feed_import(ctx, job, import_storage.clone(), e2.fetch.clone())
        })
        .register(export::JOB, move |ctx, job| {
            feed_export(ctx, job, export_storage.clone(), e3.urls.clone())
        })
        .register(export::ALL_JOB, feed_export_all)
        .register(search::SYNONYMS_JOB, move |ctx, job| {
            search_synonyms(ctx, job, m4.clone())
        })
        .register(notifications::SEND_JOB, move |ctx, job| {
            mail_send(ctx, job, mailer.clone())
        })
        .register(STAFF_INVITE_MAIL, move |ctx, job| {
            staff_invite_mail(ctx, job, auth.clone())
        })
        .register(search::INDEX_PRODUCT_JOB, move |ctx, job| {
            search_index_product(ctx, job, m1.clone())
        })
        .register(search::REINDEX_CATEGORY_JOB, search_reindex_category)
        .register(search::REBUILD_JOB, move |ctx, job| {
            search_rebuild(ctx, job, m3.clone())
        })
        .register(EVENTS_LOG, events_log)
        .register(MAINTENANCE_CLEANUP, maintenance_cleanup)
        .register(media::PROCESS_JOB, move |ctx, job| {
            media_process(ctx, job, storage.clone(), encode_slots.clone())
        })
        .register(media::PURGE_JOB, move |_ctx, job| {
            media_purge(job, purge_storage.clone())
        })
        .register(intervals::TRANSITION_JOB, price_transition)
        .register(LINK_GUEST_ORDERS, link_guest_orders)
        .register(PAYMENTS_EXPIRE, payments_expire)
        .register(stripe::EVENT_JOB, provider_event)
        .register(PAYMENTS_REMIND, move |ctx, job| {
            payments_remind(ctx, job, urls.clone())
        })
        .register(PAYMENTS_FIO_POLL, move |ctx, job| {
            payments_fio_poll(ctx, job, e4.fio.clone())
        })
        .register(ROLLUP_JOB, analytics_rollup)
        .register(PARTITIONS_JOB, analytics_partitions)
        .register(FANOUT_JOB, webhooks_fanout)
        .register(DELIVER_JOB, move |ctx, job| {
            webhooks_deliver(ctx, job, webhooks.clone())
        })
        .register(SWEEP_JOB, move |ctx, job| {
            ops_sweep(ctx, job, sweep_storage.clone(), m5.clone())
        })
        .register(proposals::JOB, move |ctx, job| {
            ai_proposal(ctx, job, ai1.clone())
        })
        .register(plan::PLAN_JOB, move |ctx, job| {
            ai_plan(ctx, job, ai2.clone())
        })
        .register(plan::APPLY_JOB, ai_apply)
        .register(themes::BUILD_JOB, move |_ctx, job| {
            theme_build(job, theme_builder.clone())
        })
        .register(themes::MAINTENANCE_JOB, move |ctx, job| {
            theme_maintenance(ctx, job, theme_storage.clone())
        })
}

/// WP23: asks the theme builder to build a revision. The builder answers 202 at once and
/// reports progress through its own callbacks; a revision it never picks up is failed by the
/// maintenance job after 30 minutes.
async fn theme_build(job: Job, builder: Option<ThemeBuilder>) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("themes.build without tenant".into()))?;
    let revision = job
        .payload
        .get("revision_id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::try_parse(v).ok())
        .ok_or_else(|| JobError::Permanent("themes.build without revision_id".into()))?;
    let b = builder.ok_or_else(|| {
        JobError::Permanent("theme builder not configured (THEME_BUILDER_URL/TOKEN)".into())
    })?;
    let url = b
        .url
        .join("builds")
        .map_err(|e| JobError::Permanent(e.to_string()))?;
    let res = b
        .http
        .post(url)
        .bearer_auth(&b.token)
        .json(&serde_json::json!({ "tenant_id": tenant, "revision_id": revision }))
        .send()
        .await
        .map_err(|e| JobError::Retry(format!("theme builder unreachable: {e}")))?;
    let status = res.status();
    if status.is_success() {
        tracing::info!(%tenant, %revision, "theme build handed to the builder");
        Ok(())
    } else if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
        Err(JobError::Permanent(format!(
            "theme builder refused the build: {status}"
        )))
    } else {
        Err(JobError::Retry(format!("theme builder answered {status}")))
    }
}

/// Hourly (WP23): stuck builds fail, artifacts nobody references are deleted.
async fn theme_maintenance(ctx: Ctx, _job: Job, storage: Storage) -> Result<(), JobError> {
    let report = themes::maintenance(&ctx.db, &storage)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::info!(?report, "theme maintenance done");
    Ok(())
}

/// A11: processes one stored provider event. Mismatches are recorded on the event (never
/// retried); database errors retry.
async fn provider_event(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let id = job
        .payload
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok())
        .ok_or_else(|| JobError::Permanent("provider event job without id".into()))?;
    match stripe::process_event(&ctx.db, id).await {
        Ok(outcome) => {
            tracing::info!(event = %id, ?outcome, "provider event processed");
            Ok(())
        }
        Err(platform::Error::NotFound) => Err(JobError::Permanent("unknown event".into())),
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

fn ai_outcome(r: Result<Outcome, platform::Error>) -> Result<(), JobError> {
    match r {
        Ok(Outcome::Done) => Ok(()),
        Ok(Outcome::Retry(reason)) => Err(JobError::Retry(reason)),
        Err(platform::Error::NotFound) => Err(JobError::Permanent("record not found".into())),
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

/// Generates an AI proposal (WP22); provider outages are retried, then the proposal fails.
async fn ai_proposal(ctx: Ctx, job: Job, ai: Ai) -> Result<(), JobError> {
    let (tenant, id) = tenant_and(&job, "proposal_id")?;
    let last = job.attempts >= job.max_attempts;
    ai_outcome(proposals::run(&ctx.db, &ai, tenant, id, last).await)
}

/// Plans a bulk edit: model output -> validation -> targets -> preview.
async fn ai_plan(ctx: Ctx, job: Job, ai: Ai) -> Result<(), JobError> {
    let (tenant, id) = tenant_and(&job, "plan_id")?;
    let last = job.attempts >= job.max_attempts;
    ai_outcome(plan::run_plan(&ctx.db, &ai, tenant, id, last).await)
}

/// Applies a confirmed bulk plan, one product per transaction (resumable).
async fn ai_apply(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let (tenant, id) = tenant_and(&job, "plan_id")?;
    let last = job.attempts >= job.max_attempts;
    ai_outcome(plan::run_apply(&ctx.db, tenant, id, last).await)
}

async fn payments_remind(ctx: Ctx, _job: Job, urls: PublicUrls) -> Result<(), JobError> {
    let sent = commerce::checkout::send_payment_reminders(&ctx.db, &urls, 500)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    if sent > 0 {
        tracing::info!(sent, "payment reminders sent");
    }
    Ok(())
}

/// Polls every account with a Fio token; one failing account does not stop the others.
async fn payments_fio_poll(ctx: Ctx, _job: Job, fio: Option<Fio>) -> Result<(), JobError> {
    let accounts = bank::fio_accounts(&ctx.db)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    let Some(fio) = fio else {
        if !accounts.is_empty() {
            tracing::warn!("Fio tokens are stored but SECRETS_KEY is not set: no polling");
        }
        return Ok(());
    };
    for acc in accounts {
        match bank::poll_fio(
            &ctx.db,
            &fio.http,
            &fio.secrets,
            &fio.base_url,
            &acc,
            chrono::Utc::now(),
        )
        .await
        {
            Ok(r) => tracing::info!(tenant = %acc.tenant_id, account = %acc.bank_account_id,
                imported = r.imported, matched = r.matched, "fio statement polled"),
            Err(e) => tracing::warn!(tenant = %acc.tenant_id, account = %acc.bank_account_id,
                error = %e, "fio polling failed"),
        }
    }
    Ok(())
}

/// Delivers one email (A14). A message that could not be handed over is retried with backoff
/// until the message itself gives up (`failed`, see `notifications::deliver`).
async fn mail_send(ctx: Ctx, job: Job, mailer: Option<Mailer>) -> Result<(), JobError> {
    let (tenant, message) = tenant_and(&job, "message_id")?;
    // Retried with backoff (the message stays `pending`); the hourly reconciliation requeues
    // it if the job gives up before mail is configured.
    let mailer = mailer.ok_or_else(|| {
        JobError::Retry("mail is not configured (MAIL_TRANSACTIONAL_SMTP_URL, ...)".into())
    })?;
    match notifications::deliver(&ctx.db, &mailer, tenant, message).await {
        Ok(Step::Done) => Ok(()),
        Ok(Step::Retry(reason)) => Err(JobError::Retry(reason)),
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

/// `staff.invited` → the invitation email through the mail pipeline (idempotent).
async fn staff_invite_mail(ctx: Ctx, job: Job, auth: Option<AuthService>) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("staff invitation without tenant".into()))?;
    let event = job.payload.get("payload").cloned().unwrap_or_default();
    let member = event
        .get("member_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no member_id".into()))?;
    let callback = event
        .get("callback_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JobError::Permanent("payload has no callback_url".into()))?;
    // Retried until the auth service is configured and reachable.
    let auth = auth.ok_or_else(|| JobError::Retry("AUTH_INTERNAL_URL is not configured".into()))?;
    commerce::staff::send_invitation(&ctx.db, &auth, tenant, member, callback)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))
}

/// Purges what an outbox event invalidates. Best effort: the edge TTLs bound staleness.
async fn edge_purge(job: Job, edge: EdgePurge) -> Result<(), JobError> {
    let Some(tenant) = job.tenant_id else {
        return Ok(());
    };
    let event_type = job
        .payload
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let payload = job.payload.get("payload").cloned().unwrap_or_default();
    match purge::for_event(event_type, &payload) {
        Some(Purge::Tenant) => edge.tenant(tenant).await,
        Some(Purge::Tags(tags)) => edge.tags(tenant, &tags).await,
        None => {}
    }
    Ok(())
}

/// A feed import step (`analyze` or `apply`); feed problems end the run as `failed`.
async fn feed_import(
    ctx: Ctx,
    job: Job,
    storage: Storage,
    fetch: SafeClient,
) -> Result<(), JobError> {
    let (tenant, run) = tenant_and(&job, "run_id")?;
    let step = job
        .payload
        .get("step")
        .and_then(|s| s.as_str())
        .unwrap_or("analyze")
        .to_owned();
    match import::run_step(&ctx.db, &storage, &fetch, tenant, run, &step).await {
        Ok(()) => Ok(()),
        Err(e) if job.attempts >= job.max_attempts => {
            tracing::error!(%run, error = %e, "feed import gave up");
            import::fail(
                &ctx.db,
                tenant,
                run,
                "the import failed repeatedly; try again",
            )
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
            Err(JobError::Permanent(e.to_string()))
        }
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

async fn feed_export(
    ctx: Ctx,
    job: Job,
    storage: Storage,
    urls: PublicUrls,
) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("feed export without tenant".into()))?;
    let written = export::generate(&ctx.db, &storage, &urls, tenant)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::info!(%tenant, written, "export feeds generated");
    Ok(())
}

/// Hourly: one export job per tenant (idempotent per hour).
async fn feed_export_all(ctx: Ctx, _job: Job) -> Result<(), JobError> {
    let tenants = sqlx::query_scalar!("SELECT id FROM platform.tenants WHERE status = 'active'")
        .fetch_all(&ctx.db)
        .await?;
    let now = chrono::Utc::now();
    for tenant in &tenants {
        let mut job = export::job(*tenant, now, false);
        job.idempotency_key = Some(format!(
            "{}:{tenant}:h{}",
            export::JOB,
            now.timestamp() / 3600
        ));
        queue::enqueue(&ctx.db, &job).await?;
    }
    tracing::info!(tenants = tenants.len(), "export feed jobs queued");
    Ok(())
}

/// Applies the tenant's synonyms to its indexes.
async fn search_synonyms(ctx: Ctx, job: Job, meili: Meili) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("synonyms without tenant".into()))?;
    search::index::apply_synonyms(&ctx.db, &meili, tenant)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))
}

/// A scheduled price change (sale start/end) took effect: publish `price.changed` (A18).
async fn price_transition(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("price transition without tenant".into()))?;
    let at = job
        .payload
        .get("at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no valid `at`".into()))?
        .with_timezone(&chrono::Utc);
    let mut tx = platform::db::tenant_tx(&ctx.db, tenant).await?;
    let published = intervals::publish_transitions(&mut tx, at)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tx.commit().await?;
    tracing::info!(%tenant, %at, published, "price transition published");
    Ok(())
}

fn tenant_and(job: &Job, field: &str) -> Result<(Uuid, Uuid), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent(format!("{} without tenant", job.kind)))?;
    let id = job
        .payload
        .get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Permanent(format!("payload has no {field}")))?;
    Ok((tenant, id))
}

/// (Re)indexes one product; stale versions are dropped (spec A27).
async fn search_index_product(ctx: Ctx, job: Job, meili: Meili) -> Result<(), JobError> {
    let (tenant, product) = tenant_and(&job, "product_id")?;
    let version = search::job_version(&job.payload);
    let outcome = search::index::index_product(&ctx.db, &meili, tenant, product, version)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::debug!(%tenant, %product, ?outcome, "product indexed");
    Ok(())
}

/// A category was renamed or moved: reindex the products in its subtree.
async fn search_reindex_category(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let (tenant, category) = tenant_and(&job, "category_id")?;
    let mut tx = platform::db::tenant_tx(&ctx.db, tenant).await?;
    let products = search::index::category_products(&mut tx, category)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    let version = search::next_version(&mut *tx).await?;
    for product in &products {
        queue::enqueue(
            &mut *tx,
            &search::index_product_job(tenant, *product, version),
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(%tenant, %category, products = products.len(), "category reindex queued");
    Ok(())
}

async fn search_rebuild(ctx: Ctx, job: Job, meili: Meili) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("search rebuild without tenant".into()))?;
    let version = search::job_version(&job.payload);
    match search::index::rebuild(&ctx.db, &meili, tenant, job.id, version).await {
        Ok(Rebuilt::Done | Rebuilt::Stale) => Ok(()),
        // Runs again after the current rebuild (backoff), so late changes are not lost.
        Ok(Rebuilt::Busy) => Err(JobError::Retry(
            "another rebuild of this tenant is running".into(),
        )),
        Err(e) => Err(JobError::Retry(e.to_string())),
    }
}

async fn events_log(_ctx: Ctx, job: Job) -> Result<(), JobError> {
    let event_type = job
        .payload
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| JobError::Permanent("payload has no event type".into()))?;
    // Ids only: payloads may carry personal data.
    tracing::info!(
        event_type,
        event_id = job
            .payload
            .get("event_id")
            .and_then(serde_json::Value::as_i64),
        tenant_id = job.tenant_id.map(tracing::field::display),
        "event"
    );
    Ok(())
}

async fn maintenance_cleanup(ctx: Ctx, _job: Job) -> Result<(), JobError> {
    let queue_rows = queue::purge(&ctx.db, JOB_RETENTION).await?;
    let keys = sqlx::query_scalar!(r#"SELECT platform.purge_idempotency_keys() AS "n!""#)
        .fetch_one(&ctx.db)
        .await?;
    let zero_results =
        sqlx::query_scalar!(r#"SELECT platform.purge_search_zero_results() AS "n!""#)
            .fetch_one(&ctx.db)
            .await?;
    let customer_auth = sqlx::query_scalar!(r#"SELECT platform.purge_customer_auth() AS "n!""#)
        .fetch_one(&ctx.db)
        .await?;
    // A14: deliveries whose job died (or whose worker died mid-send) get a new job.
    let stalled = notifications::reconcile_jobs(&ctx.db)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    for job in &stalled {
        queue::enqueue(&ctx.db, job).await?;
    }
    tracing::info!(
        queue_rows,
        stalled_emails = stalled.len(),
        idempotency_keys = keys,
        zero_results,
        customer_auth,
        "cleanup done"
    );
    Ok(())
}

fn tenant_and_asset(job: &Job) -> Result<(Uuid, Uuid), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("media job without tenant".into()))?;
    let asset = job
        .payload
        .get("asset_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no asset_id".into()))?;
    Ok((tenant, asset))
}

async fn media_process(
    ctx: Ctx,
    job: Job,
    storage: Storage,
    slots: Arc<Semaphore>,
) -> Result<(), JobError> {
    let (tenant, asset) = tenant_and_asset(&job)?;
    let _slot = slots
        .acquire()
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    let outcome = match media::process(&ctx.db, &storage, tenant, asset).await {
        Ok(outcome) => outcome,
        // Last attempt: record the failure on the asset instead of leaving it `processing`.
        Err(e) if job.attempts >= job.max_attempts => {
            tracing::error!(%asset, error = %e, "media processing gave up");
            media::mark_failed(
                &ctx.db,
                tenant,
                asset,
                "processing failed repeatedly; upload the image again",
            )
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
            return Err(JobError::Permanent(e.to_string()));
        }
        Err(e) => return Err(JobError::Retry(e.to_string())),
    };
    tracing::info!(%asset, ?outcome, "media processed");
    if outcome == Processed::Failed {
        tracing::warn!(%asset, "image could not be decoded; asset marked failed");
    }
    Ok(())
}

async fn media_purge(job: Job, storage: Storage) -> Result<(), JobError> {
    let keys = |field: &str| -> Result<Vec<String>, JobError> {
        serde_json::from_value(job.payload.get(field).cloned().unwrap_or_default())
            .map_err(|e| JobError::Permanent(format!("{field}: {e}")))
    };
    media::purge(&storage, &keys("private")?, &keys("public")?)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))
}

/// `customer.email_verified` (A5): the address is proven, so its guest orders join the account.
async fn link_guest_orders(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let tenant = job
        .tenant_id
        .ok_or_else(|| JobError::Permanent("guest linking without tenant".into()))?;
    let customer = job
        .payload
        .pointer("/payload/customer_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no customer_id".into()))?;
    let mut tx = platform::db::tenant_tx(&ctx.db, tenant).await?;
    let linked = commerce::orders::link_guest_orders(&mut tx, customer)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tx.commit().await?;
    tracing::info!(%tenant, %customer, linked, "guest orders linked");
    Ok(())
}

/// A10: cancels unpaid orders whose payment window closed and releases their stock.
async fn payments_expire(ctx: Ctx, _job: Job) -> Result<(), JobError> {
    let expired = commerce::checkout::expire_due(&ctx.db, 500)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    if expired > 0 {
        tracing::info!(expired, "unpaid orders expired");
    }
    Ok(())
}

/// Hourly: today's and yesterday's `daily_metrics` of every tenant; once a day (02:00 UTC
/// slot) the last 14 days, so an outage or late events leave no permanent gap.
async fn analytics_rollup(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let today = chrono::Utc::now().date_naive();
    let slot = job.payload.get("slot").and_then(serde_json::Value::as_i64);
    let days: u64 = if slot.is_some_and(|s| s.rem_euclid(24) == 2) {
        14
    } else {
        2
    };
    let tenants = sqlx::query_scalar!("SELECT id FROM platform.tenants ORDER BY id")
        .fetch_all(&ctx.db)
        .await?;
    for tenant in &tenants {
        let mut tx = platform::db::tenant_tx(&ctx.db, *tenant).await?;
        for back in 0..days {
            let day = today - chrono::Days::new(back);
            commerce::analytics::rollup(&mut tx, day)
                .await
                .map_err(|e| JobError::Retry(e.to_string()))?;
        }
        tx.commit().await?;
    }
    tracing::info!(tenants = tenants.len(), "analytics rollup done");
    Ok(())
}

/// Nightly: event partitions two months ahead, drop those past the 13-month retention.
async fn analytics_partitions(ctx: Ctx, _job: Job) -> Result<(), JobError> {
    let created = sqlx::query_scalar!(r#"SELECT platform.ensure_event_partitions(2) AS "n!""#)
        .fetch_one(&ctx.db)
        .await?;
    let dropped = sqlx::query_scalar!(r#"SELECT platform.drop_event_partitions(13) AS "n!""#)
        .fetch_one(&ctx.db)
        .await?;
    tracing::info!(created, dropped, "event partitions maintained");
    Ok(())
}

/// An outbox event → deliveries for the tenant's matching webhook subscriptions.
async fn webhooks_fanout(ctx: Ctx, job: Job) -> Result<(), JobError> {
    let Some(tenant) = job.tenant_id else {
        return Ok(()); // platform events have no subscribers
    };
    let event_id = job
        .payload
        .get("event_id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| JobError::Permanent("payload has no event_id".into()))?;
    let event_type = job
        .payload
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| JobError::Permanent("payload has no type".into()))?;
    let data = job.payload.get("payload").cloned().unwrap_or_default();
    commerce::webhooks::fanout(&ctx.db, tenant, event_id, event_type, &data)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    Ok(())
}

/// One webhook delivery attempt. HTTP failures are recorded on the delivery (it has its own
/// retry schedule); only database trouble retries the job.
async fn webhooks_deliver(
    ctx: Ctx,
    job: Job,
    webhooks: Option<commerce::webhooks::Webhooks>,
) -> Result<(), JobError> {
    let (tenant, delivery) = tenant_and(&job, "delivery_id")?;
    let attempt = job
        .payload
        .get("attempt")
        .and_then(serde_json::Value::as_i64)
        .and_then(|a| i32::try_from(a).ok())
        .ok_or_else(|| JobError::Permanent("payload has no attempt".into()))?;
    let window = job
        .payload
        .get("window")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .ok_or_else(|| JobError::Permanent("payload has no window".into()))?
        .with_timezone(&chrono::Utc);
    let hooks = webhooks.ok_or_else(|| JobError::Retry("SECRETS_KEY is not configured".into()))?;
    let outcome = commerce::webhooks::deliver(&ctx.db, &hooks, tenant, delivery, attempt, window)
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::info!(%tenant, %delivery, attempt, ?outcome, "webhook delivery");
    Ok(())
}

/// Every 15 minutes: stuck assets, abandoned uploads, expired carts, stale indexes.
async fn ops_sweep(ctx: Ctx, _job: Job, storage: Storage, meili: Meili) -> Result<(), JobError> {
    let report = commerce::ops::sweep(&ctx.db, &storage, Some(&meili))
        .await
        .map_err(|e| JobError::Retry(e.to_string()))?;
    tracing::info!(?report, "sweep done");
    Ok(())
}
