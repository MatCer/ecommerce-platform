//! Background worker. WP0 skeleton: connects to the database and logs a heartbeat until
//! SIGTERM. The job runner, outbox dispatcher and cron leader arrive in WP1 (spec §13).

use std::time::Duration;

use anyhow::anyhow;
use platform::config::DbConfig;

const HEARTBEAT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    platform::telemetry::init().map_err(|e| anyhow!(e))?;
    let db = platform::db::pool(&DbConfig::from_env()?)?;
    tracing::info!("worker started");

    let shutdown = platform::shutdown::signal();
    tokio::pin!(shutdown);
    let mut tick = tokio::time::interval(HEARTBEAT);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = tick.tick() => match platform::db::ping(&db).await {
                Ok(()) => tracing::info!(database = "ok", "heartbeat"),
                Err(e) => tracing::warn!(database = "fail", error = %e, "heartbeat"),
            },
        }
    }

    db.close().await;
    tracing::info!("worker stopped");
    Ok(())
}
