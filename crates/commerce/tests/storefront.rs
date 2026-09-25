//! Storefront tables (redirects, carts, handoffs, theme revisions) against a real Postgres as
//! the runtime role: tenant isolation and the service rules that live in SQL.
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use commerce::cart::{self, NewLine};
use commerce::redirects::{self, RedirectInput};
use commerce::storefront::{self, PublicUrls};
use commerce::{tenancy, themes};
use platform::db::tenant_tx;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const TABLES: &[&str] = &[
    "redirects",
    "carts",
    "cart_lines",
    "cart_coupons",
    "checkout_handoffs",
    "theme_revisions",
    "theme_active",
];

/// One row in every storefront table of `shop`.
async fn fill(runtime: &PgPool, shop: &Shop, artifact: &str) {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    redirects::create(
        &mut tx,
        "t",
        &RedirectInput {
            from_path: "/old".into(),
            to_path: "/new".into(),
            code: 301,
        },
    )
    .await
    .unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), shop.cz, None, Utc::now())
        .await
        .unwrap();
    let (id, token) = cart::create(&mut tx, &ctx).await.unwrap();
    let c = cart::find(&mut tx, &ctx, &token, None).await.unwrap();
    assert_eq!(c.id, id);
    cart::add_line(
        &mut tx,
        &ctx,
        &c,
        &NewLine {
            variant_id: shop.variants[0],
            quantity: 1,
        },
    )
    .await
    .unwrap();
    let coupon: Uuid = sqlx::query_scalar(
        "INSERT INTO coupons (tenant_id, code, kind, value) VALUES ($1, 'TEST10', 'percent', 1000) RETURNING id",
    )
    .bind(shop.tenant)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cart_coupons (tenant_id, cart_id, coupon_id) VALUES ($1, $2, $3)")
        .bind(shop.tenant)
        .bind(id)
        .bind(coupon)
        .execute(&mut *tx)
        .await
        .unwrap();
    cart::start_handoff(&mut tx, &c).await.unwrap();
    themes::activate_default(&mut tx, "t", artifact)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenants_cannot_reach_each_others_storefront_rows(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let a = testkit::storefront::shop(&runtime, "alpha").await;
    let b = testkit::storefront::shop(&runtime, "beta").await;
    let artifact = "0123456789abcdef0123456789abcdef";
    sqlx::query("INSERT INTO platform.theme_artifacts (id, kind) VALUES ($1, 'theme')")
        .bind(artifact)
        .execute(&runtime)
        .await
        .unwrap();
    fill(&runtime, &a, artifact).await;
    fill(&runtime, &b, artifact).await;

    for table in TABLES {
        let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
        let (own, foreign): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FILTER (WHERE tenant_id = $1), count(*) FILTER (WHERE tenant_id <> $1) FROM {table}"
        )))
        .bind(b.tenant)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(own > 0, "{table}: fixture row missing");
        assert_eq!(foreign, 0, "{table}: A's rows visible to B");
        let err = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE {table} SET tenant_id = $1"
        )))
        .bind(a.tenant)
        .execute(&mut *tx)
        .await
        .unwrap_err();
        assert_eq!(
            err.as_database_error().unwrap().code().as_deref(),
            Some("42501"),
            "{table}"
        );
        tx.rollback().await.unwrap();
    }

    // B cannot put A's variant into its cart: composite foreign keys.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let cart_b: Uuid = sqlx::query_scalar("SELECT id FROM carts")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let err = sqlx::query(
        "INSERT INTO cart_lines (tenant_id, cart_id, variant_id, quantity) VALUES ($1, $2, $3, 1)",
    )
    .bind(b.tenant)
    .bind(cart_b)
    .bind(a.variants[1])
    .execute(&mut *tx)
    .await
    .unwrap_err();
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    tx.rollback().await.unwrap();

    // And the service refuses A's variant for B's cart outright.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), b.cz, None, Utc::now())
        .await
        .unwrap();
    let (_, token) = cart::create(&mut tx, &ctx).await.unwrap();
    let c = cart::find(&mut tx, &ctx, &token, None).await.unwrap();
    let err = cart::add_line(
        &mut tx,
        &ctx,
        &c,
        &NewLine {
            variant_id: a.variants[0],
            quantity: 1,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "unknown_variant");
}

#[sqlx::test(migrations = "../../migrations")]
async fn storefront_tokens_resolve_their_tenant_only(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let a = testkit::storefront::shop(&runtime, "alpha").await;
    let b = testkit::storefront::shop(&runtime, "beta").await;
    assert_eq!(
        tenancy::storefront_token_tenant(&runtime, &a.token)
            .await
            .unwrap(),
        Some(a.tenant)
    );
    assert_eq!(
        tenancy::storefront_token_tenant(&runtime, &b.token)
            .await
            .unwrap(),
        Some(b.tenant)
    );
    for bad in ["", "sf_x", "sf_' OR 1=1 --", &a.token.to_uppercase()] {
        assert_eq!(
            tenancy::storefront_token_tenant(&runtime, bad)
                .await
                .unwrap(),
            None,
            "{bad}"
        );
    }
    // A suspended tenant's token stops working.
    sqlx::query("UPDATE platform.tenants SET status = 'suspended' WHERE id = $1")
        .bind(a.tenant)
        .execute(&runtime)
        .await
        .unwrap();
    assert_eq!(
        tenancy::storefront_token_tenant(&runtime, &a.token)
            .await
            .unwrap(),
        None
    );
    // A tenant created through the service gets a token and resolves with it.
    let created = tenancy::create_tenant(
        &runtime,
        &tenancy::NewTenant {
            slug: "gamma",
            name: "Gamma",
            owner_user_id: "u",
            owner_email: "u@example.test",
        },
    )
    .await
    .unwrap();
    let resolved = tenancy::resolve_host(&runtime, "gamma.localhost")
        .await
        .unwrap()
        .unwrap();
    assert!(resolved.storefront_token.starts_with("sf_") && resolved.storefront_token.len() == 67);
    assert_eq!(
        tenancy::storefront_token_tenant(&runtime, &resolved.storefront_token)
            .await
            .unwrap(),
        Some(created.tenant_id)
    );
    assert_eq!(
        resolved.theme_artifact, None,
        "no default theme published yet"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn publishing_keeps_recent_artifacts_retained(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let a = testkit::storefront::shop(&runtime, "alpha").await;
    let ids: Vec<String> = (1..=5).map(|i| format!("{i:032x}")).collect();
    for id in &ids {
        sqlx::query("INSERT INTO platform.theme_artifacts (id, kind) VALUES ($1, 'theme')")
            .bind(id)
            .execute(&runtime)
            .await
            .unwrap();
        assert_eq!(
            themes::publish_default(&runtime, "t", id).await.unwrap(),
            vec![a.tenant]
        );
    }
    // Everything superseded within 7 days stays retained (newest first).
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    let active = themes::active(&mut tx).await.unwrap();
    assert_eq!(active.artifact_id.as_deref(), Some(ids[4].as_str()));
    assert_eq!(
        active.retained,
        [
            ids[3].clone(),
            ids[2].clone(),
            ids[1].clone(),
            ids[0].clone()
        ]
    );
    // Older than 7 days: only the previous 3 revisions remain.
    sqlx::query("UPDATE theme_revisions SET published_at = now() - interval '30 days'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let active = themes::active(&mut tx).await.unwrap();
    assert_eq!(
        active.retained,
        [ids[3].clone(), ids[2].clone(), ids[1].clone()]
    );
    tx.rollback().await.unwrap();

    // A custom revision (M3) is left alone by default publishes.
    let mut tx = tenant_tx(&runtime, a.tenant).await.unwrap();
    sqlx::query("UPDATE theme_revisions SET origin = 'custom'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        themes::publish_default(&runtime, "t", &ids[0])
            .await
            .unwrap()
            .is_empty()
    );
}
