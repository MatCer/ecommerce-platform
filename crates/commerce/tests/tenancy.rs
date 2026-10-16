//! Tenancy, markets, audit and idempotency against a real Postgres, as the runtime role.
#![allow(clippy::unwrap_used)]

use commerce::markets::{self, NewMarket, TaxMode};
use commerce::tenancy::{self, CheckoutMode, CheckoutOriginPolicy, NewTenant, Role};
use commerce::{audit, idempotency};
use platform::Error;
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;

fn owner(slug: &'static str) -> NewTenant<'static> {
    NewTenant {
        slug,
        name: "Demo shop",
        owner_user_id: "user-owner",
        owner_email: "owner@example.test",
    }
}

fn sk() -> NewMarket {
    NewMarket {
        code: "sk".into(),
        name: "Slovensko".into(),
        country_codes: vec!["SK".into()],
        currency: "EUR".into(),
        default_locale: "sk".into(),
        locales: vec!["sk".into()],
        tax_mode: TaxMode::Gross,
        is_default: false,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_tenant_sets_up_market_domain_owner_audit_and_event(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let created = tenancy::create_tenant(&runtime, &owner("demo"))
        .await
        .unwrap();
    assert_eq!(created.hostname, "demo.localhost");

    let resolved = tenancy::resolve_host(&runtime, "Demo.localhost:8180")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.tenant_id, created.tenant_id);
    assert_eq!(resolved.market_id, created.market_id);
    assert_eq!(
        (resolved.market_code.as_str(), resolved.currency.as_str()),
        ("cz", "CZK")
    );

    assert_eq!(
        tenancy::membership(&runtime, "user-owner", created.tenant_id)
            .await
            .unwrap(),
        Some(Role::Owner)
    );
    let mine = tenancy::memberships(&runtime, "user-owner").await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].slug, "demo");

    let mut tx = tenant_tx(&runtime, created.tenant_id).await.unwrap();
    let page = audit::list(&mut tx, None, 10).await.unwrap();
    assert_eq!(page.items[0].action, "tenant.created");
    assert_eq!(page.items[0].actor, "platform");

    let events: Vec<String> = sqlx::query_scalar("SELECT type FROM queue.outbox")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(events, vec!["tenant.created"]);

    let dup = tenancy::create_tenant(&runtime, &owner("demo"))
        .await
        .unwrap_err();
    assert_eq!(dup.code(), "already_exists");
    assert_eq!(
        tenancy::create_tenant(&runtime, &owner("api"))
            .await
            .unwrap_err()
            .code(),
        "reserved_slug"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn custom_domains_resolve_only_after_txt_verification(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let created = tenancy::create_tenant(&runtime, &owner("demo"))
        .await
        .unwrap();
    let domain = tenancy::add_domain(&runtime, "demo", "Shop.Example.CZ", None, false)
        .await
        .unwrap();
    assert!(!domain.verified);
    assert_eq!(domain.txt_name(), "_commerce-verification.shop.example.cz");
    assert!(
        tenancy::resolve_host(&runtime, "shop.example.cz")
            .await
            .unwrap()
            .is_none()
    );

    let wrong = vec!["commerce-verification=nope".to_owned()];
    assert!(
        !tenancy::verify_domain(&runtime, "shop.example.cz", &wrong)
            .await
            .unwrap()
    );
    let right = vec!["v=spf1 -all".to_owned(), domain.txt_value()];
    assert!(
        tenancy::verify_domain(&runtime, "shop.example.cz", &right)
            .await
            .unwrap()
    );
    let resolved = tenancy::resolve_host(&runtime, "shop.example.cz")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.tenant_id, created.tenant_id);

    // Local hostnames need no verification; unknown markets and tenants are 404.
    assert!(
        tenancy::add_domain(&runtime, "demo", "demo-2.localhost", Some("cz"), true)
            .await
            .unwrap()
            .verified
    );
    assert!(matches!(
        tenancy::add_domain(&runtime, "demo", "x.example.cz", Some("de"), false).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        tenancy::add_domain(&runtime, "nope", "y.example.cz", None, false).await,
        Err(Error::NotFound)
    ));
    assert_eq!(
        tenancy::add_domain(&runtime, "demo", "shop.example.cz", None, false)
            .await
            .unwrap_err()
            .code(),
        "already_exists"
    );

    // Suspended tenants stop resolving.
    sqlx::query("UPDATE platform.tenants SET status = 'suspended'")
        .execute(&runtime)
        .await
        .unwrap();
    assert!(
        tenancy::resolve_host(&runtime, "demo.localhost")
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn markets_are_created_with_audit_and_outbox_and_stay_in_their_tenant(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let a = tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    let b = tenancy::create_tenant(&runtime, &owner("beta"))
        .await
        .unwrap();

    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    let market = markets::create(
        &mut tx,
        "user-owner",
        &NewMarket {
            is_default: true,
            ..sk()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    let list = markets::list(&mut tx).await.unwrap();
    assert_eq!(
        list.iter()
            .map(|m| (m.code.as_str(), m.is_default))
            .collect::<Vec<_>>(),
        vec![("sk", true), ("cz", false)]
    );
    let entry = &audit::list(&mut tx, None, 1).await.unwrap().items[0];
    assert_eq!(
        (entry.action.as_str(), entry.actor.as_str()),
        ("market.created", "user-owner")
    );
    assert_eq!(entry.entity_id, Some(market.id.to_string()));
    let dup = markets::create(&mut tx, "user-owner", &sk())
        .await
        .unwrap_err();
    assert_eq!(dup.code(), "already_exists");
    drop(tx);

    let mut tx = tenant_tx(&runtime, b.tenant_id).await.unwrap();
    let codes: Vec<String> = markets::list(&mut tx)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.code)
        .collect();
    assert_eq!(codes, vec!["cz"]);
    assert_eq!(
        audit::list(&mut tx, None, 100).await.unwrap().items.len(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_log_pages_with_a_cursor(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 1).await;
    let a = tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    for n in 0..4 {
        audit::record(
            &mut tx,
            "u",
            &format!("thing.{n}"),
            "thing",
            None,
            &json!({}),
        )
        .await
        .unwrap();
    }
    let first = audit::list(&mut tx, None, 3).await.unwrap();
    let actions: Vec<_> = first.items.iter().map(|e| e.action.clone()).collect();
    assert_eq!(actions, vec!["thing.3", "thing.2", "thing.1"]);
    let second = audit::list(&mut tx, first.next_cursor, 3).await.unwrap();
    let actions: Vec<_> = second.items.iter().map(|e| e.action.clone()).collect();
    assert_eq!(actions, vec!["thing.0", "tenant.created"]);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        audit::list(&mut tx, None, 101).await.unwrap_err().code(),
        "invalid_limit"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn idempotency_replays_conflicts_and_serializes_concurrent_requests(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let a = tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    let b = tenancy::create_tenant(&runtime, &owner("beta"))
        .await
        .unwrap();
    let op = "POST /admin/v1/markets";

    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    assert_eq!(
        idempotency::begin(&mut tx, op, "k1", "h1").await.unwrap(),
        None
    );
    // A second request with the same key blocks until this transaction commits.
    let waiter = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
            idempotency::begin(&mut tx, op, "k1", "h1").await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiter.is_finished());
    idempotency::finish(&mut tx, op, "k1", 201, &json!({"id": 1}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let replay = waiter.await.unwrap().unwrap().unwrap();
    assert_eq!((replay.status, replay.body), (201, json!({"id": 1})));

    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    let err = idempotency::begin(&mut tx, op, "k1", "other")
        .await
        .unwrap_err();
    assert_eq!(err.code(), "idempotency_conflict");
    drop(tx);

    // A rolled-back attempt releases the key.
    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    assert_eq!(
        idempotency::begin(&mut tx, op, "k2", "h").await.unwrap(),
        None
    );
    tx.rollback().await.unwrap();
    let mut tx = tenant_tx(&runtime, a.tenant_id).await.unwrap();
    assert_eq!(
        idempotency::begin(&mut tx, op, "k2", "h").await.unwrap(),
        None
    );
    drop(tx);

    // Keys are per tenant.
    let mut tx = tenant_tx(&runtime, b.tenant_id).await.unwrap();
    assert_eq!(
        idempotency::begin(&mut tx, op, "k1", "h1").await.unwrap(),
        None
    );
}

fn db_code(e: &sqlx::Error) -> Option<String> {
    e.as_database_error()
        .and_then(|d| d.code().map(|c| c.into_owned()))
}

async fn set_policy(runtime: &PgPool, origin: &str, set: &str) -> Result<(), sqlx::Error> {
    let done = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE platform.checkout_origins SET {set} WHERE origin = $1"
    )))
    .bind(origin)
    .execute(runtime)
    .await?;
    assert_eq!(done.rows_affected(), 1, "{origin}");
    Ok(())
}

async fn refused(runtime: &PgPool, origin: &str, set: &str) {
    let err = set_policy(runtime, origin, set).await.unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("23514"), "{origin}: {set}");
}

async fn policies(runtime: &PgPool, host: &str) -> Vec<CheckoutOriginPolicy> {
    tenancy::resolve_host(runtime, host)
        .await
        .unwrap()
        .unwrap()
        .checkout_origins
}

fn policy(
    origin: &str,
    mode: CheckoutMode,
    mode_generation: i64,
    checkout_origin: &str,
) -> CheckoutOriginPolicy {
    CheckoutOriginPolicy {
        origin: origin.into(),
        mode,
        mode_generation,
        checkout_origin: checkout_origin.into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_mode_defaults_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("demo"))
        .await
        .unwrap();
    assert_eq!(
        policies(&runtime, "demo.localhost").await,
        vec![policy(
            "https://demo.localhost",
            CheckoutMode::Isolated,
            1,
            "https://checkout.demo.localhost"
        )]
    );

    let domain = tenancy::add_domain(&runtime, "demo", "shop.example.cz", None, false)
        .await
        .unwrap();
    assert!(
        tenancy::verify_domain(&runtime, "shop.example.cz", &[domain.txt_value()])
            .await
            .unwrap()
    );
    assert_eq!(
        policies(&runtime, "shop.example.cz").await,
        vec![policy(
            "https://shop.example.cz",
            CheckoutMode::Isolated,
            1,
            "https://checkout.shop.example.cz"
        )]
    );
    let history: Vec<String> =
        sqlx::query_scalar("SELECT history FROM platform.checkout_origins ORDER BY origin")
            .fetch_all(&runtime)
            .await
            .unwrap();
    assert_eq!(history, vec!["unknown_or_used", "unknown_or_used"]);

    refused(&runtime, "https://shop.example.cz", "mode = 'same_origin'").await;
    let err = sqlx::query(
        "INSERT INTO platform.checkout_origins (scheme, hostname, port, tenant_id, market_id)
         SELECT 'http', hostname, 80, tenant_id, market_id FROM platform.domains
         WHERE hostname = 'shop.example.cz'",
    )
    .execute(&runtime)
    .await
    .unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("23514"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn freshness_requires_full_history(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    tenancy::create_tenant(&runtime, &owner("beta"))
        .await
        .unwrap();
    tenancy::add_domain(&runtime, "alpha", "alpha-shop.localhost", None, false)
        .await
        .unwrap();

    set_policy(
        &runtime,
        "https://alpha.localhost",
        "history = 'platform_never_reassigned', mode = 'same_origin'",
    )
    .await
    .unwrap();
    assert_eq!(
        policies(&runtime, "alpha.localhost").await,
        vec![policy(
            "https://alpha.localhost",
            CheckoutMode::SameOrigin,
            2,
            "https://alpha.localhost"
        )]
    );
    refused(
        &runtime,
        "https://alpha-shop.localhost",
        "history = 'platform_never_reassigned'",
    )
    .await;

    sqlx::query(
        "UPDATE platform.hostname_claims SET claimed_at = NULL WHERE hostname = 'beta.localhost'",
    )
    .execute(&db)
    .await
    .unwrap();
    refused(
        &runtime,
        "https://beta.localhost",
        "history = 'platform_never_reassigned'",
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn mode_switch_moves_the_generation(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    let origin = "https://alpha.localhost";
    set_policy(
        &runtime,
        origin,
        "history = 'platform_never_reassigned', mode = 'same_origin'",
    )
    .await
    .unwrap();
    set_policy(&runtime, origin, "mode = 'isolated'")
        .await
        .unwrap();
    set_policy(&runtime, origin, "mode = 'isolated'")
        .await
        .unwrap();
    assert_eq!(
        policies(&runtime, "alpha.localhost").await,
        vec![policy(
            origin,
            CheckoutMode::Isolated,
            3,
            "https://checkout.alpha.localhost"
        )]
    );
    let err = set_policy(&runtime, origin, "mode_generation = 1")
        .await
        .unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("42501"));
    let err = sqlx::query(
        "INSERT INTO platform.checkout_origins
             (scheme, hostname, port, tenant_id, market_id, mode_generation)
         SELECT 'https', hostname, 8443, tenant_id, market_id, 9001 FROM platform.domains
         WHERE hostname = 'alpha.localhost'",
    )
    .execute(&runtime)
    .await
    .unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("42501"));
}

async fn wait_until_blocked<T>(db: &PgPool, task: &tokio::task::JoinHandle<T>) {
    while !task.is_finished() {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                            WHERE datname = current_database() AND wait_event_type = 'Lock')",
        )
        .fetch_one(db)
        .await
        .unwrap();
        if blocked {
            return;
        }
        tokio::task::yield_now().await;
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_domain_and_shop_registration_serialize(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 3).await;
    let alpha = tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();

    let mut config = runtime.begin().await.unwrap();
    sqlx::query("INSERT INTO platform.account_domains (domain) VALUES ('accounts.example')")
        .execute(&mut *config)
        .await
        .unwrap();
    let shop = tokio::spawn({
        let runtime = runtime.clone();
        async move { tenancy::add_domain(&runtime, "alpha", "shop.accounts.example", None, false).await }
    });
    wait_until_blocked(&db, &shop).await;
    config.commit().await.unwrap();
    assert_eq!(shop.await.unwrap().unwrap_err().code(), "reserved_hostname");

    let mut registration = runtime.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id)
         VALUES ('shop.other.example', $1, $2)",
    )
    .bind(alpha.tenant_id)
    .bind(alpha.market_id)
    .execute(&mut *registration)
    .await
    .unwrap();
    let reservation = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            sqlx::query("INSERT INTO platform.account_domains (domain) VALUES ('other.example')")
                .execute(&runtime)
                .await
        }
    });
    wait_until_blocked(&db, &reservation).await;
    registration.commit().await.unwrap();
    let err = reservation.await.unwrap().unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("23514"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn merchant_attestation_needs_actor_time_and_version(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    tenancy::add_domain(&runtime, "alpha", "alpha-shop.localhost", None, false)
        .await
        .unwrap();
    for incomplete in [
        "history = 'merchant_attested_never_served'",
        "history = 'merchant_attested_never_served', attested_by = 'user-owner', attested_at = now()",
        "attested_by = 'user-owner'",
    ] {
        refused(&runtime, "https://alpha-shop.localhost", incomplete).await;
    }
    set_policy(
        &runtime,
        "https://alpha-shop.localhost",
        "history = 'merchant_attested_never_served', attested_by = 'user-owner',
         attested_at = now(), attestation_version = 1, mode = 'same_origin'",
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn retired_platform_hostname_not_reassigned(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    let beta = tenancy::create_tenant(&runtime, &owner("beta"))
        .await
        .unwrap();

    sqlx::raw_sql(
        "DELETE FROM platform.checkout_origins WHERE hostname = 'alpha.localhost';
         DELETE FROM platform.domains WHERE hostname = 'alpha.localhost'",
    )
    .execute(&db)
    .await
    .unwrap();
    assert!(
        tenancy::resolve_host(&runtime, "alpha.localhost")
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(
        tenancy::add_domain(&runtime, "beta", "alpha.localhost", None, false)
            .await
            .unwrap_err()
            .code(),
        "already_exists"
    );
    let err = sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, verified_at)
         VALUES ('alpha.localhost', $1, $2, now())",
    )
    .bind(beta.tenant_id)
    .bind(beta.market_id)
    .execute(&db)
    .await
    .unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("23505"));

    for stmt in [
        "DELETE FROM platform.hostname_claims WHERE hostname = 'alpha.localhost'",
        "UPDATE platform.hostname_claims SET tenant_id = tenant_id WHERE hostname = 'alpha.localhost'",
    ] {
        let err = sqlx::query(stmt).execute(&runtime).await.unwrap_err();
        assert_eq!(db_code(&err).as_deref(), Some("42501"), "{stmt}");
    }

    tenancy::add_domain(&runtime, "alpha", "alpha.localhost", None, true)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn aliases_have_independent_policy(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    tenancy::add_domain(&runtime, "alpha", "alpha-alias.localhost", None, false)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO platform.checkout_origins (scheme, hostname, port, tenant_id, market_id)
         SELECT 'http', hostname, 8080, tenant_id, market_id FROM platform.domains
         WHERE hostname = 'alpha.localhost'",
    )
    .execute(&runtime)
    .await
    .unwrap();
    set_policy(
        &runtime,
        "https://alpha.localhost",
        "history = 'platform_never_reassigned', mode = 'same_origin'",
    )
    .await
    .unwrap();

    assert_eq!(
        policies(&runtime, "alpha.localhost").await,
        vec![
            policy(
                "http://alpha.localhost:8080",
                CheckoutMode::Isolated,
                1,
                "http://checkout.alpha.localhost:8080"
            ),
            policy(
                "https://alpha.localhost",
                CheckoutMode::SameOrigin,
                2,
                "https://alpha.localhost"
            ),
        ]
    );
    assert_eq!(
        policies(&runtime, "alpha-alias.localhost").await,
        vec![policy(
            "https://alpha-alias.localhost",
            CheckoutMode::Isolated,
            1,
            "https://checkout.alpha-alias.localhost"
        )]
    );
    refused(
        &runtime,
        "https://alpha-alias.localhost",
        "history = 'platform_never_reassigned'",
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn account_domain_cannot_host_storefront(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    tenancy::create_tenant(&runtime, &owner("alpha"))
        .await
        .unwrap();
    tenancy::add_domain(&runtime, "alpha", "shop.example.cz", None, false)
        .await
        .unwrap();

    let err = sqlx::query("INSERT INTO platform.account_domains (domain) VALUES ('example.cz')")
        .execute(&runtime)
        .await
        .unwrap_err();
    assert_eq!(db_code(&err).as_deref(), Some("23514"));

    sqlx::query("INSERT INTO platform.account_domains (domain) VALUES ('accounts.example')")
        .execute(&runtime)
        .await
        .unwrap();
    for host in [
        "accounts.example",
        "Alpha.Accounts.Example",
        "a.b.accounts.example",
    ] {
        assert_eq!(
            tenancy::add_domain(&runtime, "alpha", host, None, false)
                .await
                .unwrap_err()
                .code(),
            "reserved_hostname",
            "{host}"
        );
    }
    tenancy::add_domain(&runtime, "alpha", "myaccounts.example", None, false)
        .await
        .unwrap();
}

#[sqlx::test(migrations = false)]
async fn migration_backfills_populated_domains_isolated(db: PgPool) {
    let migrator = sqlx::migrate!("../../migrations");
    let before = migrator
        .iter()
        .map(|m| m.version)
        .filter(|v| *v < 20261023000000)
        .max()
        .unwrap();
    migrator.run_to(before, &db).await.unwrap();
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, market) = testkit::tenant(&runtime, "demo").await;
    sqlx::query(
        "INSERT INTO platform.domains (hostname, tenant_id, market_id, is_primary, verified_at)
         VALUES ('demo.localhost', $1, $2, true, now()), ('shop.example.cz', $1, $2, false, NULL)",
    )
    .bind(tenant)
    .bind(market)
    .execute(&db)
    .await
    .unwrap();

    migrator.run(&db).await.unwrap();
    migrator.run(&db).await.unwrap();

    let origins: Vec<(String, String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT origin, mode, mode_generation, history, attested_by
         FROM platform.checkout_origins ORDER BY origin",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    let backfilled = |origin: &str| {
        (
            origin.to_owned(),
            "isolated".to_owned(),
            1,
            "unknown_or_used".to_owned(),
            None,
        )
    };
    assert_eq!(
        origins,
        vec![
            backfilled("https://demo.localhost"),
            backfilled("https://shop.example.cz")
        ]
    );
    let claims: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT hostname, platform_issued, claimed_at IS NULL FROM platform.hostname_claims
         ORDER BY hostname",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(
        claims,
        vec![
            ("demo.localhost".to_owned(), false, true),
            ("shop.example.cz".to_owned(), false, true),
        ]
    );
    refused(
        &runtime,
        "https://demo.localhost",
        "history = 'platform_never_reassigned'",
    )
    .await;
}
