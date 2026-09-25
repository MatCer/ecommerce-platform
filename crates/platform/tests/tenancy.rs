//! Row-level security, grants and tenant_tx (spec §5.2, A8) against a real Postgres.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use platform::db::{TenantTx, tenant_tx};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

/// Every tenant table, with an insert that creates one row for the transaction's tenant.
const TENANT_TABLES: &[(&str, &str)] = &[
    (
        "markets",
        "INSERT INTO markets (tenant_id, code, name, country_codes, currency, default_locale, locales)
         VALUES ($1, 'x' || substr(md5(random()::text), 1, 8), 'X', '{SK}', 'EUR', 'sk', '{sk}')",
    ),
    (
        "staff_members",
        "INSERT INTO staff_members (tenant_id, user_id, email, role)
         VALUES ($1, md5(random()::text), 'x@example.test', 'staff')",
    ),
    (
        "audit_log",
        "INSERT INTO audit_log (tenant_id, actor, action, entity) VALUES ($1, 'u', 'test', 'thing')",
    ),
    (
        "idempotency_keys",
        "INSERT INTO idempotency_keys (tenant_id, operation, key, request_hash)
         VALUES ($1, 'op', md5(random()::text), 'h')",
    ),
];

async fn insert_row(tx: &mut TenantTx, table: &str) -> Result<(), sqlx::Error> {
    let sql = TENANT_TABLES.iter().find(|(t, _)| *t == table).unwrap().1;
    let tenant = tx.tenant_id();
    sqlx::query(sql)
        .bind(tenant)
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

async fn two_tenants(runtime: &PgPool) -> (Uuid, Uuid) {
    let (a, _) = testkit::tenant(runtime, "alpha").await;
    let (b, _) = testkit::tenant(runtime, "beta").await;
    for tenant in [a, b] {
        let mut tx = tenant_tx(runtime, tenant).await.unwrap();
        for (table, _) in TENANT_TABLES {
            if *table != "markets" {
                insert_row(&mut tx, table).await.unwrap();
            }
        }
        tx.commit().await.unwrap();
    }
    (a, b)
}

async fn count(conn: &mut PgConnection, table: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(conn)
        .await
}

fn is_denied(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(|e| e.code().as_deref() == Some("42501"))
}

/// Guard for future work packages: every table in `public` is a tenant table with forced RLS,
/// owned by app_owner, with an isolation policy for app_runtime.
#[sqlx::test(migrations = "../../migrations")]
async fn every_public_table_is_tenant_scoped_with_forced_rls(db: PgPool) {
    let rows = sqlx::query(
        "SELECT c.relname::text AS name, c.relrowsecurity, c.relforcerowsecurity,
                pg_get_userbyid(c.relowner)::text AS owner,
                EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = c.oid
                        AND a.attname = 'tenant_id' AND a.attnotnull) AS has_tenant,
                EXISTS (SELECT 1 FROM pg_policies p WHERE p.schemaname = 'public'
                        AND p.tablename = c.relname AND p.policyname = 'tenant_isolation'
                        AND 'app_runtime' = ANY (p.roles)) AS has_policy
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') AND c.relname <> '_sqlx_migrations'",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(rows.len() >= TENANT_TABLES.len());
    for row in rows {
        let name: String = row.get("name");
        assert!(row.get::<bool, _>("relrowsecurity"), "{name}: RLS disabled");
        assert!(
            row.get::<bool, _>("relforcerowsecurity"),
            "{name}: RLS not forced"
        );
        assert!(row.get::<bool, _>("has_tenant"), "{name}: no tenant_id");
        assert!(
            row.get::<bool, _>("has_policy"),
            "{name}: no tenant_isolation policy"
        );
        assert_eq!(row.get::<String, _>("owner"), "app_owner", "{name}: owner");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn runtime_role_is_not_privileged(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let row =
        sqlx::query("SELECT rolbypassrls, rolsuper FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&runtime)
            .await
            .unwrap();
    assert!(!row.get::<bool, _>("rolbypassrls"));
    assert!(!row.get::<bool, _>("rolsuper"));

    for sql in [
        "SELECT count(*) FROM queue.jobs",
        "SELECT count(*) FROM queue.outbox",
        "INSERT INTO queue.jobs (kind) VALUES ('x')",
    ] {
        let err = sqlx::query(sql).execute(&runtime).await.unwrap_err();
        assert!(is_denied(&err), "{sql}: {err}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenant_tables_fail_closed_without_context(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    two_tenants(&runtime).await;
    let mut conn = runtime.acquire().await.unwrap();
    for (table, _) in TENANT_TABLES {
        assert!(count(&mut conn, table).await.is_err(), "{table} readable");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn cross_tenant_reads_and_writes_are_denied(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (a, b) = two_tenants(&runtime).await;

    for (table, _) in TENANT_TABLES {
        let mut tx = tenant_tx(&runtime, a).await.unwrap();
        let visible: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE tenant_id <> $1"
        )))
        .bind(a)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(visible, 0, "{table}: other tenant's rows visible");
        assert_eq!(count(&mut tx, table).await.unwrap(), 1, "{table}: own row");

        // Writing a row for tenant B from tenant A's transaction violates the policy.
        let err = sqlx::query(TENANT_TABLES.iter().find(|(t, _)| t == table).unwrap().1)
            .bind(b)
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(is_denied(&err), "{table}: insert for B: {err}");
        tx.rollback().await.unwrap();

        if *table == "audit_log" {
            continue; // append-only, checked below
        }
        let mut tx = tenant_tx(&runtime, a).await.unwrap();
        let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = tenant_id WHERE tenant_id = $1"
        )))
        .bind(b)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
        let deleted = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE tenant_id = $1"
        )))
        .bind(b)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!((updated, deleted), (0, 0), "{table}: touched B's rows");
        // Moving an own row to tenant B is rejected too.
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = $1"
        )))
        .bind(b)
        .execute(&mut *tx)
        .await
        .unwrap_err();
        assert!(is_denied(&err), "{table}: move to B: {err}");
        tx.rollback().await.unwrap();
    }

    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    for (table, _) in TENANT_TABLES {
        assert_eq!(count(&mut tx, table).await.unwrap(), 1, "{table}: B's row");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_log_is_append_only(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (a, _) = two_tenants(&runtime).await;
    for sql in ["UPDATE audit_log SET action = 'x'", "DELETE FROM audit_log"] {
        let mut tx = tenant_tx(&runtime, a).await.unwrap();
        let err = sqlx::query(sql).execute(&mut *tx).await.unwrap_err();
        assert!(is_denied(&err), "{sql}: {err}");
    }
}

async fn assert_no_tenant_context(runtime: &PgPool) {
    let mut conn = runtime.acquire().await.unwrap();
    let setting: Option<String> =
        sqlx::query_scalar("SELECT nullif(current_setting('app.tenant_id', true), '')")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(setting, None, "tenant context leaked");
    assert!(count(&mut conn, "markets").await.is_err());
}

/// One connection, reused after each way a tenant transaction can end (A8).
#[sqlx::test(migrations = "../../migrations")]
async fn pooled_connection_never_keeps_tenant_context(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (a, b) = two_tenants(&runtime).await;

    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    assert_eq!(count(&mut tx, "markets").await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert_no_tenant_context(&runtime).await;

    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    assert_eq!(count(&mut tx, "markets").await.unwrap(), 1);
    tx.rollback().await.unwrap();
    assert_no_tenant_context(&runtime).await;

    // Cancellation: the request future is dropped mid-query, the transaction is never finished.
    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    let slow = sqlx::query("SELECT pg_sleep(5)").execute(&mut *tx);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), slow)
            .await
            .is_err()
    );
    drop(tx);
    assert_no_tenant_context(&runtime).await;

    // And the next tenant sees only its own data on the same pool.
    let mut tx = tenant_tx(&runtime, b).await.unwrap();
    let tenants: Vec<Uuid> = sqlx::query_scalar("SELECT tenant_id FROM markets")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(tenants, vec![b]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn membership_lookup_works_before_tenant_context(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let (b, _) = testkit::tenant(&runtime, "beta").await;
    testkit::staff(&runtime, a, "user-1", "admin").await;
    testkit::staff(&runtime, b, "user-1", "staff").await;

    let role = |tenant: Uuid, user: &'static str| {
        let runtime = runtime.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>("SELECT platform.staff_membership($1, $2)")
                .bind(user)
                .bind(tenant)
                .fetch_one(&runtime)
                .await
                .unwrap()
        }
    };
    assert_eq!(role(a, "user-1").await.as_deref(), Some("admin"));
    assert_eq!(role(b, "user-1").await.as_deref(), Some("staff"));
    assert_eq!(role(a, "user-2").await, None);

    let tenants: Vec<String> =
        sqlx::query_scalar("SELECT slug FROM platform.staff_tenants('user-1')")
            .fetch_all(&runtime)
            .await
            .unwrap();
    assert_eq!(tenants, vec!["alpha", "beta"]);

    // Suspended tenants grant nothing.
    sqlx::query("UPDATE platform.tenants SET status = 'suspended' WHERE id = $1")
        .bind(a)
        .execute(&runtime)
        .await
        .unwrap();
    assert_eq!(role(a, "user-1").await, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_idempotency_keys_are_purged(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let (a, _) = testkit::tenant(&runtime, "alpha").await;
    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    sqlx::query(
        "INSERT INTO idempotency_keys (tenant_id, operation, key, request_hash, created_at)
         VALUES ($1, 'op', 'old', 'h', now() - interval '25 hours'), ($1, 'op', 'new', 'h', now())",
    )
    .bind(a)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let purged: i64 = sqlx::query_scalar("SELECT platform.purge_idempotency_keys()")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM idempotency_keys")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(keys, vec!["new"]);
}
