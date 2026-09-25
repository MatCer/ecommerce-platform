use std::ops::{Deref, DerefMut};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::config::DbConfig;

/// Lazy pool: the process starts even while Postgres is down, and `/readyz` reports it.
pub fn pool(cfg: &DbConfig) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy(&cfg.url)
}

pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}

/// A transaction bound to one tenant (spec §5.2, A8). Tenant tables are only ever queried
/// through this type: row-level security reads the transaction-local `app.tenant_id`, which
/// Postgres discards at commit or rollback, so a pooled connection never carries a previous
/// tenant. Without it, tenant-table queries raise instead of returning rows (fail closed).
///
/// Dereferences to the connection: `query.fetch_all(&mut **tx)`.
pub struct TenantTx {
    tx: Transaction<'static, Postgres>,
    tenant_id: Uuid,
}

/// Begins a transaction scoped to `tenant_id`.
pub async fn tenant_tx(pool: &PgPool, tenant_id: Uuid) -> Result<TenantTx, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "SELECT set_config('app.tenant_id', $1, true)",
        tenant_id.to_string()
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(TenantTx { tx, tenant_id })
}

impl TenantTx {
    pub fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.tx.rollback().await
    }
}

impl Deref for TenantTx {
    type Target = PgConnection;

    fn deref(&self) -> &PgConnection {
        &self.tx
    }
}

impl DerefMut for TenantTx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.tx
    }
}
