//! WP16 against a real Postgres as the runtime role: review tokens per delivered order line
//! (single use, expiring, rotated on re-issue), submission limits, moderation, the storefront
//! summary + JSON-LD (published reviews only), and tenant isolation of the new tables.
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use commerce::reviews::{self, ReviewInput, Status};
use commerce::storefront::{self, Context, PublicUrls};
use platform::Error;
use platform::db::{TenantTx, tenant_tx};
use sqlx::PgPool;
use testkit::storefront::{Shop, raw_order};
use uuid::Uuid;

async fn run<T>(
    runtime: &PgPool,
    tenant: Uuid,
    f: impl AsyncFnOnce(&mut TenantTx) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let out = f(&mut tx).await?;
    tx.commit().await.unwrap();
    Ok(out)
}

async fn ctx(tx: &mut TenantTx, shop: &Shop) -> Context {
    storefront::context(tx, &PublicUrls::default(), shop.cz, None, Utc::now())
        .await
        .unwrap()
}

fn input(token: &str, rating: i16) -> ReviewInput {
    ReviewInput {
        token: token.into(),
        rating,
        name: "Jana N.".into(),
        title: "Skvělé".into(),
        body: "Sedí přesně, <b>doporučuji</b>.".into(),
    }
}

async fn submit(runtime: &PgPool, shop: &Shop, i: &ReviewInput, ip: &[u8]) -> Result<Uuid, Error> {
    run(runtime, shop.tenant, async |tx| {
        let c = ctx(tx, shop).await;
        reviews::submit(tx, &c, i, Some(ip)).await
    })
    .await
}

async fn issue(
    runtime: &PgPool,
    shop: &Shop,
    order: Uuid,
) -> Result<Vec<reviews::IssuedToken>, Error> {
    run(runtime, shop.tenant, async |tx| {
        reviews::issue_tokens(tx, order, Utc::now()).await
    })
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn tokens_are_per_delivered_line_single_use_and_expiring(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-tokens").await;

    let shipped = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "shipped").await;
    assert_eq!(
        issue(&runtime, &shop, shipped).await.unwrap_err().code(),
        "order_not_delivered"
    );
    assert!(matches!(
        issue(&runtime, &shop, Uuid::now_v7()).await,
        Err(Error::NotFound)
    ));

    let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let first = issue(&runtime, &shop, order).await.unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].product_id, shop.product);
    assert!(first[0].expires_at > Utc::now() + Duration::days(89));
    // Re-issuing rotates: the old link stops working.
    let tokens = issue(&runtime, &shop, order).await.unwrap();
    assert_ne!(tokens[0].token, first[0].token);
    let old = run(&runtime, shop.tenant, async |tx| {
        reviews::invitation(tx, &first[0].token).await
    })
    .await;
    assert!(matches!(old, Err(Error::NotFound)));
    let token = tokens[0].token.clone();
    let inv = run(&runtime, shop.tenant, async |tx| {
        reviews::invitation(tx, &token).await
    })
    .await
    .unwrap();
    assert_eq!(inv.product_name, "Tričko");

    // Invalid input leaves the token usable.
    let mut bad = input(&token, 5);
    bad.body = "   ".into();
    assert_eq!(
        submit(&runtime, &shop, &bad, &[1; 32])
            .await
            .unwrap_err()
            .code(),
        "invalid_body"
    );
    let id = submit(&runtime, &shop, &input(&token, 5), &[1; 32])
        .await
        .unwrap();
    // Single use.
    assert!(matches!(
        submit(&runtime, &shop, &input(&token, 4), &[1; 32]).await,
        Err(Error::NotFound)
    ));
    let r = run(&runtime, shop.tenant, async |tx| reviews::get(tx, id).await)
        .await
        .unwrap();
    assert_eq!(r.status, Status::Pending);
    assert!(r.verified);
    assert_eq!(r.order_id, Some(order));
    assert_eq!(
        r.body, "Sedí přesně, <b>doporučuji</b>.",
        "stored as plain text"
    );
    // A reviewed line gets no new token.
    assert!(issue(&runtime, &shop, order).await.unwrap().is_empty());

    // Expired links are refused.
    let other = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let t = issue(&runtime, &shop, other).await.unwrap().remove(0).token;
    run(&runtime, shop.tenant, async |tx| {
        sqlx::query(
            "UPDATE review_tokens SET expires_at = now() - interval '1 second' WHERE order_id = $1",
        )
        .bind(other)
        .execute(&mut **tx)
        .await?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(matches!(
        submit(&runtime, &shop, &input(&t, 5), &[1; 32]).await,
        Err(Error::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_submissions_consume_a_token_once(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-race").await;
    let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let token = issue(&runtime, &shop, order).await.unwrap().remove(0).token;
    let mut set = tokio::task::JoinSet::new();
    for i in 0..6_u8 {
        let (runtime, token, tenant, market) =
            (runtime.clone(), token.clone(), shop.tenant, shop.cz);
        set.spawn(async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let c = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
                .await
                .unwrap();
            let out = reviews::submit(&mut tx, &c, &input(&token, 5), Some(&[i; 32])).await;
            tx.commit().await.unwrap();
            out
        });
    }
    let results = set.join_all().await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let n: i64 = run(&runtime, shop.tenant, async |tx| {
        Ok(sqlx::query_scalar("SELECT count(*) FROM reviews")
            .fetch_one(&mut **tx)
            .await?)
    })
    .await
    .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_ip_cap_holds_under_concurrent_submissions(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 8).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-rate-race").await;
    let ip = [7_u8; 32];
    run(&runtime, shop.tenant, async |tx| {
        for _ in 1..reviews::MAX_SUBMISSIONS_PER_IP_HOUR {
            sqlx::query(
                "INSERT INTO customer_auth_attempts (tenant_id, kind, email, ip_hash)
                 VALUES ($1, 'review', '', $2)",
            )
            .bind(tx.tenant_id())
            .bind(&ip[..])
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
        let token = issue(&runtime, &shop, order).await.unwrap().remove(0).token;
        let (runtime, tenant, market) = (runtime.clone(), shop.tenant, shop.cz);
        set.spawn(async move {
            let mut tx = tenant_tx(&runtime, tenant).await.unwrap();
            let c = storefront::context(&mut tx, &PublicUrls::default(), market, None, Utc::now())
                .await
                .unwrap();
            let out = reviews::submit(&mut tx, &c, &input(&token, 5), Some(&ip)).await;
            tx.commit().await.unwrap();
            out
        });
    }
    let results = set.join_all().await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, Error::TooManyRequests { .. }))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn submissions_are_rate_limited_per_ip(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-rate").await;
    let ip = [9_u8; 32];
    run(&runtime, shop.tenant, async |tx| {
        for _ in 0..reviews::MAX_SUBMISSIONS_PER_IP_HOUR {
            sqlx::query(
                "INSERT INTO customer_auth_attempts (tenant_id, kind, email, ip_hash)
                 VALUES ($1, 'review', '', $2)",
            )
            .bind(tx.tenant_id())
            .bind(&ip[..])
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let token = issue(&runtime, &shop, order).await.unwrap().remove(0).token;
    assert!(matches!(
        submit(&runtime, &shop, &input(&token, 5), &ip).await,
        Err(Error::TooManyRequests {
            code: "too_many_reviews"
        })
    ));
    // Another IP is fine, and the refused attempt did not burn the token.
    submit(&runtime, &shop, &input(&token, 5), &[8; 32])
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn moderation_drives_what_the_shop_shows(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-moderation").await;
    let mut ids = vec![];
    for rating in [5, 4, 2] {
        let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
        let token = issue(&runtime, &shop, order).await.unwrap().remove(0).token;
        ids.push(
            submit(&runtime, &shop, &input(&token, rating), &[3; 32])
                .await
                .unwrap(),
        );
    }
    let shown = async || {
        run(&runtime, shop.tenant, async |tx| {
            let c = ctx(tx, &shop).await;
            reviews::for_product(tx, &c, shop.product).await
        })
        .await
        .unwrap()
    };
    let none = shown().await;
    assert!(
        none.summary.is_none() && none.items.is_empty(),
        "pending is invisible"
    );
    assert!(reviews::json_ld(&none).is_none());

    let set = async |id: Uuid, to: Status| {
        run(&runtime, shop.tenant, async |tx| {
            reviews::set_status(tx, "staff-1", id, to).await
        })
        .await
    };
    set(ids[0], Status::Published).await.unwrap();
    set(ids[1], Status::Published).await.unwrap();
    set(ids[2], Status::Rejected).await.unwrap();
    assert_eq!(
        set(ids[2], Status::Hidden).await.unwrap_err().code(),
        "invalid_transition"
    );
    let r = run(&runtime, shop.tenant, async |tx| {
        reviews::set_reply(tx, "staff-1", ids[1], Some("  Děkujeme!  ")).await
    })
    .await
    .unwrap();
    assert_eq!(r.reply.as_deref(), Some("Děkujeme!"));

    let s = shown().await;
    let summary = s.summary.clone().unwrap();
    assert_eq!((summary.count, summary.average), (2, 4.5));
    assert_eq!(summary.histogram, [0, 0, 0, 1, 1]);
    assert_eq!(s.items.len(), 2);
    assert_eq!(
        s.items
            .iter()
            .find(|i| i.id == ids[1])
            .unwrap()
            .reply
            .as_deref(),
        Some("Děkujeme!")
    );
    let (agg, list) = reviews::json_ld(&s).unwrap();
    assert_eq!(agg["ratingValue"], "4.5");
    assert_eq!(list.as_array().unwrap().len(), 2);

    set(ids[0], Status::Hidden).await.unwrap();
    assert_eq!(shown().await.summary.unwrap().count, 1);

    // Visible changes publish the purge event; the queue lists by status.
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE type = 'review.changed'
           AND tenant_id = $1 AND payload ->> 'product_id' = $2",
    )
    .bind(shop.tenant)
    .bind(shop.product.to_string())
    .fetch_one(&db)
    .await
    .unwrap();
    let (pending, audit): (usize, i64) = run(&runtime, shop.tenant, async |tx| {
        let audit = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity = 'review'")
            .fetch_one(&mut **tx)
            .await?;
        let page = reviews::list(tx, Some(Status::Hidden), None, None, 50).await?;
        Ok((page.items.len(), audit))
    })
    .await
    .unwrap();
    assert_eq!(
        events, 4,
        "publish, publish, reply on a published review, hide"
    );
    assert_eq!(pending, 1);
    assert_eq!(audit, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reviews_and_tokens_are_tenant_isolated(db: PgPool) {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let a = testkit::storefront::shop(&runtime, "wp16-a").await;
    let b = testkit::storefront::shop(&runtime, "wp16-b").await;
    let order = raw_order(&runtime, &a, a.cz, "CZK", 12_900, 1, "delivered").await;
    let tokens = issue(&runtime, &a, order).await.unwrap();

    // B cannot use A's token, see A's rows, nor issue for A's order.
    assert!(matches!(
        submit(&runtime, &b, &input(&tokens[0].token, 5), &[1; 32]).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        issue(&runtime, &b, order).await,
        Err(Error::NotFound)
    ));
    let id = submit(&runtime, &a, &input(&tokens[0].token, 5), &[1; 32])
        .await
        .unwrap();
    let (seen, tok, get, moderate) = {
        let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
        let seen: i64 = sqlx::query_scalar("SELECT count(*) FROM reviews")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let tok: i64 = sqlx::query_scalar("SELECT count(*) FROM review_tokens")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let get = reviews::get(&mut tx, id).await;
        let moderate = reviews::set_status(&mut tx, "x", id, Status::Published).await;
        (seen, tok, get, moderate)
    };
    assert_eq!((seen, tok), (0, 0));
    assert!(matches!(get, Err(Error::NotFound)));
    assert!(matches!(moderate, Err(Error::NotFound)));

    // Writing a row for another tenant is refused by the policy.
    let mut tx = tenant_tx(&runtime, b.tenant).await.unwrap();
    let forged = sqlx::query(
        "INSERT INTO reviews (tenant_id, product_id, customer_name, rating, body, verified, locale)
         VALUES ($1, $2, 'x', 5, 'x', false, 'cs')",
    )
    .bind(a.tenant)
    .bind(a.product)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn product_page_carries_reviews_json_ld_and_the_disclosure(db: PgPool) {
    use commerce::content::LegalType;
    use commerce::content::legal::{self, CheckCode, InstallInput};

    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "wp16-page").await;
    let order = raw_order(&runtime, &shop, shop.cz, "CZK", 12_900, 1, "delivered").await;
    let token = issue(&runtime, &shop, order).await.unwrap().remove(0).token;
    let mut i = input(&token, 4);
    i.body = "</script><script>alert(1)</script>".into();
    let id = submit(&runtime, &shop, &i, &[1; 32]).await.unwrap();

    let page = async || {
        run(&runtime, shop.tenant, async |tx| {
            let c = ctx(tx, &shop).await;
            storefront::product::product_page(tx, &c, &shop.slug).await
        })
        .await
        .unwrap()
        .unwrap()
    };
    let p = page().await;
    assert!(p.reviews.summary.is_none());
    assert!(
        p.seo.json_ld[0].get("aggregateRating").is_none(),
        "pending reviews stay out"
    );

    run(&runtime, shop.tenant, async |tx| {
        reviews::set_status(tx, "staff", id, Status::Published).await
    })
    .await
    .unwrap();
    let missing = async || {
        run(&runtime, shop.tenant, async |tx| {
            legal::go_live(tx, Utc::now()).await
        })
        .await
        .unwrap()
        .checks
        .into_iter()
        .find(|c| c.code == CheckCode::LegalPages)
        .unwrap()
        .missing
    };
    assert!(
        missing().await.contains(&"reviews:cs".to_owned()),
        "Omnibus page required"
    );
    run(&runtime, shop.tenant, async |tx| {
        legal::install(
            tx,
            "staff",
            &InstallInput {
                locales: vec!["cs".into(), "sk".into()],
                types: vec![LegalType::Reviews],
            },
        )
        .await?;
        sqlx::query(
            "UPDATE pages SET status = 'published', published_at = now() WHERE legal_type = 'reviews'",
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(!missing().await.iter().any(|m| m.starts_with("reviews:")));
    // A page still carrying the pre-WP16 template ("publishes no reviews") is flagged.
    run(&runtime, shop.tenant, async |tx| {
        sqlx::query(
            r#"UPDATE page_translations
               SET blocks = '[{"type": "rich_text", "html": "<p>E-shop zatiaľ nezverejňuje recenzie zákazníkov.</p>"}]'
               WHERE locale = 'sk' AND page_id IN (SELECT id FROM pages WHERE legal_type = 'reviews')"#,
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        missing()
            .await
            .contains(&"reviews:sk (unfinished)".to_owned())
    );

    let p = page().await;
    let s = p.reviews.summary.unwrap();
    assert_eq!((s.count, s.average), (1, 4.0));
    assert_eq!(
        p.reviews.items[0].body,
        "</script><script>alert(1)</script>"
    );
    assert_eq!(
        p.reviews.verification_url.as_deref(),
        Some("/pages/overovani-recenzi")
    );
    let ld = &p.seo.json_ld[0];
    assert_eq!(ld["aggregateRating"]["reviewCount"], 1);
    assert_eq!(ld["aggregateRating"]["ratingValue"], "4.0");
    assert_eq!(ld["review"][0]["author"]["name"], "Jana N.");
    assert_eq!(ld["review"][0]["reviewRating"]["ratingValue"], 4);
}
