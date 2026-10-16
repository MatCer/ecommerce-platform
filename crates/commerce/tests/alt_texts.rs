#![allow(clippy::unwrap_used)]

use commerce::ai::settings::{self, AiSettings};
use platform::db::tenant_tx;
use sqlx::PgPool;
use testkit::storefront::Shop;

async fn setup(db: &PgPool, slug: &str) -> (PgPool, Shop) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let shop = testkit::storefront::shop(&runtime, slug).await;
    (runtime, shop)
}

async fn put(runtime: &PgPool, shop: &Shop, review_summaries: bool, alt_texts: bool) -> AiSettings {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let s = settings::put(
        &mut tx,
        "owner",
        &AiSettings {
            review_summaries,
            alt_texts,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    s
}

async fn get(runtime: &PgPool, shop: &Shop) -> AiSettings {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    settings::get(&mut tx).await.unwrap()
}

async fn purges(db: &PgPool, shop: &Shop) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = 'ai_settings.changed'",
    )
    .bind(shop.tenant)
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_alt_text_switch_is_off_by_default_and_does_not_purge_pages(db: PgPool) {
    let (runtime, shop) = setup(&db, "alt-switch").await;
    let off = AiSettings {
        review_summaries: false,
        alt_texts: false,
    };
    assert_eq!(get(&runtime, &shop).await, off);
    let on = AiSettings {
        review_summaries: false,
        alt_texts: true,
    };
    assert_eq!(put(&runtime, &shop, false, true).await, on);
    assert_eq!(get(&runtime, &shop).await, on);
    put(&runtime, &shop, false, false).await;
    assert_eq!(get(&runtime, &shop).await, off);
    assert_eq!(
        purges(&db, &shop).await,
        0,
        "the storefront shows no alt switch"
    );
}
