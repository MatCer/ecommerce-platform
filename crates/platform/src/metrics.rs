//! Prometheus metrics (spec §15): a process-wide `metrics` recorder rendered as the text
//! exposition format on a separate internal listener (`METRICS_BIND`), which the public proxy
//! never routes. Metric names:
//! - api: `http_request_duration_seconds{method,route,status}` (histogram)
//! - worker: `job_duration_seconds{kind,outcome}` (histogram), `jobs_queue_depth{kind,status}`,
//!   `jobs_lag_seconds{kind}` (oldest due, still queued job), `outbox_lag_seconds`

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tokio::sync::watch;

const LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const JOB_BUCKETS: &[f64] = &[0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0];

/// Installs the global recorder. Call once per process.
pub fn install() -> Result<PrometheusHandle, String> {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("http_request_duration_seconds".into()),
            LATENCY_BUCKETS,
        )
        .and_then(|b| {
            b.set_buckets_for_metric(Matcher::Full("job_duration_seconds".into()), JOB_BUCKETS)
        })
        .and_then(PrometheusBuilder::install_recorder)
        .map_err(|e| e.to_string())
}

/// Serves `GET /metrics` on `bind` until `shutdown`, running the recorder's upkeep.
pub async fn serve(
    bind: SocketAddr,
    handle: PrometheusHandle,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let upkeep = handle.clone();
    let mut stop_upkeep = shutdown.clone();
    tokio::spawn(async move {
        while !*stop_upkeep.borrow() {
            upkeep.run_upkeep();
            tokio::select! {
                _ = stop_upkeep.changed() => {}
                () = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
        }
    });
    let app = Router::new().route(
        "/metrics",
        get(move || {
            let body = handle.render();
            async move {
                (
                    [(
                        header::CONTENT_TYPE,
                        "text/plain; version=0.0.4; charset=utf-8",
                    )],
                    body,
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(addr = %bind, "metrics listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !*shutdown.borrow() {
                if shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
}
