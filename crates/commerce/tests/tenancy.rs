//! Tenancy, markets, audit and idempotency against a real Postgres, as the runtime role.
#![allow(clippy::unwrap_used)]

use commerce::markets::{self, NewMarket, TaxMode};
use commerce::tenancy::{self, NewTenant, Role};
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
