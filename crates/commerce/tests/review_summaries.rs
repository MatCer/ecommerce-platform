//! SHOP-36 against a real Postgres as the runtime role, with the fake provider: the opt-in
//! switch, the minimum of published verified reviews, one metered call per input change, the
//! AI label that gates what the shop shows, failures keeping the old summary, and tenant
//! isolation of the new tables.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use commerce::ai::review_summary::{self, AiSettings};
use commerce::ai::{Ai, Outcome};
use commerce::reviews::{self, Status};
use commerce::storefront::{self, PublicUrls};
use platform::ai::{Client, Fake};
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const CS: &str = "Ukázkové shrnutí od demo AI: zákazníci hodnotí „Product TEE“ průměrně 4,6 z 5.";
const SK: &str =
    "Ukážkové zhrnutie od demo AI: zákazníci hodnotia „Product TEE“ priemerne 4,6 z 5.";

async fn setup(db: &PgPool, slug: &str) -> (PgPool, Shop) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let shop = testkit::storefront::shop(&runtime, slug).await;
    (runtime, shop)
}

async fn review(runtime: &PgPool, shop: &Shop, rating: i16, verified: bool) -> Uuid {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let id = sqlx::query_scalar(
        "INSERT INTO reviews (tenant_id, product_id, customer_name, rating, title, body,
                              verified, locale, status, published_at)
         VALUES ($1, $2, 'Jana', $3, 'Dobré', 'Sedí dobře.', $4, 'cs', 'published', now())
         RETURNING id",
    )
    .bind(shop.tenant)
    .bind(shop.product)
    .bind(rating)
    .bind(verified)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn switch(runtime: &PgPool, shop: &Shop, on: bool) -> AiSettings {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let s = review_summary::put_settings(
        &mut tx,
        "owner",
        &AiSettings {
            review_summaries: on,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    s
}

async fn run(runtime: &PgPool, ai: &Ai, shop: &Shop) {
    assert_eq!(
        review_summary::run(runtime, ai, shop.tenant, shop.product, false)
            .await
            .unwrap(),
        Outcome::Done
    );
}

/// Tenant tables are read as the runtime role (FORCE RLS hides them from the owner).
async fn count(runtime: &PgPool, sql: &'static str, tenant: Uuid) -> i64 {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    sqlx::query_scalar(sql)
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn usage(runtime: &PgPool, shop: &Shop) -> i64 {
    count(
        runtime,
        "SELECT count(*) FROM ai_usage WHERE tenant_id = $1 AND feature = 'review_summary'",
        shop.tenant,
    )
    .await
}

async fn summaries(runtime: &PgPool, shop: &Shop) -> Vec<(String, String, i32)> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    sqlx::query_as(
        "SELECT locale, summary, review_count FROM review_summaries WHERE tenant_id = $1
         ORDER BY locale",
    )
    .bind(shop.tenant)
    .fetch_all(&mut *tx)
    .await
    .unwrap()
}

async fn jobs(db: &PgPool, shop: &Shop) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM queue.jobs WHERE tenant_id = $1 AND kind = 'ai.review_summary'",
    )
    .bind(shop.tenant)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn changed_events(db: &PgPool, shop: &Shop) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT payload FROM queue.outbox WHERE tenant_id = $1
           AND type IN ('review_summary.changed', 'ai_settings.changed') ORDER BY id",
    )
    .bind(shop.tenant)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn shown(runtime: &PgPool, shop: &Shop, market: Uuid) -> Option<(String, String, i32)> {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let ctx = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
        .await
        .unwrap();
    let r = reviews::for_product(&mut tx, &ctx, shop.product)
        .await
        .unwrap();
    r.ai_summary.map(|s| (s.locale, s.text, s.review_count))
}

#[sqlx::test(migrations = "../../migrations")]
async fn nothing_happens_while_the_switch_is_off(db: PgPool) {
    let (runtime, shop) = setup(&db, "off").await;
    let ai = Ai::fake();
    for _ in 0..5 {
        review(&runtime, &shop, 5, true).await;
    }
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    assert_eq!(
        review_summary::settings(&mut tx).await.unwrap(),
        AiSettings {
            review_summaries: false
        }
    );
    tx.commit().await.unwrap();
    run(&runtime, &ai, &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 0);
    assert!(summaries(&runtime, &shop).await.is_empty());
    assert_eq!(shown(&runtime, &shop, shop.cz).await, None);
}

#[sqlx::test(migrations = "../../migrations")]
async fn summaries_follow_published_verified_reviews(db: PgPool) {
    let (runtime, shop) = setup(&db, "summaries").await;
    let ai = Ai::fake();
    for rating in [5, 5, 4, 4] {
        review(&runtime, &shop, rating, true).await;
    }
    review(&runtime, &shop, 1, false).await;
    assert_eq!(
        switch(&runtime, &shop, true).await,
        AiSettings {
            review_summaries: true
        }
    );
    assert_eq!(jobs(&db, &shop).await, 0, "below the minimum");

    // Four verified (the unverified one does not count): no summary, no call.
    run(&runtime, &ai, &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 0);
    assert!(summaries(&runtime, &shop).await.is_empty());

    let fifth = review(&runtime, &shop, 5, true).await;
    run(&runtime, &ai, &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 1);
    assert_eq!(
        summaries(&runtime, &shop).await,
        vec![
            ("cs".to_owned(), CS.to_owned(), 5),
            ("sk".to_owned(), SK.to_owned(), 5)
        ]
    );
    let marks: Vec<(String, String, String, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT locale, field, feature, proposal_id, accepted_by FROM ai_marks
         WHERE tenant_id = $1 AND entity_type = 'product' AND entity_id = $2 ORDER BY locale",
    )
    .bind(shop.tenant)
    .bind(shop.product.to_string())
    .fetch_all(&mut *tenant_tx(&runtime, shop.tenant).await.unwrap())
    .await
    .unwrap();
    let mark = |l: &str| {
        (
            l.to_owned(),
            "review_summary".to_owned(),
            "review_summary".to_owned(),
            None,
            "system".to_owned(),
        )
    };
    assert_eq!(marks, vec![mark("cs"), mark("sk")]);
    // Switching on purged the tenant; the new summary purges the product.
    assert_eq!(
        changed_events(&db, &shop).await,
        vec![json!({}), json!({ "product_id": shop.product })]
    );
    assert_eq!(
        shown(&runtime, &shop, shop.cz).await,
        Some(("cs".to_owned(), CS.to_owned(), 5))
    );
    assert_eq!(
        shown(&runtime, &shop, shop.sk).await,
        Some(("sk".to_owned(), SK.to_owned(), 5))
    );

    // Unchanged input and a reply (not input) cost nothing.
    run(&runtime, &ai, &shop).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    reviews::set_reply(&mut tx, "clerk", fifth, Some("Děkujeme!"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &ai, &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 1);
    assert_eq!(changed_events(&db, &shop).await.len(), 2);

    // Off hides at once; on again shows the cached text and queues a refresh.
    switch(&runtime, &shop, false).await;
    assert_eq!(shown(&runtime, &shop, shop.cz).await, None);
    switch(&runtime, &shop, true).await;
    assert_eq!(jobs(&db, &shop).await, 1);
    assert_eq!(
        shown(&runtime, &shop, shop.cz).await,
        Some(("cs".to_owned(), CS.to_owned(), 5))
    );
    let purges = changed_events(&db, &shop).await;
    assert_eq!(&purges[2..], [json!({}), json!({})], "tenant purges");

    // Text that no longer matches its AI label never reaches the shop.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE review_summaries SET summary = 'Nejlepší!' WHERE locale = 'cs'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(shown(&runtime, &shop, shop.cz).await, None);

    // Below the minimum again: summaries and labels are gone.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    reviews::set_status(&mut tx, "clerk", fifth, Status::Hidden)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &ai, &shop).await;
    assert!(summaries(&runtime, &shop).await.is_empty());
    let left = "SELECT count(*) FROM ai_marks WHERE tenant_id = $1 AND field = 'review_summary'";
    assert_eq!(count(&runtime, left, shop.tenant).await, 0);
    assert_eq!(
        changed_events(&db, &shop).await.last(),
        Some(&json!({ "product_id": shop.product }))
    );
    assert_eq!(usage(&runtime, &shop).await, 1);
}

fn scripted(output: &'static str) -> Ai {
    let fake = Fake::new([("review_summary", output)]).unwrap();
    Ai::fake_with_client(Some(Client::Fake(Arc::new(fake))))
}

#[sqlx::test(migrations = "../../migrations")]
async fn rejected_output_and_used_up_quota_keep_the_old_summary(db: PgPool) {
    let (runtime, shop) = setup(&db, "keep").await;
    for rating in [5, 5, 4, 4, 5] {
        review(&runtime, &shop, rating, true).await;
    }
    switch(&runtime, &shop, true).await;
    run(&runtime, &Ai::fake(), &shop).await;
    review(&runtime, &shop, 3, true).await;

    for bad in [
        r#"{"summaries": [{"locale": "cs", "summary": "Jen česky."}]}"#,
        r#"{"summaries": [{"locale": "cs", "summary": "Viz https://x.example"},
                          {"locale": "sk", "summary": "Pozri"}]}"#,
        r#"{"summaries": [{"locale": "cs", "summary": "<b>Super</b>"},
                          {"locale": "sk", "summary": "Super"}]}"#,
    ] {
        run(&runtime, &scripted(bad), &shop).await;
    }
    assert_eq!(usage(&runtime, &shop).await, 4, "failed calls are metered");
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 0 WHERE id = $1")
        .bind(shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    run(&runtime, &Ai::fake(), &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 4, "no call over the quota");
    assert_eq!(
        summaries(&runtime, &shop).await,
        vec![
            ("cs".to_owned(), CS.to_owned(), 5),
            ("sk".to_owned(), SK.to_owned(), 5)
        ]
    );
    assert_eq!(
        shown(&runtime, &shop, shop.cz).await,
        Some(("cs".to_owned(), CS.to_owned(), 5))
    );

    // Without a provider (production without a key) the job just finishes.
    let disabled = Ai::fake_with_client(None);
    run(&runtime, &disabled, &shop).await;
    assert_eq!(usage(&runtime, &shop).await, 4);
}

#[sqlx::test(migrations = "../../migrations")]
async fn summaries_and_settings_are_tenant_isolated(db: PgPool) {
    let (runtime, a) = setup(&db, "iso-a").await;
    let b = testkit::storefront::shop(&runtime, "iso-b").await;
    for rating in [5, 5, 4, 4, 5] {
        review(&runtime, &a, rating, true).await;
    }
    switch(&runtime, &a, true).await;
    run(&runtime, &Ai::fake(), &a).await;
    assert_eq!(summaries(&runtime, &a).await.len(), 2);

    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let seen: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM review_summaries), (SELECT count(*) FROM ai_settings)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(seen, (0, 0));
    assert_eq!(
        review_summary::settings(&mut tx).await.unwrap(),
        AiSettings {
            review_summaries: false
        }
    );
    let forged = sqlx::query(
        "INSERT INTO ai_settings (tenant_id, review_summaries) VALUES ($1, false)
         ON CONFLICT (tenant_id) DO UPDATE SET review_summaries = false",
    )
    .bind(a.tenant)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err());
    drop(tx);

    // B's job for A's product finds nothing to summarize.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    review_summary::put_settings(
        &mut tx,
        "owner",
        &AiSettings {
            review_summaries: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        review_summary::run(&runtime, &Ai::fake(), b.tenant, a.product, false)
            .await
            .unwrap(),
        Outcome::Done
    );
    assert_eq!(usage(&runtime, &b).await, 0);
    assert_eq!(summaries(&runtime, &a).await.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn switching_on_refreshes_summaries_that_fell_below_the_minimum_while_off(db: PgPool) {
    let (runtime, shop) = setup(&db, "stale").await;
    let ai = Ai::fake();
    let mut ids = Vec::new();
    for rating in [5, 5, 4, 4, 5] {
        ids.push(review(&runtime, &shop, rating, true).await);
    }
    switch(&runtime, &shop, true).await;
    run(&runtime, &ai, &shop).await;
    assert_eq!(summaries(&runtime, &shop).await.len(), 2);

    switch(&runtime, &shop, false).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    reviews::set_status(&mut tx, "clerk", ids[0], Status::Hidden)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    run(&runtime, &ai, &shop).await;

    switch(&runtime, &shop, true).await;
    assert_eq!(jobs(&db, &shop).await, 2, "the first backfill and this one");
    run(&runtime, &ai, &shop).await;
    assert!(summaries(&runtime, &shop).await.is_empty());
    assert_eq!(shown(&runtime, &shop, shop.cz).await, None);
}
