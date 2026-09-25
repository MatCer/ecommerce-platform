//! Storefront API through the router: authorization matrix (A4), page models, cart and the
//! checkout handoff (A1), SEO files, redirects, storefront tokens, artifacts (real Postgres as
//! the runtime role).
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use axum::http::StatusCode;
use chrono::Utc;
use commerce::promotions::coupons::{self, CouponInput};
use commerce::promotions::sales::{self, SaleDiscount, SaleInput, SaleTargets};
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
    _jwks: Jwks,
}

async fn setup(db: PgPool) -> Ctx {
    let jwks = jwks_server(json!([jwk("a", X_A)])).await;
    let runtime = testkit::runtime_pool(&db, 4).await;
    let shop = testkit::storefront::shop(&runtime, "shop").await;
    let other = testkit::storefront::shop(&runtime, "other").await;
    testkit::staff(&runtime, shop.tenant, "boss", "owner").await;
    Ctx {
        s: state(runtime.clone(), &jwks, Duration::from_secs(30)),
        owner: sign(&claims("boss")),
        shop,
        other,
        runtime,
        _jwks: jwks,
    }
}

/// A call as the edge makes it for `shop`'s market `market`.
fn sf<'a>(call: Call<'a>, shop: &Shop, market: Uuid) -> Call<'a> {
    call.header("x-storefront-token", shop.token.clone())
        .header("x-market", market.to_string())
}

impl Ctx {
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (s, b, _) = sf(Call::get(uri), &self.shop, self.shop.cz)
            .send(&self.s)
            .await;
        (s, b)
    }

    async fn new_cart(&self, market: Uuid) -> String {
        let (status, _, res) = sf(
            Call::post("/storefront/v1/cart", json!({})),
            &self.shop,
            market,
        )
        .send(&self.s)
        .await;
        assert_eq!(status, StatusCode::CREATED);
        res.headers()["x-cart-token"].to_str().unwrap().to_owned()
    }

    async fn cart(&self, call: Call<'_>, market: Uuid, token: &str) -> (StatusCode, Value) {
        let (s, b, _) = sf(call, &self.shop, market)
            .header("x-cart-token", token)
            .send(&self.s)
            .await;
        (s, b)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn storefront_calls_need_a_token_whose_tenant_owns_the_market(db: PgPool) {
    let c = setup(db).await;
    let (status, body, _) = Call::get("/storefront/v1/shop").send(&c.s).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("missing_storefront_token"))
    );
    let (status, body, _) = Call::get("/storefront/v1/shop")
        .header("x-storefront-token", format!("sf_{}", "f".repeat(64)))
        .header("x-market", c.shop.cz.to_string())
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("invalid_storefront_token"))
    );
    // Our token with the other tenant's market: the market is invisible under our RLS scope.
    let (status, body, _) = sf(Call::get("/storefront/v1/shop"), &c.shop, c.other.cz)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("market_mismatch"))
    );
    // A forged tenant header that disagrees with the token.
    let (status, body, _) = sf(Call::get("/storefront/v1/shop"), &c.shop, c.shop.cz)
        .header("x-tenant", c.other.tenant.to_string())
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("tenant_mismatch"))
    );
    let (status, _, _) = Call::get("/storefront/v1/shop")
        .header("x-storefront-token", c.shop.token.clone())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "market required");
    // The Admin API never accepts a storefront token.
    let (status, _, _) = Call::get("/admin/v1/markets")
        .token(&c.shop.token)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn page_models_carry_seo_and_cache_hints(db: PgPool) {
    let c = setup(db).await;
    let (status, shop) = c.get("/storefront/v1/shop").await;
    assert_eq!(status, StatusCode::OK, "{shop}");
    assert_eq!(shop["locale"], "cs");
    assert_eq!(shop["currency"], "CZK");
    assert_eq!(shop["messages"]["cart.add"], "Přidat do košíku");
    assert_eq!(shop["menus"]["main"][0]["href"], "/c/trika");
    assert_eq!(shop["cache"]["public"], true);
    assert_eq!(shop["markets"].as_array().unwrap().len(), 2);
    assert_eq!(shop["seo"]["canonical"], "http://shop.localhost:8080/");

    let (status, home) = c.get("/storefront/v1/pages/home").await;
    assert_eq!(status, StatusCode::OK, "{home}");
    assert_eq!(home["featured"][0]["slug"], "tee-cs");
    assert_eq!(home["featured"][0]["price"]["formatted"], "129,00\u{a0}Kč");
    assert_eq!(home["featured"][0]["price_varies"], true);

    let (status, page) = c.get("/storefront/v1/pages/product/tee-cs").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["product"]["variants"].as_array().unwrap().len(), 2);
    assert_eq!(page["product"]["variants"][0]["stock"], "in_stock");
    assert_eq!(
        page["product"]["variants"][0]["reference_price"],
        Value::Null
    );
    assert_eq!(
        page["seo"]["canonical"],
        "http://shop.localhost:8080/p/tee-cs"
    );
    assert_eq!(
        page["cache"]["tags"][0],
        format!("product:{}", c.shop.product)
    );
    let types: Vec<&str> = page["seo"]["json_ld"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["@type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["Product", "BreadcrumbList"]);
    assert_eq!(page["seo"]["json_ld"][0]["offers"][0]["price"], "129.00");
    assert_eq!(page["breadcrumbs"][1]["href"], "/c/trika");

    // The same product in the SK market: EUR, Slovak messages, its own canonical host.
    let (status, sk, _) = sf(
        Call::get("/storefront/v1/pages/product/tee-cs"),
        &c.shop,
        c.shop.sk,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sk["product"]["variants"][0]["price"]["formatted"],
        "5,20\u{a0}€"
    );
    assert_eq!(
        sk["seo"]["canonical"],
        "http://shop-sk.localhost:8080/p/tee-cs"
    );

    let (status, _) = c.get("/storefront/v1/pages/product/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Another tenant's product is not reachable with our token.
    let (status, _, _) = sf(
        Call::get("/storefront/v1/pages/product/tee-cs"),
        &c.other,
        c.other.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK, "its own product with the same slug");
}

#[sqlx::test(migrations = "../../migrations")]
async fn sales_show_the_omnibus_reference_only_with_a_claim(db: PgPool) {
    let c = setup(db).await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sales::create(
        &mut tx,
        "test",
        &SaleInput {
            name: "Sleva".into(),
            discount: SaleDiscount::Percent { basis_points: 2000 },
            starts_at: Some(Utc::now() + chrono::Duration::milliseconds(300)),
            ends_at: None,
            targets: SaleTargets {
                all: true,
                ..SaleTargets::default()
            },
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (_, page) = c.get("/storefront/v1/pages/product/tee-cs").await;
    let v = &page["product"]["variants"][0];
    assert_eq!(v["price"]["amount_minor"], 10_320);
    assert_eq!(v["reference_price"]["amount_minor"], 12_900);
    assert_eq!(v["discount_percent"], 20);
    let (_, listing) = c.get("/storefront/v1/pages/category/trika").await;
    assert_eq!(listing["products"][0]["badges"][0], "sale");
}

#[sqlx::test(migrations = "../../migrations")]
async fn category_listing_filters_sorts_and_marks_filtered_urls_noindex(db: PgPool) {
    let c = setup(db).await;
    let (status, page) = c.get("/storefront/v1/pages/category/trika").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1);
    assert_eq!(page["seo"]["robots"], Value::Null);
    let size = page["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "size")
        .unwrap();
    assert_eq!(size["values"][0]["href"], "/c/trika?size=v1");
    let (_, filtered) = c
        .get("/storefront/v1/pages/category/trika?size=v2&sort=price_desc")
        .await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["seo"]["robots"], "noindex,follow");
    assert_eq!(
        filtered["seo"]["canonical"],
        "http://shop.localhost:8080/c/trika"
    );
    let (_, none) = c.get("/storefront/v1/pages/category/trika?size=nope").await;
    assert_eq!(none["total"], 1, "unknown values are ignored");
    let (status, _) = c.get("/storefront/v1/pages/category/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, search) = c.get("/storefront/v1/pages/search?q=product%20tee").await;
    assert_eq!(search["total"], 1);
    assert_eq!(search["seo"]["robots"], "noindex,follow");
    let (_, suggest) = c.get("/storefront/v1/search/suggest?q=produ").await;
    assert_eq!(suggest["products"][0]["slug"], "tee-cs");
}

#[sqlx::test(migrations = "../../migrations")]
async fn carts_are_priced_with_the_markets_vat(db: PgPool) {
    let c = setup(db).await;
    let token = c.new_cart(c.shop.cz).await;
    let (status, cart) = c
        .cart(
            Call::post(
                "/storefront/v1/cart/lines",
                json!({ "variant_id": c.shop.variants[0], "quantity": 2 }),
            ),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cart}");
    assert_eq!(cart["total"]["amount_minor"], 25_800);
    assert_eq!(cart["vat"][0]["rate"], "21");
    assert_eq!(cart["vat"][0]["vat"]["amount_minor"], 4_478); // round(25800 × 21 / 121)
    assert_eq!(cart["item_count"], 2);
    let version = cart["version"].as_i64().unwrap();

    let line = cart["lines"][0]["id"].as_str().unwrap().to_owned();
    let uri = format!("/storefront/v1/cart/lines/{line}");
    let (_, cart) = c
        .cart(
            Call::patch(&uri, json!({ "quantity": 3 })),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(cart["total"]["amount_minor"], 38_700);
    assert!(cart["version"].as_i64().unwrap() > version);
    let (status, body) = c
        .cart(
            Call::patch(&uri, json!({ "quantity": 11 })),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("insufficient_stock"))
    );

    // Coupon: published, 10 %.
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    coupons::create(
        &mut tx,
        "test",
        &CouponInput {
            code: "VITEJTE10".into(),
            discount: commerce::pricing::cart::CouponDiscount::Percent { basis_points: 1000 },
            currency: None,
            min_subtotal_minor: None,
            starts_at: None,
            ends_at: None,
            usage_limit: None,
            per_customer_limit: None,
            published: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (status, body) = c
        .cart(
            Call::post("/storefront/v1/cart/coupons", json!({ "code": "nope" })),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("coupon_not_found"))
    );
    let (_, cart) = c
        .cart(
            Call::post(
                "/storefront/v1/cart/coupons",
                json!({ "code": "vitejte10" }),
            ),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(cart["coupon"]["applied"], true);
    assert_eq!(cart["discount"]["amount_minor"], 3_870);
    assert_eq!(cart["total"]["amount_minor"], 34_830);
    let (_, cart) = c
        .cart(
            Call::delete("/storefront/v1/cart/coupons/VITEJTE10"),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(cart["coupon"], Value::Null);

    // The SK market: EUR prices and Slovak VAT (OSS, destination), 23 %.
    let sk = c.new_cart(c.shop.sk).await;
    let (_, cart) = c
        .cart(
            Call::post(
                "/storefront/v1/cart/lines",
                json!({ "variant_id": c.shop.variants[1] }),
            ),
            c.shop.sk,
            &sk,
        )
        .await;
    assert_eq!(cart["currency"], "EUR");
    assert_eq!(cart["vat"][0]["rate"], "23");
    assert_eq!(cart["ship_to_country"], "SK");
    assert_eq!(cart["total"]["formatted"], "6,00\u{a0}€");
    // A cart is bound to its market.
    let (status, _) = c
        .cart(Call::get("/storefront/v1/cart"), c.shop.cz, &sk)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn cart_capabilities_are_tenant_bound_and_rotated_at_handoff(db: PgPool) {
    let c = setup(db).await;
    let token = c.new_cart(c.shop.cz).await;
    // Another tenant's storefront token cannot use our capability.
    let (status, _, _) = sf(Call::get("/storefront/v1/cart"), &c.other, c.other.cz)
        .header("x-cart-token", token.clone())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = c
        .cart(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("cart_empty"))
    );
    c.cart(
        Call::post(
            "/storefront/v1/cart/lines",
            json!({ "variant_id": c.shop.variants[0] }),
        ),
        c.shop.cz,
        &token,
    )
    .await;
    let (status, body) = c
        .cart(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            c.shop.cz,
            &token,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let handoff = body["token"].as_str().unwrap().to_owned();
    // The shop capability is dead now.
    let (status, _) = c
        .cart(Call::get("/storefront/v1/cart"), c.shop.cz, &token)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Redeeming for another market (or tenant) fails without burning the token.
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": handoff }),
        ),
        &c.shop,
        c.shop.sk,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": handoff }),
        ),
        &c.other,
        c.other.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body, _) = sf(
        Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": handoff }),
        ),
        &c.shop,
        c.shop.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let checkout = body["cart_token"].as_str().unwrap().to_owned();
    assert_ne!(checkout, token);
    // Single use.
    let (status, body, _) = sf(
        Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": handoff }),
        ),
        &c.shop,
        c.shop.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_handoff"))
    );
    // The checkout capability reads the same cart but cannot edit it (until WP10).
    let (status, cart) = c
        .cart(Call::get("/storefront/v1/cart"), c.shop.cz, &checkout)
        .await;
    assert_eq!(
        (status, cart["item_count"].as_i64()),
        (StatusCode::OK, Some(1))
    );
    let (status, _) = c
        .cart(
            Call::post(
                "/storefront/v1/cart/lines",
                json!({ "variant_id": c.shop.variants[0] }),
            ),
            c.shop.cz,
            &checkout,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_handoffs_are_refused(db: PgPool) {
    let c = setup(db).await;
    let token = c.new_cart(c.shop.cz).await;
    c.cart(
        Call::post(
            "/storefront/v1/cart/lines",
            json!({ "variant_id": c.shop.variants[0] }),
        ),
        c.shop.cz,
        &token,
    )
    .await;
    let (_, body) = c
        .cart(
            Call::post("/storefront/v1/cart/handoff", json!({})),
            c.shop.cz,
            &token,
        )
        .await;
    let mut tx = platform::db::tenant_tx(&c.runtime, c.shop.tenant)
        .await
        .unwrap();
    sqlx::query("UPDATE checkout_handoffs SET expires_at = now() - interval '1 second'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (status, _, _) = sf(
        Call::post(
            "/storefront/v1/checkout/handoff",
            json!({ "token": body["token"] }),
        ),
        &c.shop,
        c.shop.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn seo_files_per_market(db: PgPool) {
    let c = setup(db).await;
    let text = async |name: &str, market: Uuid| {
        let uri = format!("/storefront/v1/files/{name}");
        let (status, body, _) = sf(Call::get(&uri), &c.shop, market).send_text(&c.s).await;
        (status, body)
    };
    let (_, robots) = text("robots.txt", c.shop.cz).await;
    assert!(
        robots.contains("Sitemap: http://shop.localhost:8080/sitemap.xml"),
        "{robots}"
    );
    let (_, index) = text("sitemap.xml", c.shop.cz).await;
    assert!(
        index.contains("<loc>http://shop.localhost:8080/sitemap-1.xml</loc>"),
        "{index}"
    );
    let (_, chunk) = text("sitemap-1.xml", c.shop.cz).await;
    assert!(
        chunk.contains("<loc>http://shop.localhost:8080/p/tee-cs</loc>"),
        "{chunk}"
    );
    assert!(chunk.contains("<loc>http://shop.localhost:8080/c/trika</loc>"));
    assert!(chunk.contains("hreflang=\"x-default\""));
    let (status, _) = text("sitemap-2.xml", c.shop.cz).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, llms) = text("llms.txt", c.shop.cz).await;
    assert!(llms.starts_with("# shop"), "{llms}");
    let (status, _) = text("secrets.txt", c.shop.cz).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The SK sitemap lists only what has a Slovak URL (the test product has cs/en only).
    let (_, sk) = text("sitemap-1.xml", c.shop.sk).await;
    assert!(!sk.contains("/p/tee-cs"), "{sk}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn redirects_admin_crud_and_resolution(db: PgPool) {
    let c = setup(db).await;
    let admin = async |call: Call<'_>| {
        let (s, b, _) = call.token(&c.owner).tenant(c.shop.tenant).send(&c.s).await;
        (s, b)
    };
    let (status, r) = admin(Call::post(
        "/admin/v1/redirects",
        json!({ "from_path": "/stary-produkt/", "to_path": "/p/tee-cs" }),
    ))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{r}");
    assert_eq!(r["from_path"], "/stary-produkt");
    let (status, _) = admin(Call::post(
        "/admin/v1/redirects",
        json!({ "from_path": "/x", "to_path": "//evil.example" }),
    ))
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = admin(Call::post(
        "/admin/v1/redirects",
        json!({ "from_path": "/stary-produkt", "to_path": "/p/other" }),
    ))
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, hit) = c
        .get("/storefront/v1/redirects/resolve?path=/stary-produkt?utm=1")
        .await;
    assert_eq!(
        (status, hit["to_path"].as_str(), hit["code"].as_i64()),
        (StatusCode::OK, Some("/p/tee-cs"), Some(301))
    );
    // Not visible to another tenant.
    let (status, _, _) = sf(
        Call::get("/storefront/v1/redirects/resolve?path=/stary-produkt"),
        &c.other,
        c.other.cz,
    )
    .send(&c.s)
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let uri = format!("/admin/v1/redirects/{}", r["id"].as_str().unwrap());
    let (status, _) = admin(Call::delete(&uri)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = c
        .get("/storefront/v1/redirects/resolve?path=/stary-produkt")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn storefront_token_rotation_keeps_a_grace_period(db: PgPool) {
    let c = setup(db).await;
    let (status, body, _) = Call::get("/admin/v1/storefront-token")
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(
        (status, body["token"].as_str()),
        (StatusCode::OK, Some(c.shop.token.as_str()))
    );
    let (status, body, _) = Call::post("/admin/v1/storefront-token/rotate", json!({}))
        .token(&c.owner)
        .tenant(c.shop.tenant)
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::OK);
    let new = body["token"].as_str().unwrap().to_owned();
    assert_ne!(new, c.shop.token);
    // Both work during the grace period.
    for t in [&new, &c.shop.token] {
        let (status, _, _) = Call::get("/storefront/v1/shop")
            .header("x-storefront-token", t.clone())
            .header("x-market", c.shop.cz.to_string())
            .send(&c.s)
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    // Resolve hands the edge the new one.
    let (_, resolved, _) = Call::get("/internal/v1/resolve?host=shop.localhost")
        .token(SERVICE_TOKEN)
        .send(&c.s)
        .await;
    assert_eq!(resolved["storefront_token"], new);
    // After the grace period the old one is dead.
    sqlx::query("UPDATE platform.storefront_tokens SET expires_at = now() WHERE token = $1")
        .bind(&c.shop.token)
        .execute(&c.runtime)
        .await
        .unwrap();
    let (status, _, _) = Call::get("/storefront/v1/shop")
        .header("x-storefront-token", c.shop.token.clone())
        .header("x-market", c.shop.cz.to_string())
        .send(&c.s)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn artifacts_are_published_to_tenants_and_served_to_the_edge(db: PgPool) {
    let c = setup(db).await;
    let id = "0123456789abcdef0123456789abcdef";
    commerce::themes::register_artifact(
        &c.runtime,
        &c.s.storage,
        id,
        commerce::themes::ArtifactKind::Theme,
        Some(&json!({ "color": "#000" })),
        vec![
            ("manifest.json".into(), br#"{"id":"x"}"#.to_vec()),
            ("server/entry.mjs".into(), b"export default {}".to_vec()),
        ],
    )
    .await
    .unwrap();
    let changed = commerce::themes::publish_default(&c.runtime, "test", id)
        .await
        .unwrap();
    assert_eq!(changed.len(), 2);
    let (_, resolved, _) = Call::get("/internal/v1/resolve?host=shop-sk.localhost")
        .token(SERVICE_TOKEN)
        .send(&c.s)
        .await;
    assert_eq!(resolved["theme_artifact"], id);
    assert_eq!(resolved["market_id"], c.shop.sk.to_string());
    assert_eq!(resolved["default_locale"], "sk");

    let uri = format!("/internal/v1/artifacts/{id}/server/entry.mjs");
    let (status, body, _) = Call::get(&uri).token(SERVICE_TOKEN).send_text(&c.s).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "export default {}")
    );
    let (status, _, _) = Call::get(&uri).send_text(&c.s).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for bad in [
        format!("/internal/v1/artifacts/{id}/server/../manifest.json"),
        format!("/internal/v1/artifacts/{id}/etc/passwd"),
        "/internal/v1/artifacts/ffffffffffffffffffffffffffffffff/manifest.json".to_owned(),
    ] {
        let (status, _, _) = Call::get(&bad).token(SERVICE_TOKEN).send_text(&c.s).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bad}");
    }
    // The shop model exposes the active theme's tokens.
    let (_, shop) = c.get("/storefront/v1/shop").await;
    assert_eq!(shop["tokens"]["color"], "#000");
    // Re-publishing the same artifact changes nothing; a new tenant starts on it.
    assert!(
        commerce::themes::publish_default(&c.runtime, "test", id)
            .await
            .unwrap()
            .is_empty()
    );
}
