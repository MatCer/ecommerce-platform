#![allow(clippy::unwrap_used)]
use commerce::{staff, tenancy::Role};
use platform::db::tenant_tx;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_owner_demotions_keep_one_owner(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let (tenant, _) = testkit::tenant(&runtime, "owner-race").await;
    testkit::staff(&runtime, tenant, "a", "owner").await;
    testkit::staff(&runtime, tenant, "b", "owner").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let members = staff::list(&mut tx, "a").await.unwrap();
    tx.commit().await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let demote = |id, actor: &'static str| {
        let runtime = runtime.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            barrier.wait().await;
            let result = staff::change_role(&mut tx, actor, id, Role::Admin).await;
            match result {
                Ok(_) => {
                    tx.commit().await.unwrap();
                    Ok(())
                }
                Err(e) => {
                    tx.rollback().await.unwrap();
                    Err(e.code().to_owned())
                }
            }
        })
    };
    let (a, b) = tokio::join!(demote(members[0].id, "a"), demote(members[1].id, "b"));
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results.iter().find_map(|r| r.as_ref().err()).unwrap(),
        "last_owner"
    );
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM staff_members WHERE role = 'owner'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_trigram_search_finds_name_and_sku_substrings(db: PgPool) {
    use commerce::catalog::products::{self, ProductFilter};
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (tenant, _) = testkit::tenant(&runtime, "trigram-search").await;
    let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
    let id: uuid::Uuid =
        sqlx::query_scalar("INSERT INTO products (tenant_id) VALUES ($1) RETURNING id")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    sqlx::query("INSERT INTO product_translations (tenant_id, product_id, locale, name, slug) VALUES ($1, $2, 'cs', 'Červené bavlněné tričko', 'tricko'), ($1, $2, 'en', 'Cotton T-shirt', 't-shirt')").bind(tenant).bind(id).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO variants (tenant_id, product_id, sku, position, is_default) VALUES ($1, $2, 'TSHIRT-RED-XL', 0, true)").bind(tenant).bind(id).execute(&mut *tx).await.unwrap();
    for query in ["bavlně", "tton t-", "irt-red"] {
        let page = products::list(
            &mut tx,
            &ProductFilter {
                q: Some(query.into()),
                ..Default::default()
            },
            None,
            10,
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1, "{query}");
        assert_eq!(page.items[0].id, id);
    }
    for index in ["product_translations_name_trgm", "variants_sku_trgm"] {
        let definition: String =
            sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
                .bind(index)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(definition.contains("USING gin"));
        assert!(definition.contains("gin_trgm_ops"));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_search_function_is_tenant_scoped_and_indexable(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 2).await;
    let (a, _) = testkit::tenant(&runtime, "search-a").await;
    let (b, _) = testkit::tenant(&runtime, "search-b").await;
    for tenant in [a, b] {
        let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
        let id: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO products (tenant_id) VALUES ($1) RETURNING id")
                .bind(tenant)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO product_translations (tenant_id, product_id, locale, name, slug)
             VALUES ($1, $2, 'cs', 'Sdílený název', 'sdileny')",
        )
        .bind(tenant)
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    // Only the current tenant's product matches.
    let mut tx = tenant_tx(&runtime, a).await.unwrap();
    let ids: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT platform.search_product_ids('%sdílený%')")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(ids.len(), 1);
    let owner: uuid::Uuid = sqlx::query_scalar("SELECT tenant_id FROM products WHERE id = $1")
        .bind(ids[0])
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(owner, a);
    tx.commit().await.unwrap();

    // Without tenant context the function fails closed (a missing setting raises, a reset
    // one is '' and fails the uuid cast).
    let err = sqlx::query_scalar::<_, uuid::Uuid>("SELECT platform.search_product_ids('%a%')")
        .fetch_all(&runtime)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("app.tenant_id") || err.contains("uuid"),
        "{err}"
    );

    // The function body runs as app_owner without RLS quals in the way, so it can use the
    // trigram index (app_runtime's direct ILIKE could not: ILIKE is not leakproof).
    let mut tx = db.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(a.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();
    const QUERY: &str =
        "EXPLAIN SELECT product_id FROM product_translations WHERE name ILIKE '%sdíl%'";
    let owner_plan: Vec<String> = sqlx::query_scalar(QUERY).fetch_all(&mut *tx).await.unwrap();
    assert!(
        owner_plan
            .iter()
            .any(|l| l.contains("product_translations_name_trgm")),
        "{owner_plan:#?}"
    );
    sqlx::query("SET LOCAL ROLE app_runtime")
        .execute(&mut *tx)
        .await
        .unwrap();
    let runtime_plan: Vec<String> = sqlx::query_scalar(QUERY).fetch_all(&mut *tx).await.unwrap();
    assert!(
        !runtime_plan
            .iter()
            .any(|l| l.contains("product_translations_name_trgm")),
        "RLS no longer blocks the index; the definer function may be unnecessary: {runtime_plan:#?}"
    );
}
