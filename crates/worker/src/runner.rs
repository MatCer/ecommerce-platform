//! Leased job runner (spec §13, A14): N independent loops, each claiming one job at a time,
//! heartbeating while the handler runs, then completing or failing it with backoff.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use platform::queue::{self, Failed, Job};
use sqlx::PgPool;
use tokio::sync::watch;

/// Why a handler failed. Database errors are retryable.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("{0}")]
    Retry(String),
    /// Retrying cannot help (e.g. an invalid payload): the job goes straight to `dead`.
    #[error("{0}")]
    Permanent(String),
}

impl From<sqlx::Error> for JobError {
    fn from(e: sqlx::Error) -> Self {
        Self::Retry(e.to_string())
    }
}

/// What handlers get besides the job.
#[derive(Clone)]
pub struct Ctx {
    pub db: PgPool,
}

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send>>;
type Handler = Arc<dyn Fn(Ctx, Job) -> HandlerFuture + Send + Sync>;

/// Job kind -> handler. Handlers must be idempotent: a crash after the work but before
/// `complete` runs the job again.
#[derive(Clone, Default)]
pub struct Handlers(HashMap<&'static str, Handler>);

impl Handlers {
    pub fn register<F, Fut>(mut self, kind: &'static str, f: F) -> Self
    where
        F: Fn(Ctx, Job) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), JobError>> + Send + 'static,
    {
        self.0
            .insert(kind, Arc::new(move |ctx, job| Box::pin(f(ctx, job))));
        self
    }
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Lease owner prefix (host + pid); loop `i` claims as `<owner>/<i>`.
    pub owner: String,
    pub queues: Vec<String>,
    pub concurrency: usize,
    pub poll_interval: Duration,
    pub lease: Duration,
    pub heartbeat_every: Duration,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
}

impl RunnerConfig {
    pub fn new(owner: String, concurrency: usize) -> Self {
        Self {
            owner,
            queues: vec!["default".into()],
            concurrency,
            poll_interval: Duration::from_secs(1),
            lease: queue::DEFAULT_LEASE,
            heartbeat_every: queue::DEFAULT_LEASE / 3,
            backoff_base: Duration::from_secs(5),
            backoff_cap: Duration::from_secs(3600),
        }
    }
}

/// Media gets one dedicated slot: waiting encoders must not occupy every ordinary slot.
/// Queue routing (including jobs from older producers) lives in `queue.enqueue`.
pub async fn run_background_jobs(
    db: PgPool,
    handlers: Handlers,
    cfg: RunnerConfig,
    shutdown: watch::Receiver<bool>,
) {
    let mut media = cfg.clone();
    media.owner = format!("{}/media", cfg.owner);
    media.queues = vec!["media".into()];
    media.concurrency = 1;
    tokio::join!(
        run(db.clone(), handlers.clone(), cfg, shutdown.clone()),
        run(db, handlers, media, shutdown),
    );
}

/// Runs `cfg.concurrency` job loops until `shutdown` turns true. A job in progress finishes
/// first; if the process is killed instead, its lease expires and another worker reclaims it.
pub async fn run(
    db: PgPool,
    handlers: Handlers,
    cfg: RunnerConfig,
    shutdown: watch::Receiver<bool>,
) {
    let handlers = Arc::new(handlers);
    let cfg = Arc::new(cfg);
    let mut loops = tokio::task::JoinSet::new();
    for i in 0..cfg.concurrency {
        let owner = format!("{}/{i}", cfg.owner);
        loops.spawn(job_loop(
            db.clone(),
            handlers.clone(),
            cfg.clone(),
            owner,
            shutdown.clone(),
        ));
    }
    while loops.join_next().await.is_some() {}
}

async fn job_loop(
    db: PgPool,
    handlers: Arc<Handlers>,
    cfg: Arc<RunnerConfig>,
    owner: String,
    mut shutdown: watch::Receiver<bool>,
) {
    let ctx = Ctx { db: db.clone() };
    while !*shutdown.borrow() {
        match queue::claim(&db, &owner, &cfg.queues, 1, cfg.lease).await {
            Ok(mut jobs) if !jobs.is_empty() => {
                if let Some(job) = jobs.pop() {
                    process(&ctx, &handlers, &cfg, job).await;
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "claiming jobs failed"),
        }
        tokio::select! {
            _ = shutdown.changed() => {}
            () = tokio::time::sleep(cfg.poll_interval) => {}
        }
    }
}

async fn process(ctx: &Ctx, handlers: &Handlers, cfg: &RunnerConfig, job: Job) {
    let span = tracing::info_span!("job", id = job.id, kind = %job.kind, attempt = job.attempts);
    let started = std::time::Instant::now();
    tracing::info!(parent: &span, queue = %job.queue, "job started");

    // Spawned so a panicking handler fails the job instead of killing the loop.
    let mut task = match handlers.0.get(job.kind.as_str()) {
        Some(handler) => tokio::spawn(handler(ctx.clone(), job.clone())),
        // Unknown kinds are retried: a newer worker version may know them (rolling deploy).
        None => {
            let kind = job.kind.clone();
            tokio::spawn(
                async move { Err(JobError::Retry(format!("no handler for job kind {kind}"))) },
            )
        }
    };

    let mut heartbeat = tokio::time::interval(cfg.heartbeat_every);
    heartbeat.tick().await; // the first tick is immediate
    let outcome = loop {
        tokio::select! {
            joined = &mut task => break joined.unwrap_or_else(|e| {
                Err(JobError::Retry(if e.is_panic() { "handler panicked".into() } else { e.to_string() }))
            }),
            _ = heartbeat.tick() => match queue::heartbeat(&ctx.db, &job, cfg.lease).await {
                Ok(true) => {}
                Ok(false) => {
                    // Reclaimed by another worker: stop, and do not touch the job any more.
                    task.abort();
                    tracing::warn!(parent: &span, "lease lost, abandoning job");
                    return;
                }
                Err(e) => tracing::warn!(parent: &span, error = %e, "heartbeat failed"),
            },
        }
    };

    let recorded = match &outcome {
        Ok(()) => queue::complete(&ctx.db, &job)
            .await
            .map(|ok| if ok { "done" } else { "lease lost" }),
        Err(err) => {
            let retry_in = match err {
                JobError::Retry(_) => Some(queue::backoff(
                    job.attempts,
                    cfg.backoff_base,
                    cfg.backoff_cap,
                )),
                JobError::Permanent(_) => None,
            };
            queue::fail(&ctx.db, &job, &err.to_string(), retry_in)
                .await
                .map(|f| match f {
                    Failed::Retrying => "retrying",
                    Failed::Dead => "dead",
                    Failed::LeaseLost => "lease lost",
                })
        }
    };
    metrics::histogram!(
        "job_duration_seconds",
        "kind" => job.kind.clone(),
        "outcome" => if outcome.is_ok() { "ok" } else { "error" },
    )
    .record(started.elapsed().as_secs_f64());
    let elapsed_seconds = started.elapsed().as_secs_f64();
    match (recorded, outcome) {
        (Ok(state), Ok(())) => {
            tracing::info!(parent: &span, state, elapsed_seconds, "job finished")
        }
        (Ok(state), Err(e)) => {
            tracing::warn!(parent: &span, state, elapsed_seconds, error = %e, "job failed")
        }
        // The lease runs out and the job is retried; handlers are idempotent.
        (Err(e), _) => tracing::error!(parent: &span, error = %e, "recording job result failed"),
    }
}
