#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use commerce::ai::alt_text::{self, Payload, Target};
use commerce::ai::fields::{EntityType, product_input};
use commerce::ai::marks;
use commerce::ai::settings::{self, AiSettings};
use commerce::ai::{Ai, Outcome};
use commerce::catalog::products::{self, ProductMedia};
use object_store::path::Path;
use object_store::{ObjectStoreExt, PutPayload};
use platform::db::tenant_tx;
use platform::storage::Storage;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

struct World {
    db: PgPool,
    runtime: PgPool,
    storage: Storage,
    shop: Shop,
    ai: Ai,
}

async fn setup(db: PgPool, slug: &str) -> World {
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, slug).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    sqlx::query("UPDATE markets SET locales = '{cs,en}' WHERE code = 'cz'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    World {
        db,
        runtime,
        storage: testkit::memory_storage(),
        shop,
        ai: Ai::fake(),
    }
}

impl World {
    async fn put(&self, review_summaries: bool, alt_texts: bool) -> AiSettings {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
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

    async fn get(&self) -> AiSettings {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        settings::get(&mut tx).await.unwrap()
    }

    async fn asset(&self, age_days: i32) -> Uuid {
        let id = Uuid::now_v7();
        let t = self.shop.tenant;
        let key = format!("media/{t}/{id}/a.webp");
        let variants = json!([
            {"width": 1024, "height": 768, "format": "avif", "key": format!("media/{t}/{id}/a.avif"), "bytes": 4},
            {"width": 1024, "height": 768, "format": "webp", "key": key, "bytes": 4},
        ]);
        let mut tx = tenant_tx(&self.runtime, t).await.unwrap();
        sqlx::query(
            "INSERT INTO assets (id, tenant_id, status, key, variants, created_at)
             VALUES ($1, $2, 'ready', $3, $4, now() - make_interval(days => $5))",
        )
        .bind(id)
        .bind(t)
        .bind(format!("originals/{t}/{id}"))
        .bind(variants)
        .bind(age_days)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        self.storage
            .public
            .put(&Path::from(key), PutPayload::from_static(b"RIFF"))
            .await
            .unwrap();
        id
    }

    async fn media(&self, media: &[(Uuid, &[(&str, &str)])]) {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let mut input = product_input(&products::get(&mut tx, self.shop.product).await.unwrap());
        input.media = media
            .iter()
            .map(|(asset_id, pairs)| ProductMedia {
                asset_id: *asset_id,
                variant_sku: None,
                alt_i18n: alt(pairs),
            })
            .collect();
        products::replace(&mut tx, "clerk", self.shop.product, &input)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn alts(&self) -> Vec<BTreeMap<String, String>> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        products::get(&mut tx, self.shop.product)
            .await
            .unwrap()
            .media
            .into_iter()
            .map(|m| m.alt_i18n)
            .collect()
    }

    async fn run(&self, target: Target, backfill: bool) {
        let backfill = if backfill { self.since().await } else { None };
        self.run_payload(Payload { target, backfill }).await;
    }

    async fn run_payload(&self, payload: Payload) {
        assert_eq!(
            alt_text::run(
                &self.runtime,
                &self.ai,
                &self.storage,
                self.shop.tenant,
                payload,
                false,
            )
            .await
            .unwrap(),
            Outcome::Done
        );
    }

    async fn since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar("SELECT alt_texts_since FROM ai_settings")
            .fetch_optional(&mut *tx)
            .await
            .unwrap()
            .flatten()
    }

    async fn run_product(&self, backfill: bool) {
        self.run(Target::Product(self.shop.product), backfill).await;
    }

    async fn usage(&self) -> i64 {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        sqlx::query_scalar("SELECT count(*) FROM ai_usage WHERE feature = 'alt_text'")
            .fetch_one(&mut *tx)
            .await
            .unwrap()
    }

    async fn marks(&self) -> Vec<(String, String, String)> {
        let mut tx = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        marks::list(&mut tx, EntityType::Product, &self.shop.product.to_string())
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|m| (m.locale, m.field, m.feature))
            .collect()
    }

    async fn updates(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = 'product.updated'",
        )
        .bind(self.shop.tenant)
        .fetch_one(&self.db)
        .await
        .unwrap()
    }
}

fn alt(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
        .collect()
}

fn field(asset: Uuid) -> String {
    format!("alt_{}", asset.simple())
}

const FILLED: &[(&str, &str)] = &[
    ("cs", "[cs] Product TEE"),
    ("en", "[en] Product TEE"),
    ("sk", "[sk] Product TEE"),
];

#[sqlx::test(migrations = "../../migrations")]
async fn the_alt_text_switch_is_off_by_default_and_does_not_purge_pages(db: PgPool) {
    let w = setup(db, "alt-switch").await;
    let off = AiSettings {
        review_summaries: false,
        alt_texts: false,
    };
    assert_eq!(w.get().await, off);
    let on = AiSettings {
        review_summaries: false,
        alt_texts: true,
    };
    assert_eq!(w.put(false, true).await, on);
    assert_eq!(w.get().await, on);
    w.put(false, false).await;
    assert_eq!(w.get().await, off);
    let purges: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE tenant_id = $1 AND type = 'ai_settings.changed'",
    )
    .bind(w.shop.tenant)
    .fetch_one(&w.db)
    .await
    .unwrap();
    assert_eq!(purges, 0, "the storefront shows no alt switch");
}

#[sqlx::test(migrations = "../../migrations")]
async fn new_images_get_the_missing_languages_once(db: PgPool) {
    let w = setup(db, "alt-new").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Modré tričko zepředu")])]).await;
    let before = w.updates().await;

    w.run_product(false).await;
    assert_eq!(
        w.alts().await,
        vec![alt(&[
            ("cs", "Modré tričko zepředu"),
            ("en", "[en] Product TEE, from cs: Modré tričko zepředu"),
            ("sk", "[sk] Product TEE, from cs: Modré tričko zepředu"),
        ])]
    );
    assert_eq!(w.usage().await, 1, "one call for both languages");
    assert_eq!(
        w.marks().await,
        vec![
            ("en".into(), field(a), "alt_text".into()),
            ("sk".into(), field(a), "alt_text".into()),
        ]
    );
    assert_eq!(w.updates().await, before + 1);

    w.run_product(false).await;
    assert_eq!(w.usage().await, 1, "nothing left to write");
    assert_eq!(w.updates().await, before + 1);

    w.media(&[(
        a,
        &[
            ("cs", "Modré tričko zepředu"),
            ("en", "Blue T-shirt, front"),
            ("sk", "[sk] Product TEE, from cs: Modré tričko zepředu"),
        ],
    )])
    .await;
    assert_eq!(
        w.marks().await,
        vec![("sk".into(), field(a), "alt_text".into())],
        "the edited text lost its label"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn images_uploaded_before_the_switch_wait_for_a_backfill(db: PgPool) {
    let w = setup(db, "alt-old").await;
    let old = w.asset(1).await;
    w.put(false, true).await;
    w.put(true, true).await;
    let new = w.asset(0).await;
    w.media(&[(old, &[]), (new, &[])]).await;

    w.run_product(false).await;
    assert_eq!(
        w.alts().await,
        vec![BTreeMap::new(), alt(FILLED)],
        "turning it on again keeps the original start"
    );

    w.run_product(true).await;
    assert_eq!(w.alts().await, vec![alt(FILLED), alt(FILLED)]);
    assert_eq!(w.usage().await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn switched_off_jobs_write_nothing_and_keep_what_was_written(db: PgPool) {
    let w = setup(db, "alt-off").await;
    let a = w.asset(0).await;
    w.media(&[(a, &[])]).await;
    w.run_product(true).await;
    assert_eq!(w.alts().await, vec![BTreeMap::new()], "never turned on");

    w.put(false, true).await;
    let b = w.asset(0).await;
    w.media(&[(a, &[]), (b, &[])]).await;
    w.run(Target::Asset(b), false).await;
    assert_eq!(
        w.alts().await,
        vec![BTreeMap::new(), alt(FILLED)],
        "asset.ready finds the product, the older image waits"
    );

    w.put(false, false).await;
    let c = w.asset(0).await;
    w.media(&[(a, &[]), (b, FILLED), (c, &[])]).await;
    w.run_product(true).await;
    assert_eq!(
        w.alts().await,
        vec![BTreeMap::new(), alt(FILLED), BTreeMap::new()]
    );
    assert_eq!(w.usage().await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_used_up_quota_stops_the_job_and_keeps_what_it_wrote(db: PgPool) {
    let w = setup(db, "alt-quota").await;
    w.put(false, true).await;
    let (a, b) = (w.asset(0).await, w.asset(0).await);
    w.media(&[(a, &[]), (b, &[])]).await;
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 1 WHERE id = $1")
        .bind(w.shop.tenant)
        .execute(&w.db)
        .await
        .unwrap();

    w.run_product(false).await;
    assert_eq!(w.alts().await, vec![alt(FILLED), BTreeMap::new()]);
    assert_eq!(w.usage().await, 1);
    w.run_product(false).await;
    assert_eq!(w.usage().await, 1, "no call while the quota is used up");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_merchant_save_during_generation_wins(db: PgPool) {
    let w = setup(db, "alt-race").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[])]).await;

    let mut save = tenant_tx(&w.runtime, w.shop.tenant).await.unwrap();
    let mut input = product_input(&products::get(&mut save, w.shop.product).await.unwrap());
    input.media[0].alt_i18n = alt(&[("en", "Blue T-shirt, front")]);
    products::replace(&mut save, "clerk", w.shop.product, &input)
        .await
        .unwrap();
    let job = tokio::spawn({
        let (runtime, ai, storage) = (w.runtime.clone(), w.ai.clone(), w.storage.clone());
        let (tenant, product) = (w.shop.tenant, w.shop.product);
        async move {
            let payload = Payload {
                target: Target::Product(product),
                backfill: None,
            };
            alt_text::run(&runtime, &ai, &storage, tenant, payload, false)
                .await
                .unwrap()
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    save.commit().await.unwrap();
    assert_eq!(job.await.unwrap(), Outcome::Done);

    assert_eq!(
        w.alts().await,
        vec![alt(&[
            ("cs", "[cs] Product TEE"),
            ("en", "Blue T-shirt, front"),
            ("sk", "[sk] Product TEE"),
        ])]
    );
    assert_eq!(
        w.marks().await,
        vec![
            ("cs".into(), field(a), "alt_text".into()),
            ("sk".into(), field(a), "alt_text".into()),
        ]
    );
}

impl World {
    fn spawn_run(&self, target: Target) -> tokio::task::JoinHandle<Outcome> {
        let (runtime, ai, storage, tenant) = (
            self.runtime.clone(),
            self.ai.clone(),
            self.storage.clone(),
            self.shop.tenant,
        );
        tokio::spawn(async move {
            let payload = Payload {
                target,
                backfill: None,
            };
            alt_text::run(&runtime, &ai, &storage, tenant, payload, false)
                .await
                .unwrap()
        })
    }

    async fn locked_save(&self, cs: &str) -> platform::db::TenantTx {
        let mut save = tenant_tx(&self.runtime, self.shop.tenant).await.unwrap();
        let mut input = product_input(&products::get(&mut save, self.shop.product).await.unwrap());
        input.media[0].alt_i18n = alt(&[("cs", cs)]);
        products::replace(&mut save, "clerk", self.shop.product, &input)
            .await
            .unwrap();
        save
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn overlapping_jobs_for_one_image_call_the_model_once(db: PgPool) {
    let w = setup(db, "alt-overlap").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Modré tričko")])]).await;

    let save = w.locked_save("Modré tričko").await;
    let (by_asset, by_product) = (
        w.spawn_run(Target::Asset(a)),
        w.spawn_run(Target::Product(w.shop.product)),
    );
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    save.commit().await.unwrap();
    assert_eq!(by_asset.await.unwrap(), Outcome::Done);
    assert_eq!(by_product.await.unwrap(), Outcome::Done);

    assert_eq!(w.usage().await, 1);
    assert_eq!(
        w.alts().await,
        vec![alt(&[
            ("cs", "Modré tričko"),
            ("en", "[en] Product TEE, from cs: Modré tričko"),
            ("sk", "[sk] Product TEE, from cs: Modré tričko"),
        ])]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_translation_of_an_alt_text_changed_meanwhile_is_dropped(db: PgPool) {
    let w = setup(db, "alt-stale").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Červené tričko")])]).await;

    let save = w.locked_save("Modré tričko").await;
    let job = w.spawn_run(Target::Product(w.shop.product));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    save.commit().await.unwrap();
    assert_eq!(job.await.unwrap(), Outcome::Done);

    assert_eq!(w.alts().await, vec![alt(&[("cs", "Modré tričko")])]);
    assert_eq!(w.marks().await, vec![]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn switching_off_during_generation_writes_nothing(db: PgPool) {
    let w = setup(db, "alt-off-mid").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Modré tričko")])]).await;

    let save = w.locked_save("Modré tričko").await;
    let job = w.spawn_run(Target::Product(w.shop.product));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    w.put(false, false).await;
    save.commit().await.unwrap();
    assert_eq!(job.await.unwrap(), Outcome::Done);

    assert_eq!(w.alts().await, vec![alt(&[("cs", "Modré tričko")])]);
    assert_eq!(w.marks().await, vec![]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn images_get_at_most_as_many_languages_as_the_catalog_stores(db: PgPool) {
    let w = setup(db, "alt-many").await;
    let mut tx = tenant_tx(&w.runtime, w.shop.tenant).await.unwrap();
    sqlx::query(
        "UPDATE markets SET locales = '{cs,en,de,fr,it,es,pl,hu,ro,bg,hr,sl,et,lv,lt,fi,sv,da,nl,pt,el,ga}'
         WHERE code = 'cz'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Modré tričko")])]).await;

    w.run_product(false).await;

    assert_eq!(w.usage().await, 1);
    let alts = w.alts().await;
    assert_eq!(alts[0].len(), 20);
    assert_eq!(alts[0]["cs"], "Modré tričko");
    assert_eq!(alts[0]["bg"], "[bg] Product TEE, from cs: Modré tričko");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_product_renamed_during_generation_gets_no_alt_texts_of_the_old_name(db: PgPool) {
    let w = setup(db, "alt-rename").await;
    w.put(false, true).await;
    let a = w.asset(0).await;
    w.media(&[(a, &[("cs", "Modré tričko")])]).await;

    let mut save = tenant_tx(&w.runtime, w.shop.tenant).await.unwrap();
    let mut input = product_input(&products::get(&mut save, w.shop.product).await.unwrap());
    for t in &mut input.translations {
        t.name = "Product POLO".into();
    }
    products::replace(&mut save, "clerk", w.shop.product, &input)
        .await
        .unwrap();
    let job = w.spawn_run(Target::Product(w.shop.product));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    save.commit().await.unwrap();
    assert_eq!(job.await.unwrap(), Outcome::Done);

    assert_eq!(w.alts().await, vec![alt(&[("cs", "Modré tričko")])]);
    assert_eq!(w.marks().await, vec![]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_backfill_queued_before_switching_off_and_on_again_does_nothing(db: PgPool) {
    let w = setup(db, "alt-period").await;
    let old = w.asset(1).await;
    w.put(false, true).await;
    w.media(&[(old, &[])]).await;
    let mut tx = tenant_tx(&w.runtime, w.shop.tenant).await.unwrap();
    alt_text::backfill(&mut tx, &w.ai).await.unwrap();
    tx.commit().await.unwrap();
    let queued: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM queue.jobs WHERE kind = 'ai.alt_text'")
            .fetch_one(&w.db)
            .await
            .unwrap();

    w.put(false, false).await;
    w.put(false, true).await;
    w.run_payload(serde_json::from_value(queued).unwrap()).await;

    assert_eq!(w.alts().await, vec![BTreeMap::new()]);
    assert_eq!(w.usage().await, 0);
}
