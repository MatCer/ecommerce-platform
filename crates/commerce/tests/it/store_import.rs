//! Store imports (SHOP-43) end to end against Postgres (runtime role), in-memory storage and
//! the fixture shop served on a local port.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use commerce::feeds::Source;
use commerce::feeds::import::{self, ApplyBrand, ImportRun, NewImport, RunStatus};
use commerce::media;
use platform::Error;
use platform::db::tenant_tx;
use platform::http::SafeClient;
use platform::storage::Storage;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const PAGES: &[&str] = &[
    "tricko-basic",
    "mikina-crew",
    "taska-platena",
    "rozbity-zaznam",
    "ostry-popis",
    "o-nas",
];

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../fixtures/store/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Small enough that `media::process` re-encodes it quickly.
fn jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(32, 32, |x, y| image::Rgb([x as u8 * 8, y as u8 * 8, 90]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
    buf.into_inner()
}

/// A PNG whose header declares 100 000 x 100 000 pixels.
fn huge_png() -> Vec<u8> {
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend(100_000u32.to_be_bytes());
    ihdr.extend(100_000u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(13u32.to_be_bytes());
    png.extend(&ihdr);
    png.extend(crc32fast::hash(&ihdr).to_be_bytes());
    png
}

fn product_page(sku: &str, image: &str) -> String {
    format!(
        r#"<html><head><script type="application/ld+json">{{"@type":"Product","name":"Produkt {sku}","sku":"{sku}","image":"{image}","offers":{{"price":"10","priceCurrency":"CZK"}}}}</script></head></html>"#
    )
}

#[derive(Clone)]
struct Fixture {
    base: String,
    port: u16,
    products: Arc<Mutex<String>>,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Fixture {
    fn hits(&self, pred: impl Fn(&str) -> bool) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| pred(p))
            .map(|(_, n)| n)
            .sum()
    }

    fn set_products(&self, paths: &[String]) {
        let locs: String = paths
            .iter()
            .map(|p| format!("<url><loc>{}{p}</loc></url>", self.base))
            .collect();
        *self.products.lock().unwrap() = format!(
            r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{locs}</urlset>"#
        );
    }
}

async fn serve(State(f): State<Fixture>, uri: Uri) -> Response {
    let path = uri.path().to_owned();
    *f.hits.lock().unwrap().entry(path.clone()).or_default() += 1;
    let html = |s: String| ([(header::CONTENT_TYPE, "text/html")], s).into_response();
    let file = |n: &str| fixture(n).replace("http://mocks:4010", &f.base);
    let page = path.trim_matches('/');
    match path.as_str() {
        "/" => html(file("index.html")),
        "/sitemap.xml" => file("sitemap.xml").into_response(),
        "/sitemap-pages.xml" => file("sitemap-pages.xml").into_response(),
        "/sitemap-products.xml" => f.products.lock().unwrap().clone().into_response(),
        "/huge.png" => huge_png().into_response(),
        "/secret" => "secret".into_response(),
        "/to-private/" => (
            StatusCode::TEMPORARY_REDIRECT,
            [(
                header::LOCATION,
                format!("http://127.0.0.1:{}/secret", f.port),
            )],
        )
            .into_response(),
        "/big/" => html("a".repeat(6 * 1024 * 1024)),
        "/obri-obrazek/" => html(product_page("SK-HUGE", "/huge.png")),
        "/gzip/" => {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(product_page("SK-GZIP", "/images/demo/x.jpg").as_bytes())
                .unwrap();
            (
                [
                    (header::CONTENT_TYPE, "text/html"),
                    (header::CONTENT_ENCODING, "gzip"),
                ],
                gz.finish().unwrap(),
            )
                .into_response()
        }
        p if p.starts_with("/images/demo/") => jpeg().into_response(),
        p if p.starts_with("/p") && page[1..].parse::<u32>().is_ok() => {
            html(product_page(&format!("SK-{page}"), "/images/demo/x.jpg"))
        }
        _ if PAGES.contains(&page) => html(file(&format!("{page}.html"))),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn shop_server() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://localhost:{port}");
    let f = Fixture {
        products: Arc::new(Mutex::new(
            fixture("sitemap-products.xml").replace("http://mocks:4010", &base),
        )),
        base,
        port,
        hits: Arc::default(),
    };
    let app = Router::new().fallback(serve).with_state(f.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    f
}

struct Ctx {
    runtime: PgPool,
    storage: Storage,
    shop: Shop,
    fetch: SafeClient,
    site: Fixture,
}

async fn setup(db: PgPool) -> Ctx {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    Ctx {
        runtime,
        storage: testkit::memory_storage(),
        shop,
        fetch: SafeClient::new(["localhost".to_owned()]).unwrap(),
        site: shop_server().await,
    }
}

impl Ctx {
    async fn create(&self, url: &str) -> Result<Uuid, Error> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let created = import::create(
            &mut tx,
            &self.storage,
            &self.fetch,
            "boss",
            &NewImport {
                source: Source::Store,
                market_id: self.shop.cz,
                url: Some(url.to_owned()),
                upload_size: None,
                activate: false,
            },
        )
        .await?;
        tx.commit().await.unwrap();
        Ok(created.run.id)
    }

    async fn start(&self) -> Uuid {
        self.create(&format!("{}/", self.site.base)).await.unwrap()
    }

    async fn worker(&self, id: Uuid, step: &str, fetch: &SafeClient) -> ImportRun {
        import::run_step(
            &self.runtime,
            &self.storage,
            fetch,
            self.shop.tenant,
            id,
            step,
        )
        .await
        .unwrap();
        self.get(id).await
    }

    async fn get(&self, id: Uuid) -> ImportRun {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        import::get(&mut tx, id).await.unwrap()
    }

    async fn analyze(&self, id: Uuid) -> ImportRun {
        self.worker(id, "analyze", &self.fetch).await
    }

    async fn apply(&self, id: Uuid) -> ImportRun {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        import::apply(&mut tx, "boss", id).await.unwrap();
        tx.commit().await.unwrap();
        self.worker(id, "apply", &self.fetch).await
    }

    async fn reanalyze(&self, id: Uuid) -> Result<ImportRun, Error> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        import::analyze(&mut tx, "boss", id).await?;
        tx.commit().await.unwrap();
        Ok(self.analyze(id).await)
    }

    async fn brand(&self, id: Uuid, name: bool, logo: bool) -> Result<import::ShopBrand, Error> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let out = import::apply_brand(&mut tx, "boss", id, &ApplyBrand { name, logo }).await?;
        tx.commit().await.unwrap();
        Ok(out)
    }

    async fn count(&self, sql: &str) -> i64 {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_one(&mut *tx)
            .await
            .unwrap()
    }

    async fn text(&self, sql: &str) -> Option<String> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(sql.to_owned()))
            .fetch_optional(&mut *tx)
            .await
            .unwrap()
            .flatten()
    }
}

fn has_problem(run: &ImportRun, item: &str, code: &str) -> bool {
    run.report
        .as_ref()
        .unwrap()
        .problems
        .iter()
        .any(|p| p.item_id == item && p.code == code)
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_store_import_creates_drafts_with_prices_images_and_redirects(db: PgPool) {
    let c = setup(db).await;
    let base = &c.site.base;
    let products = c.count("SELECT count(*) FROM products").await;
    let assets = c.count("SELECT count(*) FROM assets").await;
    let id = c.start().await;
    assert_eq!(c.get(id).await.status, RunStatus::Analyzing);

    let run = c.analyze(id).await;
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
    let r = run.report.clone().unwrap();
    assert_eq!((r.items, r.new_products), (4, 4), "{r:?}");
    let site = r.site.clone().unwrap();
    assert_eq!(site.home, format!("{base}/"));
    assert_eq!(site.name.as_deref(), Some("Stará Kočka"));
    assert!(site.logo_asset_id.is_some());
    let feed = site.feed.unwrap();
    assert_eq!(
        (feed.url.as_str(), feed.source),
        (
            format!("{base}/feeds/heureka-demo.xml").as_str(),
            Source::Heureka
        )
    );
    assert_eq!(
        (site.product_urls, site.pages_fetched, site.pages_failed),
        (5, 5, 0)
    );
    assert!(!site.capped);
    assert!(has_problem(
        &run,
        &format!("{base}/rozbity-zaznam/"),
        "invalid_json_ld"
    ));
    assert!(has_problem(&run, "8594000000037", "currency_mismatch"));
    assert_eq!(c.site.hits(|p| p == "/sitemap-pages.xml"), 0);
    assert_eq!(
        c.count("SELECT count(*) FROM products").await,
        products,
        "a dry run creates no products"
    );

    let run = c.apply(id).await;
    assert_eq!(run.status, RunStatus::Applied, "{:?}", run.error);
    assert_eq!((run.progress.created, run.progress.failed), (4, 0));
    assert!(run.report.as_ref().unwrap().site.is_some());
    assert_eq!(c.count("SELECT count(*) FROM products").await, products + 4);
    assert_eq!(
        c.count("SELECT count(*) FROM products WHERE status <> 'draft'")
            .await,
        products
    );
    for (sku, ean, price) in [
        ("SK-TRICKO-BASIC", Some("8594000000013"), Some(39_900)),
        ("SK-MIKINA-CREW", Some("8594000000020"), Some(129_050)),
        ("SK-CEPICE-OLIVE", None, Some(24_900)),
        ("8594000000037", Some("8594000000037"), None),
    ] {
        assert_eq!(
            c.text(&format!("SELECT ean FROM variants WHERE sku = '{sku}'"))
                .await,
            ean.map(str::to_owned),
            "{sku}"
        );
        let amount = c
            .count(&format!(
                "SELECT coalesce(max(p.amount_minor), -1) FROM variant_prices p
                 JOIN variants v ON v.id = p.variant_id WHERE v.sku = '{sku}'"
            ))
            .await;
        assert_eq!(amount, price.unwrap_or(-1), "{sku}");
    }
    assert_eq!(
        c.count("SELECT count(*) FROM assets").await,
        assets + 5,
        "the logo and the cap share one asset"
    );
    assert_eq!(
        c.count(
            "SELECT count(*) FROM product_media m JOIN variants v ON v.product_id = m.product_id
             WHERE v.sku = 'SK-TRICKO-BASIC'"
        )
        .await,
        2
    );
    assert_eq!(
        c.count(
            "SELECT count(*) FROM redirects
             WHERE from_path = '/tricko-basic' AND to_path = '/p/tricko-basic'"
        )
        .await,
        1
    );
    let description = c
        .text(
            "SELECT t.description_html FROM product_translations t
             JOIN variants v ON v.product_id = t.product_id WHERE v.sku = 'SK-CEPICE-OLIVE'",
        )
        .await
        .unwrap();
    assert!(description.contains("Hřejivá čepice."), "{description}");
    for bad in ["<script", "onclick", "onerror", "javascript:"] {
        assert!(!description.contains(bad), "{bad} in {description}");
    }

    let again = c.start().await;
    let run = c.analyze(again).await;
    assert_eq!(run.report.unwrap().new_products, 0);
    let again = c.apply(again).await;
    assert_eq!(again.progress.updated, 4);
    assert_eq!(c.count("SELECT count(*) FROM products").await, products + 4);
    assert_eq!(c.count("SELECT count(*) FROM assets").await, assets + 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn brand_is_applied_only_on_request(db: PgPool) {
    let c = setup(db).await;
    let name = "SELECT name FROM platform.tenants";
    let old = c.text(name).await;
    let id = c.start().await;
    let run = c.analyze(id).await;
    let logo = run.report.unwrap().site.unwrap().logo_asset_id.unwrap();
    assert_eq!(c.text(name).await, old);
    assert_eq!(
        c.count("SELECT count(*) FROM email_settings WHERE logo_asset_id IS NOT NULL")
            .await,
        0
    );

    let err = c.brand(id, false, false).await.unwrap_err();
    assert_eq!(err.code(), "nothing_to_apply");
    let err = c.brand(id, false, true).await.unwrap_err();
    assert_eq!(err.code(), "invalid_logo", "the logo is still processing");

    let applied = c.brand(id, true, false).await.unwrap();
    assert_eq!(applied.name, "Stará Kočka");
    assert_eq!(applied.logo_asset_id, None);
    assert_eq!(c.text(name).await.as_deref(), Some("Stará Kočka"));

    media::process(&c.runtime, &c.storage, c.shop.tenant, logo)
        .await
        .unwrap();
    let applied = c.brand(id, false, true).await.unwrap();
    assert_eq!(applied.logo_asset_id, Some(logo));
    assert_eq!(
        c.text("SELECT logo_asset_id::text FROM email_settings")
            .await,
        Some(logo.to_string())
    );
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM audit_log WHERE action = 'import.brand_applied'
             AND entity_id = '{id}' AND diff -> 'name' ->> 'new' = 'Stará Kočka'"
        ))
        .await,
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn refused_pages_are_reported_and_the_crawl_goes_on(db: PgPool) {
    let c = setup(db).await;
    let base = c.site.base.clone();
    c.site.set_products(&[
        "/tricko-basic/".into(),
        "/to-private/".into(),
        "/big/".into(),
        "/gzip/".into(),
        "/obri-obrazek/".into(),
    ]);
    let id = c.start().await;
    let run = c.analyze(id).await;
    assert_eq!(run.status, RunStatus::Analyzed, "{:?}", run.error);
    assert!(has_problem(
        &run,
        &format!("{base}/to-private/"),
        "page_failed"
    ));
    assert_eq!(
        c.site.hits(|p| p == "/secret"),
        0,
        "the private target is never hit"
    );
    assert!(has_problem(&run, &format!("{base}/big/"), "page_failed"));
    let site = run.report.as_ref().unwrap().site.clone().unwrap();
    assert_eq!(
        (
            site.pages_fetched,
            site.pages_failed,
            site.pages_without_product
        ),
        (5, 2, 1),
        "the gzip page is read as sent, not inflated"
    );
    assert_eq!(run.report.as_ref().unwrap().items, 2);

    let run = c.apply(id).await;
    assert_eq!(run.status, RunStatus::Applied, "{:?}", run.error);
    assert!(has_problem(&run, "SK-HUGE", "image_failed"));
    assert_eq!(
        c.count("SELECT count(*) FROM variants WHERE sku IN ('SK-HUGE', 'SK-TRICKO-BASIC')")
            .await,
        2,
        "the draft stays without its image"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM variants WHERE sku = 'SK-GZIP'")
            .await,
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_page_cap_holds(db: PgPool) {
    let c = setup(db).await;
    let paths: Vec<String> = (1..=250).map(|n| format!("/p{n}/")).collect();
    c.site.set_products(&paths);
    let id = c.start().await;
    let run = c.analyze(id).await;
    let site = run.report.unwrap().site.unwrap();
    assert_eq!(
        c.site
            .hits(|p| p.starts_with("/p") && p[2..].trim_end_matches('/').parse::<u32>().is_ok()),
        200
    );
    assert!(site.capped);
    assert_eq!((site.product_urls, site.pages_fetched), (250, 200));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_loopback_shop_is_refused_without_the_allowlist(db: PgPool) {
    let c = setup(db).await;
    let id = c.start().await;
    let strict = SafeClient::new(Vec::<String>::new()).unwrap();
    let run = c.worker(id, "analyze", &strict).await;
    assert_eq!(run.status, RunStatus::Failed);
    assert!(run.error.unwrap().contains("not a public"));
    assert_eq!(c.site.hits(|_| true), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_store_import_at_a_time(db: PgPool) {
    let c = setup(db).await;
    let first = c.start().await;
    let err = c.create(&format!("{}/", c.site.base)).await.unwrap_err();
    assert_eq!(err.code(), "import_busy");

    c.analyze(first).await;
    let second = c.start().await;
    let err = c.reanalyze(first).await.unwrap_err();
    assert_eq!(err.code(), "import_busy", "re-analyzing waits too");
    c.analyze(second).await;
    assert_eq!(
        c.reanalyze(first).await.unwrap().status,
        RunStatus::Analyzed
    );
}
