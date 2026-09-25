//! Content through the router (WP13a): pages and blog posts with blocks, menus in `/shop`,
//! legal entity + templates + go-live checklist (A29), tenant isolation.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::PgPool;
use testkit::storefront::Shop;
use uuid::Uuid;

mod common;
use common::*;

struct Ctx {
    s: api::AppState,
    shop: Shop,
    other: Shop,
    runtime: PgPool,
    owner: String,
    clerk: String,
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    testkit::staff(&runtime, shop.tenant, "clerk", "staff").await;
    testkit::staff(&runtime, other.tenant, "rival", "owner").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        owner: sign(&claims("boss")),
        clerk: sign(&claims("clerk")),
        shop,
        other,
        runtime,
        _jwks: jwks,
    }
}

impl Ctx {
    async fn admin(&self, call: Call<'_>) -> (StatusCode, Value) {
        let (s, b, _) = call
            .token(&self.owner)
            .tenant(self.shop.tenant)
            .send(&self.s)
            .await;
        (s, b)
    }

    async fn sf(&self, uri: &str) -> (StatusCode, Value) {
        let (s, b, _) = Call::get(uri)
            .header("x-storefront-token", self.shop.token.clone())
            .header("x-market", self.shop.cz.to_string())
            .send(&self.s)
            .await;
        (s, b)
    }
}

fn page(kind: &str, slug: &str, status: &str, blocks: Value) -> Value {
    json!({
        "kind": kind,
        "legal_type": null,
        "status": status,
        "published_at": null,
        "image_asset_id": null,
        "translations": [{
            "locale": "cs", "title": format!("Stránka {slug}"), "slug": slug,
            "blocks": blocks, "seo_title": null, "seo_description": null
        }]
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn published_pages_render_sanitized_blocks(db: PgPool) {
    let c = setup(db).await;
    let blocks = json!([
        { "type": "heading", "text": "Doprava" },
        { "type": "rich_text", "html": "<p onclick=\"x()\">Posíláme <b>zdarma</b><script>alert(1)</script></p>" },
        { "type": "button", "label": "Trička", "href": "/c/trika" },
        { "type": "product_grid", "title": "Tipy", "product_ids": [c.shop.product] },
        { "type": "faq", "items": [{ "question": "Kdy?", "answer_html": "<a href=\"javascript:x\">Hned</a>" }] }
    ]);
    let (status, created) = c
        .admin(
            Call::post(
                "/admin/v1/pages",
                page("page", "doprava", "published", blocks),
            )
            .key("p1"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let stored = created["translations"][0]["blocks"].to_string();
    assert!(
        !stored.contains("script") && !stored.contains("onclick") && !stored.contains("javascript")
    );

    let (status, body) = c.sf("/storefront/v1/pages/cms/doprava").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "Stránka doprava");
    let types: Vec<&str> = body["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["heading", "rich_text", "button", "product_grid", "faq"]
    );
    assert_eq!(
        body["blocks"][3]["products"][0]["id"],
        json!(c.shop.product)
    );
    assert_eq!(body["blocks"][2]["href"], "/c/trika");
    assert_eq!(
        body["seo"]["canonical"],
        "http://shop.localhost:8080/pages/doprava"
    );
    assert!(
        body["cache"]["tags"][0]
            .as_str()
            .unwrap()
            .starts_with("page:")
    );

    // Drafts, future posts and the wrong kind are invisible.
    c.admin(
        Call::post(
            "/admin/v1/pages",
            page("page", "koncept", "draft", json!([])),
        )
        .key("p2"),
    )
    .await;
    assert_eq!(
        c.sf("/storefront/v1/pages/cms/koncept").await.0,
        StatusCode::NOT_FOUND
    );
    let mut future = page("blog_post", "brzy", "published", json!([]));
    future["published_at"] = json!("2999-01-01T00:00:00Z");
    c.admin(Call::post("/admin/v1/pages", future).key("p3"))
        .await;
    let mut post = page(
        "blog_post",
        "novinky",
        "published",
        json!([
            { "type": "rich_text", "html": "<p>Nová kolekce triček je tady.</p>" }
        ]),
    );
    post["translations"][0]["title"] = json!("Novinky");
    c.admin(Call::post("/admin/v1/pages", post).key("p4")).await;
    let (_, blog) = c.sf("/storefront/v1/pages/blog").await;
    let posts = blog["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 1, "{blog}");
    assert_eq!(posts[0]["href"], "/blog/novinky");
    assert_eq!(posts[0]["excerpt"], "Nová kolekce triček je tady.");
    assert_eq!(
        c.sf("/storefront/v1/pages/blog/brzy").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.sf("/storefront/v1/pages/cms/novinky").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, post) = c.sf("/storefront/v1/pages/blog/novinky").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(post["breadcrumbs"][1]["href"], "/blog");

    // Validation: bad links, unknown products, slug collisions.
    let bad = page(
        "page",
        "zla",
        "draft",
        json!([{ "type": "button", "label": "x", "href": "javascript:alert(1)" }]),
    );
    assert_eq!(
        c.admin(Call::post("/admin/v1/pages", bad)).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let foreign = page(
        "page",
        "cizi",
        "draft",
        json!([{ "type": "product_grid", "product_ids": [c.other.product] }]),
    );
    let (status, body) = c.admin(Call::post("/admin/v1/pages", foreign)).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_reference"))
    );
    let (status, body) = c
        .admin(Call::post(
            "/admin/v1/pages",
            page("page", "doprava", "draft", json!([])),
        ))
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("slug_taken"))
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn pages_and_menus_are_tenant_scoped(db: PgPool) {
    let c = setup(db).await;
    let (_, created) = c
        .admin(
            Call::post(
                "/admin/v1/pages",
                page("page", "kontakt", "published", json!([])),
            )
            .key("k"),
        )
        .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let rival = sign(&claims("rival"));
    let (status, _, _) = Call::get(&format!("/admin/v1/pages/{id}"))
        .token(&rival)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = Call::delete(&format!("/admin/v1/pages/{id}"))
        .token(&rival)
        .tenant(c.other.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The other shop's storefront does not see it.
    let (status, _, _) = Call::get("/storefront/v1/pages/cms/kontakt")
        .header("x-storefront-token", c.other.token.clone())
        .header("x-market", c.other.cz.to_string())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A menu cannot link to another tenant's page or category.
    let (status, body, _) = Call::put(
        "/admin/v1/menus/main",
        json!({ "items": [{ "link": { "type": "page", "id": id } }] }),
    )
    .token(&rival)
    .tenant(c.other.tenant)
    .send(&c.s)
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_reference"))
    );

    // RLS as the second line: a direct read in the other tenant's scope sees nothing.
    let mut tx = platform::db::tenant_tx(&c.runtime, c.other.tenant)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM pages")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn menus_replace_the_shop_navigation(db: PgPool) {
    let c = setup(db).await;
    let (_, contact) = c
        .admin(
            Call::post(
                "/admin/v1/pages",
                page("page", "kontakt", "published", json!([])),
            )
            .key("k"),
        )
        .await;
    let (status, menu) = c
        .admin(Call::put(
            "/admin/v1/menus/main",
            json!({ "items": [
                { "link": { "type": "category", "id": c.shop.category }, "children": [
                    { "link": { "type": "product", "id": c.shop.product } }
                ]},
                { "label_i18n": { "cs": "Akce" }, "link": { "type": "url", "url": "/search?q=akce" } }
            ]}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{menu}");
    c.admin(Call::put(
        "/admin/v1/menus/footer",
        json!({ "items": [{ "link": { "type": "page", "id": contact["id"] } }] }),
    ))
    .await;
    let (_, shop) = c.sf("/storefront/v1/shop").await;
    let main = &shop["menus"]["main"];
    assert_eq!(main[0]["href"], "/c/trika", "{shop}");
    assert_eq!(
        main[0]["children"][0]["href"],
        format!("/p/{}", c.shop.slug)
    );
    assert_eq!(
        main[1],
        json!({ "label": "Akce", "href": "/search?q=akce", "children": [] })
    );
    assert_eq!(shop["menus"]["footer"][0]["href"], "/pages/kontakt");
    assert_eq!(shop["menus"]["footer"][0]["label"], "Stránka kontakt");

    let (status, _) = c
        .admin(Call::put(
            "/admin/v1/menus/main",
            json!({ "items": [{ "label_i18n": { "cs": "x" }, "link": { "type": "url", "url": "javascript:alert(1)" } }] }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, list) = c.admin(Call::get("/admin/v1/menus")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        c.admin(Call::delete("/admin/v1/menus/footer")).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, shop) = c.sf("/storefront/v1/shop").await;
    assert_eq!(shop["menus"]["footer"], json!([]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn go_live_flags_gaps_until_legal_content_is_complete(db: PgPool) {
    let c = setup(db).await;
    let (_, report) = c.admin(Call::get("/admin/v1/go-live")).await;
    assert_eq!(report["ready"], false);
    let check = |r: &Value, code: &str| {
        r["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["code"] == code)
            .unwrap()
            .clone()
    };
    assert_eq!(check(&report, "legal_entity")["ok"], false);
    assert_eq!(check(&report, "tax_profile")["ok"], true);
    let pages = check(&report, "legal_pages");
    assert!(
        pages["missing"]
            .as_array()
            .unwrap()
            .contains(&json!("terms:cs"))
    );
    assert!(
        pages["missing"]
            .as_array()
            .unwrap()
            .contains(&json!("terms:sk"))
    );
    assert_eq!(check(&report, "gpsr")["missing_count"], 1);

    // Staff may read but not change the legal entity or install templates.
    let (status, _, _) = Call::put("/admin/v1/legal-entity", json!({ "company_name": "X" }))
        .token(&c.clerk)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = c
        .admin(Call::put(
            "/admin/v1/legal-entity",
            json!({ "country": "cz" }),
        ))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, entity) = c
        .admin(Call::put(
            "/admin/v1/legal-entity",
            json!({
                "company_name": "Shop s.r.o.", "company_id": "12345678", "street": "Dlouhá 1",
                "city": "Praha", "postal_code": "110 00", "country": "CZ",
                "email": "info@shop.test", "phone": "+420 123 456 789"
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{entity}");

    let (status, installed) = c
        .admin(Call::post("/admin/v1/legal/templates/install", json!({})))
        .await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(installed["created"].as_array().unwrap().len(), 6);
    assert!(
        installed["notice"]
            .as_str()
            .unwrap()
            .contains("not legal advice")
    );
    let (_, again) = c
        .admin(Call::post("/admin/v1/legal/templates/install", json!({})))
        .await;
    assert_eq!(again["created"], json!([]));
    assert_eq!(again["skipped"].as_array().unwrap().len(), 6);

    // Installed pages are drafts filled from the legal entity: still not live.
    let (_, report) = c.admin(Call::get("/admin/v1/go-live")).await;
    assert_eq!(check(&report, "legal_entity")["ok"], true);
    assert_eq!(check(&report, "legal_pages")["ok"], false);
    for id in installed["created"].as_array().unwrap() {
        let uri = format!("/admin/v1/pages/{}", id.as_str().unwrap());
        let (_, mut p) = c.admin(Call::get(&uri)).await;
        let text = p["translations"].to_string();
        assert!(
            !text.contains("DOPLŇTE: company"),
            "filled from the legal entity: {text}"
        );
        let locales: Vec<&str> = p["translations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["locale"].as_str().unwrap())
            .collect();
        assert_eq!(locales, ["cs", "sk"], "the markets' locales");
        p["status"] = json!("published");
        for k in ["id", "created_at", "updated_at"] {
            p.as_object_mut().unwrap().remove(k);
        }
        let (status, body) = c.admin(Call::put(&uri, p)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE products SET gpsr = '{\"manufacturer\": {\"name\": \"Výrobce\", \"address\": \"Praha\"}}'",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_, report) = c.admin(Call::get("/admin/v1/go-live")).await;
    assert_eq!(report["ready"], true, "{report}");

    // The footer lists the published legal pages; the cookies page is the consent policy.
    let (_, shop) = c.sf("/storefront/v1/shop").await;
    let hrefs: Vec<&str> = shop["legal_pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["href"].as_str().unwrap())
        .collect();
    assert_eq!(
        hrefs,
        [
            "/pages/obchodni-podminky",
            "/pages/ochrana-osobnich-udaju",
            "/pages/odstoupeni-od-smlouvy",
            "/pages/reklamacni-rad",
            "/pages/overovani-recenzi"
        ]
    );
    assert_eq!(shop["consent"]["policy_url"], "/pages/cookies");
    let (status, terms) = c.sf("/storefront/v1/pages/cms/obchodni-podminky").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(terms["title"], "Obchodní podmínky");
}

#[sqlx::test(migrations = "../../migrations")]
async fn idempotent_page_creation(db: PgPool) {
    let c = setup(db).await;
    let body = page("page", "o-nas", "draft", json!([]));
    let (s1, a) = c
        .admin(Call::post("/admin/v1/pages", body.clone()).key("same"))
        .await;
    let (s2, b) = c
        .admin(Call::post("/admin/v1/pages", body).key("same"))
        .await;
    assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(a["id"], b["id"]);
    let (_, list) = c.admin(Call::get("/admin/v1/pages?kind=page")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let id: Uuid = serde_json::from_value(a["id"].clone()).unwrap();
    assert_eq!(
        c.admin(Call::delete(&format!("/admin/v1/pages/{id}")))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}
