//! Feed imports end to end against Postgres (runtime role), in-memory storage and a local
//! image/feed server (WP13a, A18, A21, A28).
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::routing::get;
use commerce::feeds::Source;
use commerce::feeds::import::{self, ImportRun, NewImport, RunStatus};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::http::SafeClient;
use platform::storage::Storage;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const HEUREKA: &str = include_str!("../../../fixtures/feeds/heureka-demo.xml");
const JPEG: &[u8] = include_bytes!("../../../fixtures/images/smoke-photo.jpg");

/// Serves the demo image for every `/images/demo/*` path and the feed at `/feed.xml`.
async fn server(feed: String) -> String {
    let app = Router::new()
        .route("/images/demo/{name}", get(|| async { JPEG }))
        .route("/feed.xml", get(move || async move { feed.clone() }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

struct Ctx {
    runtime: PgPool,
    storage: Storage,
    shop: Shop,
    other: Shop,
    fetch: SafeClient,
    feed: String,
    base: String,
}

async fn setup(db: PgPool) -> Ctx {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    let base = server(String::new()).await;
    let feed = HEUREKA.replace("http://mocks:4010", &base);
    let base = server(feed.clone()).await;
    Ctx {
        runtime,
        storage: testkit::memory_storage(),
        shop,
        other,
        fetch: SafeClient::new(["localhost".to_owned()]).unwrap(),
        feed,
        base,
    }
}

impl Ctx {
    async fn upload_run(&self) -> Uuid {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let created = import::create(
            &mut tx,
            &self.storage,
            "boss",
            &NewImport {
                source: Source::Heureka,
                market_id: self.shop.cz,
                url: None,
                upload_size: Some(self.feed.len() as u64),
            },
        )
        .await
        .unwrap();
        assert!(created.upload.is_some());
        tx.commit().await.unwrap();
        let key = Path::from(format!(
            "imports/{}/{}.xml",
            self.shop.tenant, created.run.id
        ));
        self.storage
            .private
            .put(&key, PutPayload::from(self.feed.clone().into_bytes()))
            .await
            .unwrap();
        created.run.id
    }

    async fn step(&self, id: Uuid, step: &str) -> ImportRun {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        match step {
            "analyze" => import::analyze(&mut tx, "boss", id).await.unwrap(),
            _ => import::apply(&mut tx, "boss", id).await.unwrap(),
        };
        tx.commit().await.unwrap();
        import::run_step(
            &self.runtime,
            &self.storage,
            &self.fetch,
            self.shop.tenant,
            id,
            step,
        )
        .await
        .unwrap();
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        import::get(&mut tx, id).await.unwrap()
    }

    async fn count(&self, sql: &'static str) -> i64 {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar(sql).fetch_one(&mut *tx).await.unwrap()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn dry_run_then_apply_is_idempotent(db: PgPool) {
    let c = setup(db).await;
    let id = c.upload_run().await;
    let before = c.count("SELECT count(*) FROM products").await;

    let run = c.step(id, "analyze").await;
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
    let r = run.report.unwrap();
    assert_eq!(r.items, 107);
    assert_eq!(r.new_products, r.products);
    assert!(r.products >= 40 && r.variants >= 100, "{r:?}");
    assert_eq!(r.missing.get("price"), Some(&1));
    assert_eq!(r.missing.get("image"), Some(&1));
    assert!(r.missing.get("ean").is_some());
    assert!(
        r.collisions.iter().any(|c| c.kind == "redirect"),
        "the duplicate old URL is reported: {:?}",
        r.collisions
    );
    assert!(r.new_categories >= 10);
    assert_eq!(r.images, 7);
    // A dry run writes nothing.
    assert_eq!(c.count("SELECT count(*) FROM products").await, before);

    let run = c.step(id, "apply").await;
    assert_eq!(run.status, RunStatus::Applied, "{:?}", run.error);
    let p = run.progress;
    assert_eq!(p.created, r.products, "{:?}", run.report);
    assert_eq!(p.failed, 0, "{:?}", run.report.map(|r| r.problems));
    assert_eq!(p.images_downloaded, 7);
    assert!(
        p.redirects_created >= 90 && p.redirects_skipped >= 1,
        "{p:?}"
    );

    // New products are drafts; the T-shirt group became one product with two options.
    assert_eq!(
        c.count("SELECT count(*) FROM products WHERE status <> 'draft'")
            .await,
        before
    );
    let tee = c
        .count(
            "SELECT count(*) FROM variants v JOIN product_translations t ON t.product_id = v.product_id
             WHERE t.name = 'Tričko Basic'",
        )
        .await;
    assert_eq!(tee, 16);
    assert_eq!(
        c.count(
            "SELECT count(*) FROM product_options o JOIN product_translations t ON t.product_id = o.product_id
             WHERE t.name = 'Tričko Basic'"
        )
        .await,
        2
    );
    // Prices are imported (A18: no reduction claims without history) and without compare_at.
    assert!(
        c.count("SELECT count(*) FROM price_intervals WHERE imported")
            .await
            >= 100,
        "imported intervals"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM variant_prices WHERE compare_at_minor IS NOT NULL")
            .await,
        0
    );
    assert_eq!(
        c.count(
            "SELECT count(*) FROM redirects WHERE from_path = '/produkt/tricko-basic-cerna-s'
               AND to_path = '/p/tricko-basic'"
        )
        .await,
        1
    );
    assert_eq!(
        c.count("SELECT count(*) FROM assets WHERE status = 'processing'")
            .await,
        7,
        "images wait for the re-encoding job"
    );
    let products = c.count("SELECT count(*) FROM products").await;
    let mappings = c.count("SELECT count(*) FROM import_mappings").await;

    // Importing the same feed again updates the same products.
    let again = c.upload_run().await;
    let run = c.step(again, "analyze").await;
    let r = run.report.unwrap();
    assert_eq!((r.new_products, r.new_categories, r.new_images), (0, 0, 0));
    let run = c.step(again, "apply").await;
    assert_eq!(run.progress.updated, r.products);
    assert_eq!(run.progress.images_downloaded, 0);
    assert_eq!(c.count("SELECT count(*) FROM products").await, products);
    assert_eq!(
        c.count("SELECT count(*) FROM import_mappings").await,
        mappings
    );

    // Another tenant sees none of it.
    let mut tx = tenant_tx(&c.runtime, c.other.tenant).await.unwrap();
    assert!(import::get(&mut tx, id).await.is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM import_mappings")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn url_runs_download_through_the_safe_client(db: PgPool) {
    let c = setup(db).await;
    let create = async |url: String| {
        let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
        let created = import::create(
            &mut tx,
            &c.storage,
            "boss",
            &NewImport {
                source: Source::Heureka,
                market_id: c.shop.cz,
                url: Some(url),
                upload_size: None,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        created.run.id
    };
    let ok = create(format!("{}/feed.xml", c.base)).await;
    import::run_step(
        &c.runtime,
        &c.storage,
        &c.fetch,
        c.shop.tenant,
        ok,
        "analyze",
    )
    .await
    .unwrap();
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let run = import::get(&mut tx, ok).await.unwrap();
    drop(tx);
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);

    // Without the dev allowlist, a loopback URL is refused and the run fails (no retry).
    let strict = SafeClient::new(Vec::<String>::new()).unwrap();
    let blocked = create(format!("{}/feed.xml", c.base)).await;
    import::run_step(
        &c.runtime,
        &c.storage,
        &strict,
        c.shop.tenant,
        blocked,
        "analyze",
    )
    .await
    .unwrap();
    let mut tx = tenant_tx(&c.runtime, c.shop.tenant).await.unwrap();
    let run = import::get(&mut tx, blocked).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("not a public"));
    // Applying needs a finished dry run.
    assert!(import::apply(&mut tx, "boss", blocked).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn broken_feeds_fail_with_a_message(db: PgPool) {
    let c = setup(db).await;
    let id = c.upload_run().await;
    let key = Path::from(format!("imports/{}/{id}.xml", c.shop.tenant));
    c.storage
        .private
        .put(&key, PutPayload::from_static(b"<SHOP><SHOPITEM><ITEM_ID>1"))
        .await
        .unwrap();
    let run = c.step(id, "analyze").await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("invalid XML"));
}
