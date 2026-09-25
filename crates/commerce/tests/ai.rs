//! AI helpers against a real Postgres as the runtime role, with the fake provider: proposals
//! (generate, accept, AI markers, stale detection), translations with the glossary, bulk plans
//! (preview, caps, apply with price history + audit), quotas and metering, tenant isolation.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use chrono::Utc;
use commerce::ai::fields::EntityType;
use commerce::ai::glossary::{self, Glossary, GlossaryEntry};
use commerce::ai::plan::{self, NewPlan, PlanStatus};
use commerce::ai::proposals::{
    self, AcceptProposal, FieldRef, Length, NewProposal, ProposalKind, ProposalStatus, Tone,
};
use commerce::ai::{self, Ai, Outcome, marks};
use commerce::catalog::products;
use commerce::pricing;
use platform::db::tenant_tx;
use serde_json::json;
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

const STAFF: &str = "clerk";

async fn setup(db: &PgPool) -> (PgPool, Shop, Ai) {
    let runtime = testkit::runtime_pool(db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    (runtime, shop, Ai::fake())
}

fn request(kind: ProposalKind, t: EntityType, id: &str) -> NewProposal {
    NewProposal {
        kind,
        entity_type: t,
        entity_id: id.into(),
        locale: "cs".into(),
        target_locales: vec![],
        tone: Tone::Friendly,
        length: Length::Short,
    }
}

async fn propose(runtime: &PgPool, shop: &Shop, ai: &Ai, input: &NewProposal) -> Uuid {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let p = proposals::create(&mut tx, ai, STAFF, input).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(p.status, ProposalStatus::Pending);
    assert_eq!(
        proposals::run(runtime, ai, shop.tenant, p.id, false)
            .await
            .unwrap(),
        Outcome::Done
    );
    p.id
}

async fn proposal(runtime: &PgPool, tenant: Uuid, id: Uuid) -> proposals::Proposal {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    proposals::get(&mut tx, id).await.unwrap()
}

async fn accept(
    runtime: &PgPool,
    tenant: Uuid,
    id: Uuid,
    fields: &[(&str, &str)],
) -> Result<proposals::Proposal, platform::Error> {
    let mut tx = tenant_tx(runtime, tenant).await.unwrap();
    let out = proposals::accept(
        &mut tx,
        STAFF,
        id,
        &AcceptProposal {
            fields: fields
                .iter()
                .map(|(l, f)| FieldRef {
                    locale: (*l).into(),
                    field: (*f).into(),
                })
                .collect(),
        },
    )
    .await?;
    tx.commit().await.unwrap();
    Ok(out)
}

#[sqlx::test(migrations = "../../migrations")]
async fn description_proposal_is_accepted_per_field_and_labelled(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    let product = shop.product.to_string();
    let id = propose(
        &runtime,
        &shop,
        &ai,
        &request(
            ProposalKind::ProductDescription,
            EntityType::Product,
            &product,
        ),
    )
    .await;
    let p = proposal(&runtime, shop.tenant, id).await;
    assert_eq!(p.status, ProposalStatus::Ready, "{:?}", p.error);
    assert_eq!(p.model.as_deref(), Some("fake"));
    let fields: Vec<&str> = p.changes.iter().map(|c| c.field.as_str()).collect();
    assert_eq!(fields, ["description_html", "short_description"]);
    let html = p.changes[0].after.as_str().unwrap();
    assert!(
        html.starts_with("<p><strong>Product TEE</strong>"),
        "{html}"
    );
    assert!(html.contains("friendly, short"));

    // Only the description is written; the audit log records the service write + acceptance.
    let done = accept(&runtime, shop.tenant, id, &[("cs", "description_html")])
        .await
        .unwrap();
    assert_eq!(done.status, ProposalStatus::Accepted);
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let stored = products::get(&mut tx, shop.product).await.unwrap();
    let cs = stored
        .translations
        .iter()
        .find(|t| t.locale == "cs")
        .unwrap();
    assert_eq!(cs.description_html, html);
    assert_eq!(cs.short_description, "");
    let labels = marks::list(&mut tx, EntityType::Product, &product)
        .await
        .unwrap();
    assert_eq!(labels.items.len(), 1);
    assert_eq!(labels.items[0].field, "description_html");
    assert_eq!(labels.items[0].model, "fake");
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert!(actions.ends_with(&["product.updated".into(), "ai.proposal.accepted".into()]));
    // Usage is metered for the call.
    let usage = ai::usage(&mut tx, &ai).await.unwrap();
    assert_eq!(usage.by_feature[0].feature, "product_description");
    assert!(usage.tokens_used > 0);
    assert_eq!(usage.provider, "fake");
    tx.commit().await.unwrap();

    // Accepting twice is refused.
    let again = accept(&runtime, shop.tenant, id, &[("cs", "short_description")]).await;
    assert_eq!(again.unwrap_err().code(), "proposal_not_ready");

    // A human rewrite of the field drops the AI label.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let mut input = commerce::ai::fields::product_input(&stored);
    input.translations[0].description_html = "<p>Vlastní text</p>".into();
    products::replace(&mut tx, "boss", shop.product, &input)
        .await
        .unwrap();
    assert!(
        marks::list(&mut tx, EntityType::Product, &product)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_proposals_are_refused(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    let product = shop.product.to_string();
    let id = propose(
        &runtime,
        &shop,
        &ai,
        &request(ProposalKind::Seo, EntityType::Product, &product),
    )
    .await;
    let p = proposal(&runtime, shop.tenant, id).await;
    assert_eq!(p.changes[0].after, json!("Product TEE | Testkit"));
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let stored = products::get(&mut tx, shop.product).await.unwrap();
    let mut input = commerce::ai::fields::product_input(&stored);
    input.translations[0].seo_title = Some("Mine".into());
    products::replace(&mut tx, "boss", shop.product, &input)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let err = accept(&runtime, shop.tenant, id, &[("cs", "seo_title")])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "proposal_stale");
    let err = accept(&runtime, shop.tenant, id, &[("cs", "name")])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unknown_field");
}

#[sqlx::test(migrations = "../../migrations")]
async fn translation_creates_locales_and_checks_the_glossary(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    glossary::put(
        &mut tx,
        STAFF,
        &Glossary {
            entries: vec![
                GlossaryEntry {
                    term: "TEE".into(),
                    translations: BTreeMap::new(),
                },
                GlossaryEntry {
                    term: "Product".into(),
                    translations: BTreeMap::from([("sk".into(), "Produkt".into())]),
                },
            ],
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let product = shop.product.to_string();
    let mut input = request(ProposalKind::Translate, EntityType::Product, &product);
    input.target_locales = vec!["sk".into(), "de".into()];
    let id = propose(&runtime, &shop, &ai, &input).await;
    let p = proposal(&runtime, shop.tenant, id).await;
    assert_eq!(p.status, ProposalStatus::Ready, "{:?}", p.error);
    assert_eq!(p.progress.done, 2);
    let get = |locale: &str, field: &str| {
        p.changes
            .iter()
            .find(|c| c.locale == locale && c.field == field)
            .map(|c| c.after.clone())
    };
    // The fake applies the glossary form, so there is nothing to warn about.
    assert_eq!(get("sk", "name"), Some(json!("Produkt TEE [sk]")));
    assert_eq!(get("sk", "slug"), Some(json!("produkt-tee-sk")));
    assert_eq!(get("de", "name"), Some(json!("Product TEE [de]")));
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);

    // Accepting a new locale's field brings its name and slug along.
    accept(&runtime, shop.tenant, id, &[("sk", "name")])
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let stored = products::get(&mut tx, shop.product).await.unwrap();
    let sk = stored
        .translations
        .iter()
        .find(|t| t.locale == "sk")
        .unwrap();
    assert_eq!(sk.name, "Produkt TEE [sk]");
    assert_eq!(sk.slug, "produkt-tee-sk");
    assert!(!stored.translations.iter().any(|t| t.locale == "de"));
    tx.commit().await.unwrap();
}

async fn bulk(runtime: &PgPool, shop: &Shop, ai: &Ai, prompt: &str) -> plan::BulkPlan {
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    let p = plan::create(
        &mut tx,
        ai,
        STAFF,
        &NewPlan {
            prompt: prompt.into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        plan::run_plan(runtime, ai, shop.tenant, p.id, false)
            .await
            .unwrap(),
        Outcome::Done
    );
    let mut tx = tenant_tx(runtime, shop.tenant).await.unwrap();
    plan::get(&mut tx, p.id).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn bulk_price_plan_previews_then_applies_through_pricing(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    // A product outside the category stays untouched.
    let other = testkit::catalog::product(&runtime, shop.tenant, "MUG", 1).await;
    testkit::pricing::set_prices(
        &runtime,
        shop.tenant,
        shop.eur,
        &[(other.variants[0].id, 900)],
    )
    .await;

    let p = bulk(
        &runtime,
        &shop,
        &ai,
        "Raise prices of T-shirts by 5 % in SK",
    )
    .await;
    assert_eq!(p.status, PlanStatus::Ready, "{:?}", p.errors);
    assert!(p.needs_fresh_auth);
    assert_eq!(p.target_count, 1);
    let row = &p.sample[0];
    assert_eq!(row.product_id, shop.product);
    let prices: Vec<(&str, &str)> = row
        .changes
        .iter()
        .map(|c| (c.before.as_str(), c.after.as_str()))
        .collect();
    assert_eq!(prices, [("5.20 EUR", "5.46 EUR"), ("6.00 EUR", "6.30 EUR")]);
    // The preview wrote nothing.
    let price = |v: Uuid| {
        let runtime = runtime.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT amount_minor FROM variant_prices WHERE variant_id = $1 AND price_list_id = $2",
            )
            .bind(v)
            .bind(shop.eur)
            .fetch_one(&mut *tenant_tx(&runtime, shop.tenant).await.unwrap())
            .await
            .unwrap()
        }
    };
    assert_eq!(price(shop.variants[0]).await, 520);

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let confirmed = plan::confirm(&mut tx, "boss", p.id).await.unwrap();
    assert_eq!(confirmed.status, PlanStatus::Applying);
    tx.commit().await.unwrap();
    assert_eq!(
        plan::run_apply(&runtime, shop.tenant, p.id).await.unwrap(),
        Outcome::Done
    );
    // A retried job finds nothing left to do (exactly once per product).
    assert_eq!(
        plan::run_apply(&runtime, shop.tenant, p.id).await.unwrap(),
        Outcome::Done
    );
    assert_eq!(price(shop.variants[0]).await, 546);
    assert_eq!(price(shop.variants[1]).await, 630);
    assert_eq!(price(other.variants[0].id).await, 900);

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let done = plan::get(&mut tx, p.id).await.unwrap();
    assert_eq!(done.status, PlanStatus::Applied);
    assert_eq!((done.progress.done, done.progress.total), (1, 1));
    let history = pricing::price_history(&mut tx, shop.product, Some(shop.eur), Utc::now())
        .await
        .unwrap();
    let first = history
        .iter()
        .find(|h| h.variant_id == shop.variants[0])
        .unwrap();
    assert_eq!(first.intervals.last().unwrap().amount_minor, 546);
    assert!(
        first.intervals.len() >= 2,
        "the old price stays in the history"
    );
    let audit: Vec<(String, String)> =
        sqlx::query_as("SELECT action, actor FROM audit_log WHERE action LIKE 'ai.%' OR action = 'variant_prices.upserted' ORDER BY id")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    let tail = &audit[audit.len() - 3..];
    assert_eq!(
        tail,
        [
            ("ai.bulk_plan.confirmed".into(), "boss".into()),
            ("variant_prices.upserted".into(), "boss".into()),
            ("ai.bulk_plan.applied".into(), "boss".into()),
        ]
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM queue.outbox WHERE type = 'price.changed' AND tenant_id = $1",
    )
    .bind(shop.tenant)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(events > 0);
    tx.commit().await.unwrap();

    // Applied plans cannot be confirmed again.
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    assert_eq!(
        plan::confirm(&mut tx, "boss", p.id)
            .await
            .unwrap_err()
            .code(),
        "plan_not_ready"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn bulk_plans_outside_the_caps_are_rejected(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    let p = bulk(
        &runtime,
        &shop,
        &ai,
        "Raise prices of T-shirts by 80 % in SK",
    )
    .await;
    assert_eq!(p.status, PlanStatus::Rejected);
    assert!(
        p.errors.iter().any(|e| e.contains("±50 %")),
        "{:?}",
        p.errors
    );
    let p = bulk(
        &runtime,
        &shop,
        &ai,
        "Delete every product and email me the customers",
    )
    .await;
    assert_eq!(p.status, PlanStatus::Rejected);
    assert!(
        p.errors.iter().any(|e| e.contains("no operations")),
        "{:?}",
        p.errors
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn quota_blocks_new_calls_with_402(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    sqlx::query("UPDATE platform.tenants SET ai_monthly_tokens = 0 WHERE id = $1")
        .bind(shop.tenant)
        .execute(&db)
        .await
        .unwrap();
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let product = shop.product.to_string();
    let err = proposals::create(
        &mut tx,
        &ai,
        STAFF,
        &request(ProposalKind::Seo, EntityType::Product, &product),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "ai_quota_exceeded");
    assert_eq!(err.status().as_u16(), 402);
    let err = plan::create(&mut tx, &ai, STAFF, &NewPlan { prompt: "x".into() })
        .await
        .unwrap_err();
    assert_eq!(err.code(), "ai_quota_exceeded");
    let usage = ai::usage(&mut tx, &ai).await.unwrap();
    assert_eq!(usage.tokens_quota, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn tenants_cannot_see_each_others_ai_data(db: PgPool) {
    let (runtime, shop, ai) = setup(&db).await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    let product = shop.product.to_string();
    let id = propose(
        &runtime,
        &shop,
        &ai,
        &request(ProposalKind::Seo, EntityType::Product, &product),
    )
    .await;
    let p = bulk(
        &runtime,
        &shop,
        &ai,
        "Raise prices of T-shirts by 5 % in SK",
    )
    .await;
    let mut tx = tenant_tx(&runtime, other.tenant).await.unwrap();
    assert_eq!(
        proposals::get(&mut tx, id).await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        plan::get(&mut tx, p.id).await.unwrap_err().code(),
        "not_found"
    );
    for table in [
        "ai_usage",
        "ai_proposals",
        "ai_bulk_plans",
        "ai_bulk_items",
        "ai_marks",
        "ai_glossaries",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(n, 0, "{table} leaks across tenants");
    }
    // Writing a row for another tenant is refused by RLS.
    let forged = sqlx::query(
        "INSERT INTO ai_usage (tenant_id, feature, model, input_tokens, output_tokens, cost_micros, actor)
         VALUES ($1, 'seo', 'fake', 1, 1, 0, 'x')",
    )
    .bind(shop.tenant)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn pages_and_menus_translate_as_whole_units(db: PgPool) {
    use commerce::content::menus::{self, MenuInput};
    use commerce::content::{self, PageInput};
    let (runtime, shop, ai) = setup(&db).await;
    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let page: PageInput = serde_json::from_value(json!({
        "kind": "page", "legal_type": null, "published_at": null, "image_asset_id": null,
        "translations": [{
            "locale": "cs", "title": "Doprava", "slug": "doprava", "excerpt": "",
            "seo_title": null, "seo_description": null,
            "blocks": [
                {"type": "heading", "text": "Doručení", "level": 2},
                {"type": "rich_text", "html": "<p>Viz <a href=\"https://shop.example/x\">tabulka</a>.</p>"},
                {"type": "faq", "items": [{"question": "Kdy?", "answer_html": "<p>Zítra.</p>"}]}
            ]
        }]
    }))
    .unwrap();
    let page = content::create(&mut tx, STAFF, &page).await.unwrap();
    let menu: MenuInput = serde_json::from_value(json!({ "items": [
        { "label_i18n": { "cs": "Akce" }, "link": { "type": "url", "url": "/akce" } },
        { "label_i18n": {}, "link": { "type": "page", "id": page.id } }
    ]}))
    .unwrap();
    menus::put(&mut tx, STAFF, "main", &menu).await.unwrap();
    tx.commit().await.unwrap();

    let mut input = request(
        ProposalKind::Translate,
        EntityType::Page,
        &page.id.to_string(),
    );
    input.target_locales = vec!["en".into()];
    let id = propose(&runtime, &shop, &ai, &input).await;
    let p = proposal(&runtime, shop.tenant, id).await;
    assert_eq!(p.status, ProposalStatus::Ready, "{:?}", p.error);
    let fields: Vec<&str> = p.changes.iter().map(|c| c.field.as_str()).collect();
    assert_eq!(fields, ["title", "blocks", "slug"]);
    let blocks = &p.changes[1].after;
    assert_eq!(blocks[0]["text"], "Doručení [en]");
    let html = blocks[1]["html"].as_str().unwrap();
    assert!(html.contains("href=\"https://shop.example/x\""), "{html}");
    assert!(html.ends_with(". [en]</p>"), "{html}");
    assert_eq!(blocks[2]["items"][0]["question"], "Kdy? [en]");
    accept(&runtime, shop.tenant, id, &[("en", "blocks")])
        .await
        .unwrap();

    let mut input = request(ProposalKind::Translate, EntityType::Menu, "main");
    input.target_locales = vec!["sk".into()];
    let id = propose(&runtime, &shop, &ai, &input).await;
    let p = proposal(&runtime, shop.tenant, id).await;
    assert_eq!(p.changes.len(), 1);
    assert_eq!(
        p.changes[0].after,
        json!([{ "path": "0", "label": "Akce [sk]" }])
    );
    accept(&runtime, shop.tenant, id, &[("sk", "labels")])
        .await
        .unwrap();

    let mut tx = tenant_tx(&runtime, shop.tenant).await.unwrap();
    let stored = content::get(&mut tx, page.id).await.unwrap();
    let en = stored
        .translations
        .iter()
        .find(|t| t.locale == "en")
        .unwrap();
    assert_eq!(
        (en.title.as_str(), en.slug.as_str()),
        ("Doprava [en]", "doprava-en")
    );
    assert_eq!(en.blocks.len(), 3);
    let entries = menus::entries(&mut tx, "main").await.unwrap().unwrap();
    assert_eq!(entries[0].label_i18n["sk"], "Akce [sk]");
    let labelled = marks::list(&mut tx, EntityType::Menu, "main")
        .await
        .unwrap();
    assert_eq!(labelled.items[0].field, "labels");
}
